//! Escáner multi-hilo del sistema de archivos como fallback robusto.
//! Se activa automáticamente si el usuario no tiene privilegios de Administrador
//! o si la unidad no es formateada en NTFS (ej. FAT32, exFAT o recursos de red).

use std::path::{Path, PathBuf};
use std::time::Instant;
use dragon_core::{FileRecord, ScanMethod, ScanMetrics};

pub struct FallbackScanResult {
    pub root_path: PathBuf,
    pub records: Vec<FileRecord>,
    pub metrics: ScanMetrics,
}

/// Escanea un directorio o unidad usando `jwalk` (paralelismo automático por hilos de CPU).
pub fn scan_path_fallback<P: AsRef<Path>>(root: P) -> Result<FallbackScanResult, String> {
    let root_path = root.as_ref().to_path_buf();
    let start = Instant::now();

    if !root_path.exists() {
        return Err(format!("La ruta especificada no existe: {:?}", root_path));
    }

    let mut records = Vec::with_capacity(50_000);
    let mut file_count = 0;
    let mut dir_count = 0;

    for entry_result in jwalk::WalkDir::new(&root_path)
        .skip_hidden(false)
        .follow_links(false)
    {
        if let Ok(entry) = entry_result {
            let path = entry.path();
            let is_dir = entry.file_type().is_dir();
            let name = entry.file_name().to_string_lossy().into_owned();

            let meta = entry.metadata().ok();
            let size = if is_dir {
                dir_count += 1;
                0
            } else {
                file_count += 1;
                meta.as_ref().map(|m| m.len()).unwrap_or(0)
            };

            let modified_ms = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let mut attributes = 0u32;
            if is_dir {
                attributes |= FileRecord::ATTR_DIRECTORY;
            }

            let deterministic_id = stable_path_id(&path);

            records.push(FileRecord {
                id: deterministic_id,
                parent_id: 0,
                name,
                path: Some(path),
                size,
                attributes,
                modified_ms,
                is_directory: is_dir,
            });
        }
    }

    let duration = start.elapsed();
    log::info!(
        "Fallback scan de {:?} finalizado en {:?}: {} archivos, {} carpetas",
        root_path, duration, file_count, dir_count
    );

    Ok(FallbackScanResult {
        root_path: root_path.clone(),
        records,
        metrics: ScanMetrics {
            volume_or_path: root_path.to_string_lossy().into_owned(),
            total_files: file_count,
            total_directories: dir_count,
            duration_ms: duration.as_millis() as u64,
            scan_method: ScanMethod::FileSystemWalk,
        },
    })
}

/// Genera un ID determinista FNV-1a de 64 bits a partir de la ruta normalizada.
/// Evita colisiones de IDs secuenciales entre múltiples ejecuciones de escaneo.
fn stable_path_id(path: &Path) -> u64 {
    let s = path.to_string_lossy();
    let normalized = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
