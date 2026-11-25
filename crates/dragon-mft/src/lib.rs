//! dragon-mft: Motor unificado de indexación de archivos.
//! Intenta lectura de hardware MFT en NTFS primero para máxima velocidad.
//! Si faltan privilegios de administrador o el sistema de archivos no es NTFS,
//! conmuta transparentemente al motor multi-hilo de alta velocidad (jwalk).

pub mod fallback;
#[cfg(windows)]
pub mod ntfs;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use dragon_core::{FileRecord, ScanMetrics};

pub enum UnifiedScanResult {
    #[cfg(windows)]
    Mft(ntfs::win::MftScanResult),
    Fallback(fallback::FallbackScanResult),
}

impl UnifiedScanResult {
    pub fn metrics(&self) -> &ScanMetrics {
        match self {
            #[cfg(windows)]
            UnifiedScanResult::Mft(m) => &m.metrics,
            UnifiedScanResult::Fallback(f) => &f.metrics,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            #[cfg(windows)]
            UnifiedScanResult::Mft(m) => m.records.len(),
            UnifiedScanResult::Fallback(f) => f.records.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Escáner inteligente con selección automática de estrategia.
pub struct UnifiedScanner;

impl UnifiedScanner {
    /// Escanea una unidad o carpeta específica.
    /// Si es una letra de unidad (ej. "C", "C:", "C:\") intenta MFT en Windows;
    /// si falla o es una ruta normal, ejecuta el escaneo multi-hilo optimizado.
    pub fn scan<P: AsRef<Path>>(target: P) -> Result<UnifiedScanResult, String> {
        let path = target.as_ref();
        let path_str = path.to_string_lossy();

        #[cfg(windows)]
        {
            // Detectar si el objetivo es una letra de unidad raíz (ej. "C", "C:", "C:\")
            let chars: Vec<char> = path_str.chars().collect();
            let is_drive = (chars.len() == 1 && chars[0].is_ascii_alphabetic())
                || (chars.len() == 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':')
                || (chars.len() == 3 && chars[0].is_ascii_alphabetic() && chars[1] == ':' && (chars[2] == '\\' || chars[2] == '/'));

            if is_drive {
                let drive_letter = chars[0].to_ascii_uppercase();
                log::info!("Intentando escaneo MFT directo para unidad {}: ...", drive_letter);
                match ntfs::win::scan_volume_mft(drive_letter) {
                    Ok(mft_result) => return Ok(UnifiedScanResult::Mft(mft_result)),
                    Err(err) => {
                        log::warn!(
                            "No se pudo usar MFT en {}: ({}); activando fallback multi-hilo...",
                            drive_letter, err
                        );
                        let root_drive_path = format!(r"{}:\", drive_letter);
                        let fallback_result = fallback::scan_path_fallback(&root_drive_path)?;
                        return Ok(UnifiedScanResult::Fallback(fallback_result));
                    }
                }
            }
        }

        // Fallback multi-hilo
        let fallback_result = fallback::scan_path_fallback(path)?;
        Ok(UnifiedScanResult::Fallback(fallback_result))
    }
}

/// Índice ultrarrápido en memoria RAM para filtrado de nombres y autocompletado en tiempo real.
pub struct MemoryFileIndex {
    pub entries: Vec<FileRecord>,
    pub name_index: HashMap<String, Vec<usize>>,
}

impl MemoryFileIndex {
    pub fn from_records(entries: Vec<FileRecord>) -> Self {
        let mut name_index: HashMap<String, Vec<usize>> = HashMap::with_capacity(entries.len() / 2);
        for (idx, entry) in entries.iter().enumerate() {
            let lower = entry.name.to_lowercase();
            name_index.entry(lower).or_default().push(idx);
        }
        Self { entries, name_index }
    }

    /// Búsqueda por subcadena en memoria en milisegundos.
    pub fn search_substring(&self, query: &str, limit: usize) -> Vec<&FileRecord> {
        let q_lower = query.to_lowercase();
        let mut results = Vec::with_capacity(limit);

        for entry in &self.entries {
            if entry.name.to_lowercase().contains(&q_lower) {
                results.push(entry);
                if results.len() >= limit {
                    break;
                }
            }
        }
        results
    }
}

/// Descubre las unidades de disco fijas locales del equipo (ej. `C:\`, `D:\`, `E:\`).
pub fn discover_fixed_drives() -> Vec<String> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

        let mut drives = Vec::new();
        let mask = unsafe { GetLogicalDrives() };
        for bit in 0..26u32 {
            if (mask & (1 << bit)) != 0 {
                let letter = (b'A' + bit as u8) as char;
                let root = format!(r"{}:\", letter);
                let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
                let dtype = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
                // DRIVE_FIXED == 3
                if dtype == 3 {
                    drives.push(root);
                }
            }
        }
        drives
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

/// Descubre todas las rutas locales indexables en el equipo omitiendo carpetas del SO.
pub fn discover_local_user_targets(only_user_docs: bool) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let drives = discover_fixed_drives();

    let system_exclusions = [
        "windows",
        "program files",
        "program files (x86)",
        "programdata",
        "$recycle.bin",
        "system volume information",
        "recovery",
        "boot",
        "perflogs",
        "msocache",
        "config.msi",
        "windowsapps",
    ];

    let profile_exclusions = [
        "default",
        "public",
        "all users",
        "default user",
    ];

    let user_subdir_exclusions = [
        "appdata",
        "application data",
        "cookies",
        "local settings",
        "nethood",
        "printhood",
        "recent",
        "sendto",
        "start menu",
        "templates",
    ];

    let base_user_folders = ["Documents", "Desktop", "Downloads", "Pictures", "Videos", "Music"];

    for drive in drives {
        let drive_lower = drive.to_ascii_lowercase();
        if drive_lower.starts_with("c:") {
            // 1. Perfiles de usuario en C:\Users
            let users_dir = Path::new(&drive).join("Users");
            if let Ok(entries) = std::fs::read_dir(&users_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name_lower = entry.file_name().to_string_lossy().to_lowercase();
                    if profile_exclusions.contains(&name_lower.as_str()) || name_lower.starts_with("defaultuser") {
                        continue;
                    }

                    for sub in base_user_folders {
                        let sub_path = path.join(sub);
                        if sub_path.is_dir() {
                            targets.push(sub_path);
                        }
                    }

                    if !only_user_docs {
                        if let Ok(user_entries) = std::fs::read_dir(&path) {
                            for ue in user_entries.flatten() {
                                let up = ue.path();
                                if !up.is_dir() {
                                    continue;
                                }
                                let uname = ue.file_name().to_string_lossy().to_string();
                                let ulower = uname.to_lowercase();
                                if ulower.starts_with('.')
                                    || user_subdir_exclusions.contains(&ulower.as_str())
                                    || base_user_folders.iter().any(|b| b.eq_ignore_ascii_case(&uname))
                                {
                                    continue;
                                }
                                targets.push(up);
                            }
                        }
                    }
                }
            }

            if !only_user_docs {
                // 2. Carpetas no-SO en la raíz de C:\
                if let Ok(root_entries) = std::fs::read_dir(&drive) {
                    for entry in root_entries.flatten() {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let name_lower = entry.file_name().to_string_lossy().to_lowercase();
                        if name_lower == "users"
                            || name_lower.starts_with(".trash")
                            || system_exclusions.contains(&name_lower.as_str())
                        {
                            continue;
                        }
                        targets.push(path);
                    }
                }
            }
        } else {
            // Unidades secundarias (D:, E:, etc.): todo excepto carpetas de sistema/SO
            if let Ok(root_entries) = std::fs::read_dir(&drive) {
                for entry in root_entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name_lower = entry.file_name().to_string_lossy().to_lowercase();
                    if name_lower.starts_with(".trash") || system_exclusions.contains(&name_lower.as_str()) {
                        continue;
                    }
                    targets.push(path);
                }
            }
        }
    }

    targets
}
