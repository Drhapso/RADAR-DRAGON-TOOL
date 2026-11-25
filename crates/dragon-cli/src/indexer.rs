//! dragon-cli::indexer - Motor unificado de indexación incremental, checkpoints y validación de arranque.
//!
//! Funcionalidades:
//! 1. Indexación automática inicial de todas las unidades locales (omitiendo archivos/carpetas del SO).
//! 2. Checkpoints persistentes en `redb` cada N archivos y por carpeta objetivo, permitiendo reanudar
//!    exactamente desde el último punto de control si la herramienta se detiene.
//! 3. Validación completa en cada arranque: detecta archivos nuevos, modificados por peso/fecha,
//!    eliminados y archivos movidos de su ubicación original (conciliación por nombre + peso + fecha).

use anyhow::Result;
use dragon_ai::{DragonEmbedder, VectorStore};
use dragon_core::{classify_path, is_noisy_build_or_cache_path, FileCategory, FileRecord};
use dragon_extract::{detect_format, extract_content, DocumentFormat};
use dragon_index::{DragonIndexEngine, IndexWriter};
use dragon_mft::{discover_fixed_drives, discover_local_user_targets, UnifiedScanResult, UnifiedScanner};
use dragon_storage::{CheckpointState, DragonStorage, IndexedFileState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Cada cuántos cambios (archivos nuevos/modificados/movidos) se guarda un checkpoint firme a disco.
const CHECKPOINT_BATCH_SIZE: usize = 500;
/// Intervalo mínimo (segundos) entre escrituras completas intermedias de `vectors.bin` durante indexación masiva.
const VECTOR_SAVE_INTERVAL_SECS: u64 = 90;
const OWNER_MACHINE_SETTING_KEY: &str = "owner_machine";
/// Versión del extractor de contenido (v2: Excel numérico/fórmulas `<v>`, Word/PPTX/ODF sin partir runs,
/// PDF con respaldo Form XObject/streams y commit Tantivy garantizado al cierre de cada carpeta).
const CURRENT_CONTENT_VERSION: u8 = 2;

/// Rutas resueltas para la base de datos e índices propios del equipo actual.
#[derive(Debug, Clone)]
pub struct MachineStoragePaths {
    pub machine_name: String,
    pub mode_label: String,
    pub data_root: PathBuf,
    pub index_dir: PathBuf,
    pub db_path: PathBuf,
    pub vectors_file: PathBuf,
}

/// Obtiene el nombre identificativo del equipo actual donde se ejecuta el proceso.
pub fn get_machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|s| sanitize_machine_name(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "LOCAL-PC".to_string())
}

fn sanitize_machine_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Devuelve la ruta de datos local dentro del perfil del equipo cliente (`%LOCALAPPDATA%\DragonRadar\<EQUIPO>`).
pub fn per_machine_local_data_root(machine_name: &str) -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        if !local_app_data.trim().is_empty() {
            return PathBuf::from(local_app_data)
                .join("DragonRadar")
                .join(machine_name);
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        if !home.trim().is_empty() {
            return PathBuf::from(home)
                .join(".dragon-radar")
                .join(machine_name);
        }
    }
    std::env::temp_dir().join("DragonRadar").join(machine_name)
}

/// Detecta si el ejecutable o directorio de trabajo proviene de una ruta de red LAN (UNC o unidad mapeada remota).
pub fn is_running_from_network_location() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if is_network_path(&exe) {
            return true;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if is_network_path(&cwd) {
            return true;
        }
    }
    false
}

pub fn is_network_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let trimmed = s.trim_start_matches(r"\\?\");
    if s.starts_with(r"\\") || trimmed.starts_with("UNC\\") {
        return true;
    }

    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let root = format!(r"{}:\", (bytes[0] as char).to_ascii_uppercase());
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            let dtype = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
            // DRIVE_REMOTE == 4
            if dtype == 4 {
                return true;
            }
        }
    }

    false
}

fn is_dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".dragon_write_probe_{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn owner_marker_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("owner_machine.txt")
}

fn write_owner_marker(db_path: &Path, owner: &str) {
    let marker = owner_marker_path(db_path);
    let _ = std::fs::write(&marker, owner);
}

fn existing_db_belongs_to_other_machine(db_path: &Path, current_machine: &str) -> bool {
    if !db_path.exists() {
        return false;
    }
    // Lectura rápida en <1 ms mediante marcador lateral (evita abrir radar.redb de >500 MB dos veces al iniciar)
    let marker = owner_marker_path(db_path);
    if let Ok(saved) = std::fs::read_to_string(&marker) {
        let trimmed = saved.trim();
        if !trimmed.is_empty() {
            return !trimmed.eq_ignore_ascii_case(current_machine);
        }
    }
    if let Ok(storage) = DragonStorage::open(db_path) {
        if let Ok(Some(owner)) = storage.get_setting(OWNER_MACHINE_SETTING_KEY) {
            write_owner_marker(db_path, &owner);
            return !owner.eq_ignore_ascii_case(current_machine);
        }
    }
    false
}

/// Indica si la extensión corresponde a un formato cuyo extractor mejoró en v2 (Office OOXML/ODF y PDF),
/// evitando re-extraer innecesariamente código fuente o texto plano ya indexado correctamente.
fn needs_v2_content_reextract(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "xlsx" | "xlsm" | "docx" | "docm" | "dotx" | "dotm" | "pptx" | "pptm" | "ppsx" | "odt" | "ott" | "ods" | "odp" | "pdf"
    )
}

/// Resuelve las rutas de indexación garantizando aislamiento por equipo cuando se ejecuta en red LAN
/// o desde ubicaciones compartidas/solo lectura, y permitiendo operación portable local cuando se ejecuta en el mismo equipo.
pub fn resolve_machine_storage_paths(force_per_machine_local: bool) -> MachineStoragePaths {
    let machine_name = get_machine_name();

    if let Ok(custom_dir) = std::env::var("DRAGON_DATA_DIR") {
        let custom = custom_dir.trim();
        if !custom.is_empty() {
            let root = PathBuf::from(custom);
            return MachineStoragePaths {
                machine_name,
                mode_label: "CUSTOM_LOCAL".to_string(),
                index_dir: root.join("index"),
                db_path: root.join("data").join("radar.redb"),
                vectors_file: root.join("data").join("vectors.bin"),
                data_root: root,
            };
        }
    }

    let running_over_lan = is_running_from_network_location();
    if force_per_machine_local || running_over_lan {
        let root = per_machine_local_data_root(&machine_name);
        return MachineStoragePaths {
            machine_name,
            mode_label: if running_over_lan {
                "LAN_CLIENT_LOCAL".to_string()
            } else {
                "LOCAL_PROFILE".to_string()
            },
            index_dir: root.join("index"),
            db_path: root.join("data").join("radar.redb"),
            vectors_file: root.join("data").join("vectors.bin"),
            data_root: root,
        };
    }

    // Buscar carpeta portable local ('.', '../..', o junto al ejecutable en disco local)
    let mut candidate_root = PathBuf::from(".");
    if Path::new(".dragon-data").exists() || Path::new(".dragon-index").exists() {
        candidate_root = PathBuf::from(".");
    } else if Path::new("../../Cargo.toml").exists() {
        candidate_root = PathBuf::from("../..");
    } else if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(repo_root) = dir.parent().and_then(|p| p.parent()) {
                if repo_root.join("Cargo.toml").exists() {
                    candidate_root = repo_root.to_path_buf();
                } else {
                    candidate_root = dir.to_path_buf();
                }
            } else {
                candidate_root = dir.to_path_buf();
            }
        }
    }

    let candidate_db = candidate_root.join(".dragon-data").join("radar.redb");
    if existing_db_belongs_to_other_machine(&candidate_db, &machine_name) || !is_dir_writable(&candidate_root) {
        let root = per_machine_local_data_root(&machine_name);
        return MachineStoragePaths {
            machine_name,
            mode_label: "LOCAL_ISOLATED".to_string(),
            index_dir: root.join("index"),
            db_path: root.join("data").join("radar.redb"),
            vectors_file: root.join("data").join("vectors.bin"),
            data_root: root,
        };
    }

    MachineStoragePaths {
        machine_name,
        mode_label: "LOCAL_PORTABLE".to_string(),
        index_dir: candidate_root.join(".dragon-index"),
        db_path: candidate_db,
        vectors_file: candidate_root.join(".dragon-data").join("vectors.bin"),
        data_root: candidate_root,
    }
}

/// Resuelve rutas de almacenamiento dedicadas para el modo LAN `shared_server`,
/// evitando mezclar el índice personal del equipo con el repositorio de carpetas compartidas.
pub fn resolve_shared_server_storage_paths(base: &MachineStoragePaths) -> MachineStoragePaths {
    let use_dot_prefix = base.mode_label == "LOCAL_PORTABLE";
    let (index_dir, data_dir) = if use_dot_prefix {
        (
            base.data_root.join(".dragon-shared-index"),
            base.data_root.join(".dragon-shared-data"),
        )
    } else {
        (
            base.data_root.join("shared-index"),
            base.data_root.join("shared-data"),
        )
    };
    MachineStoragePaths {
        machine_name: base.machine_name.clone(),
        mode_label: format!("{}_SHARED", base.mode_label),
        index_dir,
        db_path: data_dir.join("radar.redb"),
        vectors_file: data_dir.join("vectors.bin"),
        data_root: base.data_root.clone(),
    }
}

/// Estado de cobertura e indexación por unidad fija local (ej. `C:\`, `D:\`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DriveIndexStatus {
    pub drive: String,
    pub targets_total: usize,
    pub targets_completed: usize,
    pub indexed_files: usize,
    pub status: String,
}

/// Estado vivo compartido con el servidor HTTP del HUD para mostrar progreso en tiempo real.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexerTelemetry {
    pub phase: String,
    pub current_target: String,
    pub current_file: String,
    pub target_index: usize,
    pub total_targets: usize,
    pub target_processed: usize,
    pub target_total: usize,
    pub percent: f32,
    pub new_files: usize,
    pub updated_files: usize,
    pub moved_files: usize,
    pub deleted_files: usize,
    pub skipped_files: usize,
    pub last_checkpoint: String,
    pub last_sync_ms: u64,
    pub initial_scan_completed: bool,
    pub drives: Vec<DriveIndexStatus>,
}

/// Recursos compartidos entre el servidor HTTP del HUD y el worker de indexación en segundo plano.
pub struct SharedRadarState {
    pub storage: Arc<DragonStorage>,
    pub index_engine: Arc<DragonIndexEngine>,
    pub embedder: Arc<DragonEmbedder>,
    pub vector_store: Arc<RwLock<VectorStore>>,
    pub telemetry: Arc<RwLock<IndexerTelemetry>>,
    pub vectors_file: PathBuf,
    caches_hydrated: AtomicBool,
    pub shutdown_requested: AtomicBool,
}

impl SharedRadarState {
    #[inline]
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Abre el estado completo cargando vectores en memoria (para comandos CLI síncronos).
    pub fn open(index_dir: &Path, db_path: &Path, vectors_file: &Path) -> Result<Arc<Self>> {
        let state = Self::open_internal(index_dir, db_path, vectors_file, true)?;
        Ok(state)
    }

    /// Arranque rápido para el servidor HTTP del HUD: abre Redb + Tantivy y permite enlazar el puerto
    /// en <2 s, delegando la carga de `vectors.bin` y caché RAM de nombres al hilo de fondo.
    pub fn open_fast(index_dir: &Path, db_path: &Path, vectors_file: &Path) -> Result<Arc<Self>> {
        let state = Self::open_internal(index_dir, db_path, vectors_file, false)?;
        Ok(state)
    }

    fn open_internal(
        index_dir: &Path,
        db_path: &Path,
        vectors_file: &Path,
        load_heavy_caches_now: bool,
    ) -> Result<Arc<Self>> {
        let storage = Arc::new(DragonStorage::open(db_path)?);
        let machine_name = get_machine_name();
        let owner = match storage.get_setting(OWNER_MACHINE_SETTING_KEY).ok().flatten() {
            Some(existing) if !existing.trim().is_empty() => existing,
            _ => {
                let _ = storage.set_setting(OWNER_MACHINE_SETTING_KEY, &machine_name);
                machine_name.clone()
            }
        };
        write_owner_marker(db_path, &owner);

        let index_engine = Arc::new(DragonIndexEngine::open_or_create(index_dir)?);
        let embedder = Arc::new(DragonEmbedder::load_or_detect::<&str>(None));
        let vector_store = if load_heavy_caches_now {
            Arc::new(RwLock::new(
                VectorStore::load_from_file(vectors_file).unwrap_or_default(),
            ))
        } else {
            Arc::new(RwLock::new(VectorStore::default()))
        };
        let cp = storage.load_checkpoint().unwrap_or_default();
        let init_phase = if cp.initial_scan_completed && !cp.in_progress {
            "READY".to_string()
        } else {
            "PENDING".to_string()
        };
        let init_drives = if load_heavy_caches_now {
            compute_drive_index_statuses(&storage, &cp, "", &init_phase)
        } else {
            Vec::new()
        };
        let init_pct = if cp.initial_scan_completed && !cp.in_progress {
            100.0
        } else if !cp.targets.is_empty() {
            ((cp.current_target_idx as f32 / cp.targets.len() as f32) * 100.0).clamp(0.0, 99.9)
        } else {
            0.0
        };

        let telemetry = Arc::new(RwLock::new(IndexerTelemetry {
            phase: init_phase,
            target_index: cp.current_target_idx,
            total_targets: cp.targets.len(),
            percent: init_pct,
            initial_scan_completed: cp.initial_scan_completed,
            last_checkpoint: cp.last_checkpoint_path.unwrap_or_default(),
            last_sync_ms: cp.updated_at_ms,
            drives: init_drives,
            ..Default::default()
        }));

        Ok(Arc::new(Self {
            storage,
            index_engine,
            embedder,
            vector_store,
            telemetry,
            vectors_file: vectors_file.to_path_buf(),
            caches_hydrated: AtomicBool::new(load_heavy_caches_now),
            shutdown_requested: AtomicBool::new(false),
        }))
    }

    /// Hidrata una sola vez en segundo plano la caché RAM de nombres/rutas y `vectors.bin`
    /// una vez que el servidor HTTP ya está escuchando en el puerto.
    pub fn hydrate_background_caches(self: &Arc<Self>) {
        if self
            .caches_hydrated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        // Cargar caché RAM de búsqueda rápida y vectors.bin en segundo plano sin bloquear el arranque HTTP
        if let Ok(cached) = self.storage.warm_name_cache() {
            log::info!("Caché RAM de búsqueda rápida precargada ({} registros)", cached);
        }
        // Pasada ultrarrápida NTFS MFT estilo Everything (~1-3 s si hay privilegios de Administrador):
        // hace visibles al instante archivos nuevos y de raíz en todas las unidades antes de extraer contenido.
        let mft_added = run_fast_mft_filename_pass(self);
        if mft_added > 0 {
            log::info!(
                "[MFT-EVERYTHING] {} archivos nuevos/raíz incorporados instantáneamente a búsqueda por nombre",
                mft_added
            );
        }
        if self.vectors_file.exists() {
            if let Ok(loaded) = VectorStore::load_from_file(&self.vectors_file) {
                let count = loaded.len();
                if let Ok(mut vs) = self.vector_store.write() {
                    *vs = loaded;
                }
                log::info!("Almacén vectorial cargado en segundo plano ({} vectores)", count);
            }
        }
        let cp = self.storage.load_checkpoint().unwrap_or_default();
        let phase_now = self
            .telemetry
            .read()
            .map(|t| t.phase.clone())
            .unwrap_or_else(|_| "READY".to_string());
        let drives = compute_drive_index_statuses(&self.storage, &cp, "", &phase_now);
        update_telemetry(self, |t| {
            if t.drives.is_empty() {
                t.drives = drives;
            }
        });
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncSummary {
    pub new_files: usize,
    pub updated_files: usize,
    pub moved_files: usize,
    pub deleted_files: usize,
    pub skipped_files: usize,
    pub stored_records: usize,
    pub resumed_from_checkpoint: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MoveSignature {
    name_lower: String,
    size: u64,
    modified_ms: u64,
}

struct MissingEntry {
    key: String,
    orig_path: String,
    state: IndexedFileState,
}

/// Pasada ultrarrápida estilo Everything (NTFS MFT + archivos raíz/usuario en < 1-3 s):
/// 1) Si tiene privilegios de Administrador/SYSTEM, lee la tabla MFT de cada unidad fija NTFS.
/// 2) Siempre (con o sin privilegios de Administrador) escanea los archivos sueltos en la raíz
///    de `C:\`, `D:\`, `E:\`, `F:\` y el nivel superior de Escritorio/Descargas/Documentos
///    e incorpora de inmediato los faltantes a Redb + caché RAM (sin marcar `index_state` para
///    que el motor Kensaku extraiga su contenido textual a continuación).
pub fn run_fast_mft_filename_pass(state: &Arc<SharedRadarState>) -> usize {
    #[cfg(windows)]
    {
        if state.is_shutdown_requested() {
            return 0;
        }
        let Ok(existing_states) = state.storage.load_index_states() else {
            return 0;
        };
        let fetch_meta = !existing_states.is_empty();
        let drives = discover_fixed_drives();
        let mut total_added = 0usize;
        let mut batch: Vec<FileRecord> = Vec::with_capacity(1000);
        let mut seen_fast: HashSet<String> = HashSet::new();

        // Paso A: Archivos sueltos en la raíz de todas las unidades fijas (funciona con o sin Admin en <100 ms)
        for drv in &drives {
            if state.is_shutdown_requested() {
                break;
            }
            let Ok(entries) = std::fs::read_dir(drv) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let clean = clean_abs_path(&path);
                let path_str = clean.to_string_lossy().to_string();
                let path_key = normalize_path_key(&path_str);
                if existing_states.contains_key(&path_key)
                    || is_os_excluded_mft_path(&path_key)
                    || !seen_fast.insert(path_key.clone())
                {
                    continue;
                }
                let name = clean
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.is_empty() || name.starts_with('$') || name.starts_with('.') {
                    continue;
                }
                let (size, modified_ms) = read_size_and_mtime(&clean);
                batch.push(FileRecord {
                    id: stable_mft_path_id(&path_key),
                    parent_id: 0,
                    name,
                    path: Some(clean),
                    size,
                    attributes: 0,
                    modified_ms,
                    is_directory: false,
                });
            }
        }
        if !batch.is_empty() {
            if let Ok(n) = state.storage.insert_batch(&batch) {
                total_added += n;
            }
            batch.clear();
        }

        // Paso B: Lectura directa NTFS MFT en todas las unidades (cuando hay privilegios elevados)
        for drv in drives {
            if state.is_shutdown_requested() {
                break;
            }
            let Some(letter) = drv.chars().next().filter(|c| c.is_ascii_alphabetic()) else {
                continue;
            };
            let Ok(mft) = dragon_mft::ntfs::win::scan_volume_mft(letter.to_ascii_uppercase()) else {
                continue;
            };
            for (&id, &(parent_id, ref name, attr)) in &mft.records {
                if state.is_shutdown_requested() {
                    break;
                }
                let is_dir = (attr & FileRecord::ATTR_DIRECTORY) != 0;
                if is_dir || name.starts_with('$') {
                    continue;
                }
                let Some(resolved) = mft.resolve_path(id) else {
                    continue;
                };
                let path_str = resolved.to_string_lossy().to_string();
                let path_key = normalize_path_key(&path_str);
                if existing_states.contains_key(&path_key)
                    || is_os_excluded_mft_path(&path_key)
                    || !seen_fast.insert(path_key.clone())
                {
                    continue;
                }
                let stable_id = stable_mft_path_id(&path_key);
                let (size, modified_ms) = if fetch_meta {
                    read_size_and_mtime(&resolved)
                } else {
                    (0, 0)
                };
                batch.push(FileRecord {
                    id: stable_id,
                    parent_id,
                    name: name.clone(),
                    path: Some(resolved),
                    size,
                    attributes: attr,
                    modified_ms,
                    is_directory: false,
                });
                if batch.len() >= 1000 {
                    if let Ok(n) = state.storage.insert_batch(&batch) {
                        total_added += n;
                    }
                    batch.clear();
                }
            }
        }
        if !batch.is_empty() {
            if let Ok(n) = state.storage.insert_batch(&batch) {
                total_added += n;
            }
        }
        total_added
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        0
    }
}

#[cfg(windows)]
fn is_os_excluded_mft_path(path_lower: &str) -> bool {
    let p = path_lower.replace('/', "\\");
    p.contains(r"\windows\")
        || p.contains(r"\windows.old\")
        || p.contains(r"\program files\")
        || p.contains(r"\program files (x86)\")
        || p.contains(r"\programdata\")
        || p.contains(r"\$recycle.bin\")
        || p.contains(r"\system volume information\")
        || p.contains(r"\recovery\")
        || p.contains(r"\perflogs\")
        || p.contains(r"\msocache\")
        || p.contains(r"\config.msi\")
        || p.contains(r"\windowsapps\")
        || p.contains(r"\onedrivetemp\")
        || p.contains(r"\appdata\")
        || p.contains(r"\.git\objects\")
        || p.ends_with(r"\pagefile.sys")
        || p.ends_with(r"\hiberfil.sys")
        || p.ends_with(r"\swapfile.sys")
        || p.ends_with(r"\desktop.ini")
        || p.ends_with(r"\thumbs.db")
        || p.contains(r"\ntuser.")
        || p.contains(r"\usrclass.")
}

#[cfg(windows)]
fn stable_mft_path_id(normalized_lower: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalized_lower.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Flujo principal al iniciar la aplicación:
/// - Primera vez o sesión interrumpida: ejecuta indexación automática con checkpoints persistentes.
/// - Arranques posteriores: valida el índice completo (nuevos, modificados por peso/fecha, movidos y eliminados).
pub fn run_startup_auto_sync_and_validate(
    state: &Arc<SharedRadarState>,
    force_full: bool,
) -> Result<SyncSummary> {
    run_startup_auto_sync_with_targets(state, force_full, None)
}

/// Variante con carpetas objetivo explícitas (usada en modo LAN `shared_server` para indexar
/// únicamente las carpetas declaradas en `config/site_config.json -> shared_paths`).
pub fn run_startup_auto_sync_with_targets(
    state: &Arc<SharedRadarState>,
    force_full: bool,
    custom_targets: Option<&[PathBuf]>,
) -> Result<SyncSummary> {
    let t0 = Instant::now();
    let mut summary = SyncSummary::default();
    let mut cp = state.storage.load_checkpoint().unwrap_or_default();

    let resolved_custom: Option<Vec<String>> = custom_targets.map(|list| {
        list.iter()
            .map(|p| clean_abs_path(p).to_string_lossy().to_string())
            .collect()
    });

    if let Some(ref targets) = resolved_custom {
        if targets.is_empty() {
            update_telemetry(state, |t| {
                t.phase = "READY".to_string();
                t.total_targets = 0;
                t.target_index = 0;
                t.current_target.clear();
                t.initial_scan_completed = true;
            });
            return Ok(summary);
        }
    }

    let targets_changed = match &resolved_custom {
        Some(targets) => cp.targets != *targets,
        None => false,
    };

    let needs_initial_or_resume = force_full || targets_changed || !cp.initial_scan_completed || cp.in_progress;

    if needs_initial_or_resume {
        let resuming = !force_full && !targets_changed && cp.in_progress && !cp.targets.is_empty();
        summary.resumed_from_checkpoint = resuming;

        if !resuming {
            let discovered: Vec<String> = match resolved_custom {
                Some(ref targets) => targets.clone(),
                None => discover_local_user_targets(false)
                    .into_iter()
                    .map(|p| clean_abs_path(&p).to_string_lossy().to_string())
                    .collect(),
            };

            cp = CheckpointState {
                initial_scan_completed: false,
                in_progress: true,
                targets: discovered,
                current_target_idx: 0,
                last_checkpoint_path: None,
                processed_count: 0,
                updated_at_ms: now_epoch_ms(),
            };
            state.storage.save_checkpoint(&cp)?;
            log::info!(
                "[CHECKPOINT] Iniciando indexación automática inicial de {} ubicaciones...",
                cp.targets.len()
            );
        } else {
            log::info!(
                "[CHECKPOINT] Reanudando indexación desde punto de control {}/{} ({})",
                cp.current_target_idx + 1,
                cp.targets.len(),
                cp.last_checkpoint_path.as_deref().unwrap_or("-")
            );
        }

        let phase_name = if resuming {
            "RESUMING_CHECKPOINT"
        } else {
            "INITIAL_INDEXING"
        };

        // Si se reanuda desde checkpoint, comprobar si apareció alguna unidad/carpeta nueva no listada aún
        if resuming && resolved_custom.is_none() {
            let current_discovered: Vec<String> = discover_local_user_targets(false)
                .into_iter()
                .map(|p| clean_abs_path(&p).to_string_lossy().to_string())
                .collect();
            let existing_set: HashSet<String> = cp.targets.iter().map(|t| normalize_path_key(t)).collect();
            let mut added_any = false;
            for cand in current_discovered {
                if !existing_set.contains(&normalize_path_key(&cand)) {
                    cp.targets.push(cand);
                    added_any = true;
                }
            }
            if added_any {
                cp.updated_at_ms = now_epoch_ms();
                let _ = state.storage.save_checkpoint(&cp);
            }
        }

        let initial_drives = compute_drive_index_statuses(
            &state.storage,
            &cp,
            cp.targets.get(cp.current_target_idx).map(|s| s.as_str()).unwrap_or(""),
            phase_name,
        );
        update_telemetry(state, |t| {
            t.phase = phase_name.to_string();
            t.total_targets = cp.targets.len();
            t.target_index = cp.current_target_idx;
            t.target_processed = 0;
            t.target_total = 0;
            t.current_file.clear();
            t.percent = if cp.targets.is_empty() {
                0.0
            } else {
                ((cp.current_target_idx as f32 / cp.targets.len() as f32) * 100.0).clamp(0.0, 99.9)
            };
            t.initial_scan_completed = false;
            t.drives = initial_drives;
        });

        let mut writer = state.index_engine.create_writer(64)?;
        let total_targets = cp.targets.len();

        while cp.current_target_idx < total_targets {
            let idx = cp.current_target_idx;
            let target = cp.targets[idx].clone();

            update_telemetry(state, |t| {
                t.target_index = idx + 1;
                t.total_targets = total_targets;
                t.current_target = target.clone();
                t.current_file = "Escaneando carpeta...".to_string();
                t.target_processed = 0;
                t.target_total = 0;
                if total_targets > 0 {
                    t.percent = ((idx as f32 / total_targets as f32) * 100.0).clamp(0.0, 99.9);
                }
            });

            if Path::new(&target).exists() {
                match index_target_incremental_internal(
                    state,
                    &mut writer,
                    &target,
                    force_full,
                    false, // Durante el barrido por partes, conciliamos movimientos/borrados al cierre global
                    Some(&mut cp),
                ) {
                    Ok(target_sum) => {
                        summary.new_files += target_sum.new_files;
                        summary.updated_files += target_sum.updated_files;
                        summary.skipped_files += target_sum.skipped_files;
                        summary.stored_records += target_sum.stored_records;
                    }
                    Err(err) => {
                        log::warn!(
                            "[CHECKPOINT] Omitiendo ruta '{}' por error durante escaneo/indexación: {}",
                            target,
                            err
                        );
                    }
                }
            }

            cp.current_target_idx = idx + 1;
            cp.last_checkpoint_path = Some(target);
            cp.updated_at_ms = now_epoch_ms();
            state.storage.save_checkpoint(&cp)?;

            let next_target = cp.targets.get(cp.current_target_idx).map(|s| s.as_str()).unwrap_or("");
            let drives_snap = compute_drive_index_statuses(&state.storage, &cp, next_target, phase_name);
            update_telemetry(state, |t| {
                t.new_files = summary.new_files;
                t.updated_files = summary.updated_files;
                t.skipped_files = summary.skipped_files;
                t.last_checkpoint = cp.last_checkpoint_path.clone().unwrap_or_default();
                t.last_sync_ms = cp.updated_at_ms;
                t.percent = if total_targets > 0 {
                    (((idx + 1) as f32 / total_targets as f32) * 100.0).clamp(0.0, 99.9)
                } else {
                    99.9
                };
                t.drives = drives_snap;
            });
        }

        // Conciliación final de archivos movidos o borrados tras completar las unidades
        match validate_and_reconcile_global(state, &mut writer, &cp.targets) {
            Ok(reconcile_sum) => {
                summary.moved_files += reconcile_sum.moved_files;
                summary.deleted_files += reconcile_sum.deleted_files;
                summary.new_files += reconcile_sum.new_files;
                summary.updated_files += reconcile_sum.updated_files;
            }
            Err(err) => {
                log::warn!("[CHECKPOINT] Conciliación final con advertencia: {}", err);
            }
        }

        state.index_engine.commit_with_retry(&mut writer)?;
        save_vector_store_to_disk(state);

        cp.initial_scan_completed = true;
        cp.in_progress = false;
        cp.current_target_idx = 0;
        cp.last_checkpoint_path = Some("COMPLETADO".to_string());
        cp.updated_at_ms = now_epoch_ms();
        state.storage.save_checkpoint(&cp)?;
    } else {
        // Arranque normal: validación global de índice (nuevos, modificados por peso/fecha, movidos, eliminados)
        let targets: Vec<String> = match resolved_custom {
            Some(targets) => targets,
            None => discover_local_user_targets(false)
                .into_iter()
                .map(|p| clean_abs_path(&p).to_string_lossy().to_string())
                .collect(),
        };

        if cp.targets != targets && !targets.is_empty() {
            cp.targets = targets.clone();
        }
        let val_drives = compute_drive_index_statuses(&state.storage, &cp, "", "VALIDATING_INDEX");
        update_telemetry(state, |t| {
            t.phase = "VALIDATING_INDEX".to_string();
            t.total_targets = targets.len();
            t.target_index = 0;
            t.initial_scan_completed = true;
            t.drives = val_drives;
        });

        let mut writer = state.index_engine.create_writer(64)?;
        summary = validate_and_reconcile_global(state, &mut writer, &targets)?;

        if summary.new_files + summary.updated_files + summary.moved_files + summary.deleted_files > 0 {
            state.index_engine.commit_with_retry(&mut writer)?;
            save_vector_store_to_disk(state);
        }

        cp.updated_at_ms = now_epoch_ms();
        cp.last_checkpoint_path = Some(format!(
            "Validado (+{} ~{} >{} -{})",
            summary.new_files, summary.updated_files, summary.moved_files, summary.deleted_files
        ));
        state.storage.save_checkpoint(&cp)?;
    }

    let final_drives = compute_drive_index_statuses(&state.storage, &cp, "", "READY");
    let final_ts = now_epoch_ms();
    update_telemetry(state, |t| {
        t.phase = "READY".to_string();
        t.current_target.clear();
        t.current_file.clear();
        t.target_processed = 0;
        t.target_total = 0;
        t.percent = 100.0;
        t.new_files = summary.new_files;
        t.updated_files = summary.updated_files;
        t.moved_files = summary.moved_files;
        t.deleted_files = summary.deleted_files;
        t.skipped_files = summary.skipped_files;
        t.last_checkpoint = cp.last_checkpoint_path.clone().unwrap_or_default();
        t.last_sync_ms = final_ts;
        t.initial_scan_completed = true;
        t.drives = final_drives;
    });

    log::info!(
        "Sincronización finalizada en {:?} -> nuevos: {}, modificados: {}, movidos: {}, eliminados: {}, omitidos: {}",
        t0.elapsed(),
        summary.new_files,
        summary.updated_files,
        summary.moved_files,
        summary.deleted_files,
        summary.skipped_files
    );

    Ok(summary)
}

/// Indexa una sola ruta/carpeta específica de forma incremental con soporte de checkpoints por lote.
pub fn index_single_target(
    state: &Arc<SharedRadarState>,
    target: &str,
    force: bool,
) -> Result<SyncSummary> {
    let clean_target = clean_abs_path(Path::new(target)).to_string_lossy().to_string();
    let mut writer = state.index_engine.create_writer(64)?;
    let sum = index_target_incremental_internal(state, &mut writer, &clean_target, force, true, None)?;
    if sum.new_files + sum.updated_files + sum.moved_files + sum.deleted_files > 0 {
        state.index_engine.commit_with_retry(&mut writer)?;
        save_vector_store_to_disk(state);
    }
    Ok(sum)
}

fn index_target_incremental_internal(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    clean_target_str: &str,
    force: bool,
    reconcile_moves_and_deletions: bool,
    mut checkpoint_opt: Option<&mut CheckpointState>,
) -> Result<SyncSummary> {
    let existing_states = state.storage.load_index_states()?;
    let scan_res = UnifiedScanner::scan(clean_target_str).map_err(|e| anyhow::anyhow!(e))?;
    let now_ms = now_epoch_ms();

    let mut summary = SyncSummary::default();
    let mut seen_keys = HashSet::new();
    let mut candidate_new: Vec<FileRecord> = Vec::new();
    let mut batch_records: Vec<FileRecord> = Vec::with_capacity(500);
    let mut batch_states: Vec<(String, IndexedFileState)> = Vec::with_capacity(500);
    let mut pending_changes_since_cp = 0usize;
    let mut vectors_dirty = false;
    let mut last_vector_save = Instant::now();
    let mut tantivy_dirty = false;

    let records = collect_scan_records(scan_res);
    let total_records_in_target = records.len();
    let mut processed_in_target = 0usize;
    let mut last_live_emit = Instant::now();

    update_telemetry(state, |t| {
        t.target_total = total_records_in_target;
        t.target_processed = 0;
        t.current_file = format!("Procesando {} entradas...", total_records_in_target);
    });

    for mut rec in records {
        processed_in_target += 1;
        let Some(raw_path) = rec.path.take() else {
            continue;
        };
        let clean_path = clean_abs_path(&raw_path);
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);
        rec.path = Some(clean_path.clone());

        seen_keys.insert(path_key.clone());

        if processed_in_target % 50 == 0 || last_live_emit.elapsed().as_millis() >= 350 {
            last_live_emit = Instant::now();
            let cur_name = rec.name.clone();
            update_telemetry(state, |t| {
                t.target_processed = processed_in_target;
                t.target_total = total_records_in_target;
                t.current_file = cur_name;
                if t.total_targets > 0 && total_records_in_target > 0 {
                    let base = t.target_index.saturating_sub(1) as f32;
                    let frac = (processed_in_target as f32 / total_records_in_target as f32) * 0.5;
                    t.percent = (((base + frac) / t.total_targets as f32) * 100.0).clamp(0.0, 99.9);
                }
            });
        }

        if rec.is_directory {
            if force || !existing_states.contains_key(&path_key) {
                batch_states.push((
                    path_key,
                    IndexedFileState {
                        doc_id: rec.id,
                        size: 0,
                        modified_ms: rec.modified_ms,
                        last_indexed_ms: now_ms,
                        is_dir: true,
                        original_path: path_str,
                        content_version: CURRENT_CONTENT_VERSION,
                    },
                ));
                batch_records.push(rec);
                if batch_records.len() >= 500 {
                    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                }
            }
            continue;
        }

        if let Some(prev) = existing_states.get(&path_key) {
            let needs_content_upgrade = prev.content_version < CURRENT_CONTENT_VERSION
                && needs_v2_content_reextract(&clean_path)
                && !is_noisy_build_or_cache_path(&path_str);
            if !force && !needs_content_upgrade && prev.size == rec.size && prev.modified_ms == rec.modified_ms {
                summary.skipped_files += 1;
                continue;
            }
            // Modificado por peso o fecha, forzado, o actualización de versión del extractor de contenido
            if process_extract_and_write(
                state,
                writer,
                &rec,
                &clean_path,
                &path_str,
                true,
                &mut vectors_dirty,
            ) {
                tantivy_dirty = true;
            }
            summary.updated_files += 1;
            pending_changes_since_cp += 1;

            batch_states.push((
                path_key,
                IndexedFileState {
                    doc_id: rec.id,
                    size: rec.size,
                    modified_ms: rec.modified_ms,
                    last_indexed_ms: now_ms,
                    is_dir: false,
                    original_path: path_str.clone(),
                    content_version: CURRENT_CONTENT_VERSION,
                },
            ));
            batch_records.push(rec);
        } else {
            // Candidato a nuevo o movido
            candidate_new.push(rec);
        }

        if pending_changes_since_cp >= CHECKPOINT_BATCH_SIZE {
            if tantivy_dirty {
                match state.index_engine.commit_with_retry(writer) {
                    Ok(_) => tantivy_dirty = false,
                    Err(e) => log::warn!("Commit intermedio de Tantivy diferido: {}", e),
                }
            }
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            if vectors_dirty && last_vector_save.elapsed().as_secs() >= VECTOR_SAVE_INTERVAL_SECS {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
                last_vector_save = Instant::now();
            }
            if let Some(ref mut cp) = checkpoint_opt {
                cp.processed_count += pending_changes_since_cp;
                cp.last_checkpoint_path = Some(path_str.clone());
                cp.updated_at_ms = now_epoch_ms();
                let _ = state.storage.save_checkpoint(cp);
            }
            pending_changes_since_cp = 0;
        }
    }

    // Buscar entradas faltantes dentro del árbol escaneado si corresponde conciliar movimientos/borrados
    let mut unmatched_missing: Vec<MissingEntry> = Vec::new();
    let mut missing_by_sig: HashMap<MoveSignature, Vec<MissingEntry>> = HashMap::new();

    if reconcile_moves_and_deletions {
        let target_key = normalize_path_key(clean_target_str);
        let target_prefix = ensure_dir_prefix(&target_key);

        for (key, st) in &existing_states {
            let inside_target = key == &target_key || key.starts_with(&target_prefix);
            if inside_target && !seen_keys.contains(key) {
                let orig_path = resolve_orig_path(&state.storage, key, st);
                if st.is_dir {
                    unmatched_missing.push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                    continue;
                }
                if st.size > 0 && st.modified_ms > 0 {
                    let fname_lower = Path::new(&orig_path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let sig = MoveSignature {
                        name_lower: fname_lower,
                        size: st.size,
                        modified_ms: st.modified_ms,
                    };
                    missing_by_sig.entry(sig).or_default().push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                } else {
                    unmatched_missing.push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                }
            }
        }
    }

    // Procesar candidate_new: conciliar si fue MOVIDO desde otro punto, si no es NUEVO
    let mut moved_removals: Vec<(String, String, u64)> = Vec::new();
    let total_candidates = candidate_new.len();
    let mut processed_candidates = 0usize;
    let mut last_cand_emit = Instant::now();

    for rec in candidate_new {
        processed_candidates += 1;
        let Some(ref clean_path) = rec.path else {
            continue;
        };
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);

        if processed_candidates % 20 == 0 || last_cand_emit.elapsed().as_millis() >= 350 {
            last_cand_emit = Instant::now();
            let cur_name = rec.name.clone();
            let cur_new = summary.new_files;
            let cur_upd = summary.updated_files;
            update_telemetry(state, |t| {
                t.target_processed = processed_candidates;
                t.target_total = total_candidates.max(1);
                t.current_file = cur_name;
                t.last_sync_ms = now_epoch_ms();
                if t.total_targets > 0 && total_candidates > 0 {
                    let base = t.target_index.saturating_sub(1) as f32;
                    let frac = 0.5 + (processed_candidates as f32 / total_candidates as f32) * 0.5;
                    t.percent = (((base + frac) / t.total_targets as f32) * 100.0).clamp(0.0, 99.9);
                }
                if cur_new > t.new_files {
                    t.new_files = cur_new;
                }
                if cur_upd > t.updated_files {
                    t.updated_files = cur_upd;
                }
            });
        }

        let moved_from = if reconcile_moves_and_deletions && rec.size > 0 && rec.modified_ms > 0 {
            let sig = MoveSignature {
                name_lower: rec.name.to_lowercase(),
                size: rec.size,
                modified_ms: rec.modified_ms,
            };
            missing_by_sig.get_mut(&sig).and_then(|list| list.pop())
        } else {
            None
        };

        if let Some(old_entry) = moved_from {
            if handle_moved_file(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &old_entry.orig_path,
                &mut vectors_dirty,
            ) {
                tantivy_dirty = true;
            }
            moved_removals.push((old_entry.key, old_entry.orig_path, old_entry.state.doc_id));
            summary.moved_files += 1;
        } else {
            if process_extract_and_write(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                false,
                &mut vectors_dirty,
            ) {
                tantivy_dirty = true;
            }
            summary.new_files += 1;
        }

        pending_changes_since_cp += 1;
        batch_states.push((
            path_key,
            IndexedFileState {
                doc_id: rec.id,
                size: rec.size,
                modified_ms: rec.modified_ms,
                last_indexed_ms: now_ms,
                is_dir: false,
                original_path: path_str.clone(),
                content_version: CURRENT_CONTENT_VERSION,
            },
        ));
        batch_records.push(rec);

        if pending_changes_since_cp >= CHECKPOINT_BATCH_SIZE {
            if tantivy_dirty {
                match state.index_engine.commit_with_retry(writer) {
                    Ok(_) => tantivy_dirty = false,
                    Err(e) => log::warn!("Commit intermedio de Tantivy diferido: {}", e),
                }
            }
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            if !moved_removals.is_empty() {
                let _ = state.storage.remove_by_paths(&moved_removals);
                moved_removals.clear();
            }
            if vectors_dirty && last_vector_save.elapsed().as_secs() >= VECTOR_SAVE_INTERVAL_SECS {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
                last_vector_save = Instant::now();
            }
            if let Some(ref mut cp) = checkpoint_opt {
                cp.processed_count += pending_changes_since_cp;
                cp.last_checkpoint_path = Some(path_str);
                cp.updated_at_ms = now_epoch_ms();
                let _ = state.storage.save_checkpoint(cp);
            }
            pending_changes_since_cp = 0;
        }
    }

    // Purgar archivos eliminados (lo que no coincidió como movido)
    for (_, list) in missing_by_sig {
        unmatched_missing.extend(list);
    }

    if !unmatched_missing.is_empty() {
        let mut to_remove = Vec::with_capacity(unmatched_missing.len());
        for entry in unmatched_missing {
            state.index_engine.delete_by_path(writer, &entry.orig_path);
            tantivy_dirty = true;
            if let Ok(mut vs) = state.vector_store.write() {
                if vs.remove_by_path(&entry.orig_path) {
                    vectors_dirty = true;
                }
            }
            to_remove.push((entry.key, entry.orig_path, entry.state.doc_id));
            summary.deleted_files += 1;
        }
        state.storage.remove_by_paths(&to_remove)?;
    }

    if !moved_removals.is_empty() {
        state.storage.remove_by_paths(&moved_removals)?;
    }

    // Confirmar Tantivy ANTES de persistir el último lote de `index_state` en Redb para que nunca
    // queden documentos marcados como "ya indexados" sin su contenido confirmado en disco.
    if tantivy_dirty {
        state.index_engine.commit_with_retry(writer)?;
    }
    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
    if vectors_dirty {
        save_vector_store_to_disk(state);
    }

    Ok(summary)
}

/// Validación global de arranque a través de todas las unidades locales del equipo:
/// verifica archivos nuevos, modificados (peso/fecha), eliminados y movidos entre cualquier carpeta/unidad.
fn validate_and_reconcile_global(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    targets: &[String],
) -> Result<SyncSummary> {
    let existing_states = state.storage.load_index_states()?;
    let now_ms = now_epoch_ms();

    let mut summary = SyncSummary::default();
    let mut seen_keys = HashSet::new();
    let mut candidate_new: Vec<FileRecord> = Vec::new();
    let mut instant_new_meta: Vec<FileRecord> = Vec::with_capacity(200);
    let mut deferred_v2_upgrades: Vec<(FileRecord, String, String)> = Vec::new();
    let mut batch_records: Vec<FileRecord> = Vec::with_capacity(500);
    let mut batch_states: Vec<(String, IndexedFileState)> = Vec::with_capacity(500);
    let mut pending_changes = 0usize;
    let mut vectors_dirty = false;
    let mut last_vector_save = Instant::now();
    let mut tantivy_dirty = false;
    let mut aborted_by_shutdown = false;

    let total_targets = targets.len();
    update_telemetry(state, |t| {
        t.total_targets = total_targets;
    });

    for (idx, target) in targets.iter().enumerate() {
        if state.is_shutdown_requested() {
            aborted_by_shutdown = true;
            break;
        }
        if !Path::new(target).exists() {
            continue;
        }
        update_telemetry(state, |t| {
            t.target_index = idx + 1;
            t.current_target = target.clone();
            t.target_processed = 0;
            t.target_total = 0;
            t.current_file = "Escaneando...".to_string();
        });

        let Ok(scan_res) = UnifiedScanner::scan(target) else {
            continue;
        };
        let records = collect_scan_records(scan_res);
        let total_in_target = records.len();
        let mut processed_in_target = 0usize;
        let mut last_val_emit = Instant::now();

        update_telemetry(state, |t| {
            t.target_total = total_in_target;
        });

        for mut rec in records {
            if processed_in_target % 64 == 0 && state.is_shutdown_requested() {
                aborted_by_shutdown = true;
                break;
            }
            processed_in_target += 1;
            let Some(raw_path) = rec.path.take() else {
                continue;
            };
            let clean_path = clean_abs_path(&raw_path);
            let path_str = clean_path.to_string_lossy().to_string();
            let path_key = normalize_path_key(&path_str);
            rec.path = Some(clean_path.clone());

            seen_keys.insert(path_key.clone());

            if processed_in_target % 100 == 0 || last_val_emit.elapsed().as_millis() >= 500 {
                last_val_emit = Instant::now();
                let cur_name = rec.name.clone();
                let cur_upd = summary.updated_files;
                update_telemetry(state, |t| {
                    t.target_processed = processed_in_target;
                    t.target_total = total_in_target;
                    t.current_file = cur_name;
                    t.updated_files = cur_upd;
                    t.last_sync_ms = now_epoch_ms();
                });
            }

            if rec.is_directory {
                if !existing_states.contains_key(&path_key) {
                    batch_states.push((
                        path_key,
                        IndexedFileState {
                            doc_id: rec.id,
                            size: 0,
                            modified_ms: rec.modified_ms,
                            last_indexed_ms: now_ms,
                            is_dir: true,
                            original_path: path_str,
                            content_version: CURRENT_CONTENT_VERSION,
                        },
                    ));
                    batch_records.push(rec);
                    if batch_records.len() >= 500 {
                        summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                    }
                }
                continue;
            }

            if let Some(prev) = existing_states.get(&path_key) {
                let changed_on_disk = prev.size != rec.size || prev.modified_ms != rec.modified_ms;
                if !changed_on_disk {
                    // Diferir mejoras de extractor v2 hasta DESPUÉS de descubrir todos los archivos nuevos/modificados
                    if prev.content_version < CURRENT_CONTENT_VERSION
                        && needs_v2_content_reextract(&clean_path)
                        && !is_noisy_build_or_cache_path(&path_str)
                    {
                        deferred_v2_upgrades.push((rec, path_key, path_str));
                    } else {
                        summary.skipped_files += 1;
                    }
                    continue;
                }
                // Modificado realmente en disco (peso o fecha): actualizar de inmediato
                if process_extract_and_write(
                    state,
                    writer,
                    &rec,
                    &clean_path,
                    &path_str,
                    true,
                    &mut vectors_dirty,
                ) {
                    tantivy_dirty = true;
                }
                summary.updated_files += 1;
                pending_changes += 1;

                batch_states.push((
                    path_key,
                    IndexedFileState {
                        doc_id: rec.id,
                        size: rec.size,
                        modified_ms: rec.modified_ms,
                        last_indexed_ms: now_ms,
                        is_dir: false,
                        original_path: path_str,
                        content_version: CURRENT_CONTENT_VERSION,
                    },
                ));
                batch_records.push(rec);
            } else {
                // Hacer visible el nombre en Redb + caché RAM de inmediato (sin esperar al final de los 196 targets)
                instant_new_meta.push(rec.clone());
                if instant_new_meta.len() >= 200 {
                    let _ = state.storage.insert_batch(&instant_new_meta);
                    instant_new_meta.clear();
                }
                candidate_new.push(rec);
            }

            if pending_changes >= CHECKPOINT_BATCH_SIZE {
                if tantivy_dirty {
                    match state.index_engine.commit_with_retry(writer) {
                        Ok(_) => tantivy_dirty = false,
                        Err(e) => log::warn!("Commit intermedio de validación diferido: {}", e),
                    }
                }
                summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                if vectors_dirty && last_vector_save.elapsed().as_secs() >= VECTOR_SAVE_INTERVAL_SECS {
                    save_vector_store_to_disk(state);
                    vectors_dirty = false;
                    last_vector_save = Instant::now();
                }
                pending_changes = 0;
            }
        }

        if !instant_new_meta.is_empty() {
            let _ = state.storage.insert_batch(&instant_new_meta);
            instant_new_meta.clear();
        }
        if aborted_by_shutdown {
            break;
        }
    }

    if aborted_by_shutdown {
        if tantivy_dirty {
            let _ = state.index_engine.commit_with_retry(writer);
        }
        let _ = flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states);
        if vectors_dirty {
            save_vector_store_to_disk(state);
        }
        return Ok(summary);
    }

    // Verificar todas las entradas no vistas en disco para detectar movidos vs eliminados reales
    let mut unmatched_missing: Vec<MissingEntry> = Vec::new();
    let mut missing_by_sig: HashMap<MoveSignature, Vec<MissingEntry>> = HashMap::new();

    for (key, st) in &existing_states {
        if seen_keys.contains(key) {
            continue;
        }
        let orig_path = resolve_orig_path(&state.storage, key, st);
        // Comprobación física: si aún existe en disco (ej. carpeta personalizada agregada manualmente), no borrar
        if Path::new(&orig_path).exists() {
            continue;
        }
        if st.is_dir {
            unmatched_missing.push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
            continue;
        }

        if st.size > 0 && st.modified_ms > 0 {
            let fname_lower = Path::new(&orig_path)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let sig = MoveSignature {
                name_lower: fname_lower,
                size: st.size,
                modified_ms: st.modified_ms,
            };
            missing_by_sig.entry(sig).or_default().push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
        } else {
            unmatched_missing.push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
        }
    }

    // Conciliar candidate_new con missing_by_sig (detección global de archivos movidos entre carpetas/unidades)
    let mut moved_removals: Vec<(String, String, u64)> = Vec::new();
    for rec in candidate_new {
        let Some(ref clean_path) = rec.path else {
            continue;
        };
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);

        let moved_from = if rec.size > 0 && rec.modified_ms > 0 {
            let sig = MoveSignature {
                name_lower: rec.name.to_lowercase(),
                size: rec.size,
                modified_ms: rec.modified_ms,
            };
            missing_by_sig.get_mut(&sig).and_then(|list| list.pop())
        } else {
            None
        };

        if let Some(old_entry) = moved_from {
            log::info!("[MOVED] {} -> {}", old_entry.orig_path, path_str);
            if handle_moved_file(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &old_entry.orig_path,
                &mut vectors_dirty,
            ) {
                tantivy_dirty = true;
            }
            moved_removals.push((old_entry.key, old_entry.orig_path, old_entry.state.doc_id));
            summary.moved_files += 1;
        } else {
            if process_extract_and_write(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                false,
                &mut vectors_dirty,
            ) {
                tantivy_dirty = true;
            }
            summary.new_files += 1;
        }

        pending_changes += 1;
        batch_states.push((
            path_key,
            IndexedFileState {
                doc_id: rec.id,
                size: rec.size,
                modified_ms: rec.modified_ms,
                last_indexed_ms: now_ms,
                is_dir: false,
                original_path: path_str,
                content_version: CURRENT_CONTENT_VERSION,
            },
        ));
        batch_records.push(rec);

        if pending_changes >= CHECKPOINT_BATCH_SIZE {
            if tantivy_dirty {
                match state.index_engine.commit_with_retry(writer) {
                    Ok(_) => tantivy_dirty = false,
                    Err(e) => log::warn!("Commit intermedio de validación diferido: {}", e),
                }
            }
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            if !moved_removals.is_empty() {
                let _ = state.storage.remove_by_paths(&moved_removals);
                moved_removals.clear();
            }
            if vectors_dirty && last_vector_save.elapsed().as_secs() >= VECTOR_SAVE_INTERVAL_SECS {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
                last_vector_save = Instant::now();
            }
            pending_changes = 0;
        }
    }

    for (_, list) in missing_by_sig {
        unmatched_missing.extend(list);
    }

    if !unmatched_missing.is_empty() {
        let mut to_remove = Vec::with_capacity(unmatched_missing.len());
        for entry in unmatched_missing {
            log::debug!("[DELETED] {}", entry.orig_path);
            state.index_engine.delete_by_path(writer, &entry.orig_path);
            tantivy_dirty = true;
            if let Ok(mut vs) = state.vector_store.write() {
                if vs.remove_by_path(&entry.orig_path) {
                    vectors_dirty = true;
                }
            }
            to_remove.push((entry.key, entry.orig_path, entry.state.doc_id));
            summary.deleted_files += 1;
        }
        state.storage.remove_by_paths(&to_remove)?;
    }

    if !moved_removals.is_empty() {
        state.storage.remove_by_paths(&moved_removals)?;
    }

    // Fase 2 (no bloqueante): actualizar extracción de contenido v2 en documentos Office/PDF ya existentes,
    // después de haber garantizado que TODOS los archivos nuevos/raíz/modificados ya son buscables.
    if !deferred_v2_upgrades.is_empty() && !state.is_shutdown_requested() {
        let total_v2 = deferred_v2_upgrades.len();
        update_telemetry(state, |t| {
            t.current_target = format!("Actualizando extractores v2 ({} docs Office/PDF)", total_v2);
            t.target_processed = 0;
            t.target_total = total_v2;
        });
        let mut last_v2_emit = Instant::now();
        for (v2_idx, (rec, path_key, path_str)) in deferred_v2_upgrades.into_iter().enumerate() {
            if state.is_shutdown_requested() {
                break;
            }
            if let Some(ref clean_path) = rec.path {
                if process_extract_and_write(
                    state,
                    writer,
                    &rec,
                    clean_path,
                    &path_str,
                    true,
                    &mut vectors_dirty,
                ) {
                    tantivy_dirty = true;
                }
                summary.updated_files += 1;
                pending_changes += 1;
                batch_states.push((
                    path_key,
                    IndexedFileState {
                        doc_id: rec.id,
                        size: rec.size,
                        modified_ms: rec.modified_ms,
                        last_indexed_ms: now_ms,
                        is_dir: false,
                        original_path: path_str,
                        content_version: CURRENT_CONTENT_VERSION,
                    },
                ));
                batch_records.push(rec);
            }
            if v2_idx % 25 == 0 || last_v2_emit.elapsed().as_millis() >= 500 {
                last_v2_emit = Instant::now();
                let upd = summary.updated_files;
                update_telemetry(state, |t| {
                    t.target_processed = v2_idx + 1;
                    t.target_total = total_v2;
                    t.updated_files = upd;
                    t.last_sync_ms = now_epoch_ms();
                });
            }
            if pending_changes >= CHECKPOINT_BATCH_SIZE {
                if tantivy_dirty {
                    match state.index_engine.commit_with_retry(writer) {
                        Ok(_) => tantivy_dirty = false,
                        Err(e) => log::warn!("Commit intermedio v2 diferido: {}", e),
                    }
                }
                summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                if vectors_dirty && last_vector_save.elapsed().as_secs() >= VECTOR_SAVE_INTERVAL_SECS {
                    save_vector_store_to_disk(state);
                    vectors_dirty = false;
                    last_vector_save = Instant::now();
                }
                pending_changes = 0;
            }
        }
    }

    if tantivy_dirty {
        state.index_engine.commit_with_retry(writer)?;
    }
    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
    if vectors_dirty {
        save_vector_store_to_disk(state);
    }

    Ok(summary)
}

fn handle_moved_file(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    rec: &FileRecord,
    clean_path: &Path,
    new_path_str: &str,
    old_path_str: &str,
    vectors_dirty: &mut bool,
) -> bool {
    let was_doc = classify_path(Path::new(old_path_str)) == FileCategory::Document
        && !is_noisy_build_or_cache_path(old_path_str);
    let mut tantivy_changed = false;
    if was_doc {
        state.index_engine.delete_by_path(writer, old_path_str);
        tantivy_changed = true;
    }

    if classify_path(clean_path) != FileCategory::Document || is_noisy_build_or_cache_path(new_path_str) {
        if let Ok(mut vs) = state.vector_store.write() {
            if vs.remove_by_path(old_path_str) {
                *vectors_dirty = true;
            }
        }
        return tantivy_changed;
    }

    if let Ok(mut vs) = state.vector_store.write() {
        if vs.relocate_path(old_path_str, new_path_str, rec.id) {
            *vectors_dirty = true;
        }
    }

    let format = detect_format(clean_path);
    let content = if format != DocumentFormat::Unsupported {
        extract_content(clean_path, 150_000).unwrap_or_default()
    } else {
        String::new()
    };
    if state
        .index_engine
        .add_document(
            writer,
            rec.id,
            new_path_str,
            &rec.name,
            &content,
            rec.size,
            rec.modified_ms,
        )
        .is_ok()
    {
        tantivy_changed = true;
    }
    tantivy_changed
}

fn process_extract_and_write(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    rec: &FileRecord,
    clean_path: &Path,
    path_str: &str,
    is_existing_entry: bool,
    vectors_dirty: &mut bool,
) -> bool {
    // Regla estricta:
    // - Documentos conocidos (fuera de cachés/builds ruidosos) -> Nombre (Redb) + Contenido (Tantivy) + Vectores (Model2Vec)
    // - Multimedia, Imágenes/Diseño, Sistema/Binarios y cachés -> Únicamente por Nombre (Redb)
    if classify_path(clean_path) != FileCategory::Document || is_noisy_build_or_cache_path(path_str) {
        if is_existing_entry {
            state.index_engine.delete_by_path(writer, path_str);
            if let Ok(mut vs) = state.vector_store.write() {
                if vs.remove_by_path(path_str) {
                    *vectors_dirty = true;
                }
            }
            return true;
        }
        return false;
    }

    let format = detect_format(clean_path);
    let content = if format != DocumentFormat::Unsupported {
        extract_content(clean_path, 150_000).unwrap_or_default()
    } else {
        String::new()
    };

    let tantivy_changed = state
        .index_engine
        .add_document(
            writer,
            rec.id,
            path_str,
            &rec.name,
            &content,
            rec.size,
            rec.modified_ms,
        )
        .is_ok();

    if !content.is_empty() {
        let snippet: String = content.chars().take(800).collect();
        let vec = state.embedder.embed(&snippet);
        if let Ok(mut vs) = state.vector_store.write() {
            vs.upsert(rec.id, path_str, 0, &snippet, &vec);
            *vectors_dirty = true;
        }
    } else if is_existing_entry {
        if let Ok(mut vs) = state.vector_store.write() {
            if vs.remove_by_path(path_str) {
                *vectors_dirty = true;
            }
        }
    }

    tantivy_changed
}

fn collect_scan_records(scan_res: UnifiedScanResult) -> Vec<FileRecord> {
    match scan_res {
        #[cfg(windows)]
        UnifiedScanResult::Mft(mft) => {
            let mut out = Vec::with_capacity(mft.records.len());
            for (&id, &(parent_id, ref name, attr)) in &mft.records {
                let is_dir = (attr & FileRecord::ATTR_DIRECTORY) != 0;
                if let Some(resolved_path) = mft.resolve_path(id) {
                    let (size, modified_ms) = if is_dir {
                        (0, 0)
                    } else {
                        read_size_and_mtime(&resolved_path)
                    };
                    out.push(FileRecord {
                        id,
                        parent_id,
                        name: name.clone(),
                        path: Some(resolved_path),
                        is_directory: is_dir,
                        size,
                        modified_ms,
                        attributes: attr,
                    });
                }
            }
            out
        }
        UnifiedScanResult::Fallback(fallback) => fallback.records,
    }
}

fn flush_redb_batches(
    storage: &DragonStorage,
    batch_records: &mut Vec<FileRecord>,
    batch_states: &mut Vec<(String, IndexedFileState)>,
) -> Result<usize> {
    let mut stored = 0;
    if !batch_records.is_empty() {
        stored = storage.insert_batch(batch_records)?;
        batch_records.clear();
    }
    if !batch_states.is_empty() {
        storage.upsert_index_states(batch_states)?;
        batch_states.clear();
    }
    Ok(stored)
}

fn save_vector_store_to_disk(state: &Arc<SharedRadarState>) {
    if let Some(parent) = state.vectors_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(vs) = state.vector_store.read() {
        let _ = vs.save_to_file(&state.vectors_file);
    }
}

fn resolve_orig_path(storage: &DragonStorage, key: &str, st: &IndexedFileState) -> String {
    if !st.original_path.is_empty() {
        return st.original_path.clone();
    }
    storage
        .get_by_id(st.doc_id)
        .ok()
        .flatten()
        .and_then(|r| r.path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| key.to_string())
}

fn update_telemetry<F: FnOnce(&mut IndexerTelemetry)>(state: &Arc<SharedRadarState>, f: F) {
    if let Ok(mut t) = state.telemetry.write() {
        f(&mut t);
    }
}

fn extract_drive_root_upper(path: &str) -> Option<String> {
    let cleaned = path
        .trim()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\");
    let bytes = cleaned.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()));
    }
    None
}

/// Calcula el estado de indexación y cobertura real de cada unidad fija detectada en el equipo.
pub fn compute_drive_index_statuses(
    storage: &DragonStorage,
    cp: &CheckpointState,
    current_target: &str,
    phase: &str,
) -> Vec<DriveIndexStatus> {
    let fixed_drives = discover_fixed_drives();
    let mut counts_by_drive: HashMap<String, usize> = HashMap::new();
    if let Ok(states) = storage.load_index_states() {
        for (k, st) in states {
            if st.is_dir {
                continue;
            }
            if let Some(drv) = extract_drive_root_upper(&k) {
                *counts_by_drive.entry(drv).or_insert(0) += 1;
            }
        }
    }

    let mut targets_total_by_drive: HashMap<String, usize> = HashMap::new();
    let mut targets_done_by_drive: HashMap<String, usize> = HashMap::new();
    for (idx, target) in cp.targets.iter().enumerate() {
        if let Some(drv) = extract_drive_root_upper(target) {
            *targets_total_by_drive.entry(drv.clone()).or_insert(0) += 1;
            if cp.initial_scan_completed || (!cp.in_progress && cp.current_target_idx == 0 && idx < cp.targets.len()) || idx < cp.current_target_idx {
                if cp.initial_scan_completed || idx < cp.current_target_idx {
                    *targets_done_by_drive.entry(drv).or_insert(0) += 1;
                }
            }
        }
    }

    let active_drive = extract_drive_root_upper(current_target);
    let mut out = Vec::with_capacity(fixed_drives.len());
    for drv in fixed_drives {
        let drv_key = extract_drive_root_upper(&drv).unwrap_or_else(|| drv.to_ascii_uppercase());
        let t_total = *targets_total_by_drive.get(&drv_key).unwrap_or(&0);
        let t_done = *targets_done_by_drive.get(&drv_key).unwrap_or(&0);
        let indexed_files = *counts_by_drive.get(&drv_key).unwrap_or(&0);

        let status = if phase != "READY" && active_drive.as_deref() == Some(drv_key.as_str()) {
            "INDEXING".to_string()
        } else if t_total > 0 && t_done < t_total {
            "PENDING".to_string()
        } else if indexed_files > 0 {
            "OK".to_string()
        } else if cp.initial_scan_completed {
            "MISSING".to_string()
        } else {
            "PENDING".to_string()
        };

        out.push(DriveIndexStatus {
            drive: drv_key,
            targets_total: t_total,
            targets_completed: t_done,
            indexed_files,
            status,
        });
    }
    out
}

pub fn clean_abs_path(path: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|c| c.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    let s = abs.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", stripped))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        abs
    }
}

pub fn normalize_path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

fn ensure_dir_prefix(key: &str) -> String {
    if key.ends_with('\\') || key.ends_with('/') {
        key.to_string()
    } else {
        format!("{}{}", key, std::path::MAIN_SEPARATOR)
    }
}

pub fn read_size_and_mtime(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let size = m.len();
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (size, mtime)
        }
        Err(_) => (0, 0),
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_unc_network_paths_and_isolates_per_machine_storage() {
        assert!(is_network_path(Path::new(r"\\SERVIDOR\Compartido\dragon.exe")));
        assert!(is_network_path(Path::new(r"\\?\UNC\SERVIDOR\Compartido\dragon.exe")));
        assert!(!is_network_path(Path::new(r"C:\RadarDragon\dragon.exe")));

        let isolated = resolve_machine_storage_paths(true);
        assert!(!isolated.machine_name.is_empty());
        assert!(isolated.data_root.to_string_lossy().contains(&isolated.machine_name));

        let tmp = std::env::temp_dir().join(format!("dragon_owner_test_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let st = DragonStorage::open(&tmp).unwrap();
        st.set_setting(OWNER_MACHINE_SETTING_KEY, "SERVIDOR-CENTRAL").unwrap();
        drop(st);

        assert!(existing_db_belongs_to_other_machine(&tmp, "PC-VENTAS-01"));
        assert!(!existing_db_belongs_to_other_machine(&tmp, "SERVIDOR-CENTRAL"));

        let _ = std::fs::remove_file(&tmp);
    }
}
