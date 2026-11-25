//! dragon-cli::ui_server - Servidor HTTP asíncrono para la interfaz táctica del Radar del Dragón.
//! Provee comunicación REST instantánea entre el HUD Canvas del navegador y los motores
//! en Rust (Tantivy, Redb, Model2Vec, MFT y Extractor), y ejecuta en segundo plano la
//! indexación automática inicial con checkpoints y validación de arranque.

use crate::config::{LanMode, LoadedSiteConfig};
use crate::indexer::{
    get_machine_name, resolve_machine_storage_paths, resolve_shared_server_storage_paths,
    run_startup_auto_sync_and_validate, run_startup_auto_sync_with_targets, MachineStoragePaths,
    SharedRadarState,
};
use anyhow::Result;
use chrono::{DateTime, Local};
use dragon_core::{classify_path, extract_extension_lower, is_noisy_build_or_cache_path, FileCategory};
use dragon_extract::{
    detect_format, extract_content, extract_docx_paragraphs, extract_excel_visual,
    extract_pdf_preview, extract_pptx_slides, extract_pptx_visual, extract_word_visual,
    extract_xlsx_sheets, extract_zip_entries, read_zip_entry_bytes, DocumentFormat,
};
use rayon::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};

pub const APP_VERSION: &str = "1.3.0";

pub struct UiServerOptions {
    pub host: String,
    pub port: Option<u16>,
    pub force_local: bool,
    pub no_browser: bool,
    pub strict_port: bool,
    pub config_path: Option<PathBuf>,
    pub lan_mode_override: Option<LanMode>,
}

enum ServerBootState {
    Loading { started_at_ms: u64, stage: String },
    Ready(Arc<SharedRadarState>),
    Failed(String),
}

struct RuntimeServerContext {
    machine_name: String,
    machine_paths: Arc<RwLock<Option<MachineStoragePaths>>>,
    boot_state: Arc<RwLock<ServerBootState>>,
    site_config: LoadedSiteConfig,
    listen_port: u16,
    bind_lan: bool,
    sync_running: Arc<AtomicBool>,
    shared_roots_for_sync: Option<Vec<PathBuf>>,
    shutdown_notify: Arc<Notify>,
}

fn get_ready_state(ctx: &RuntimeServerContext) -> Option<Arc<SharedRadarState>> {
    let guard = ctx.boot_state.read().ok()?;
    match &*guard {
        ServerBootState::Ready(st) => Some(Arc::clone(st)),
        _ => None,
    }
}

fn now_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn last_cleanup_script_epoch_secs() -> &'static AtomicU64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    &LAST
}

fn find_index_validation_script() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
            if let Some(repo_root) = parent.parent().and_then(|p| p.parent()) {
                dirs.push(repo_root.to_path_buf());
            }
        }
    }
    for dir in dirs {
        for rel in [
            PathBuf::from("deploy").join("validar-index-al-abrir.ps1"),
            PathBuf::from("herramientas").join("validar-index-al-abrir.ps1"),
        ] {
            let candidate = dir.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Ejecuta `validar-index-al-abrir.ps1` en segundo plano (sin ventana visible) para cerrar
/// ventanas de comandos auxiliares una vez que la indexación está confirmada como completa.
fn trigger_console_cleanup_script_debounced(port: u16) {
    #[cfg(windows)]
    {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let prev = last_cleanup_script_epoch_secs().load(Ordering::Relaxed);
        if now_secs.saturating_sub(prev) < 8 {
            return;
        }
        if last_cleanup_script_epoch_secs()
            .compare_exchange(prev, now_secs, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        if let Some(script_path) = find_index_validation_script() {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-WindowStyle",
                    "Hidden",
                    "-File",
                    &script_path.to_string_lossy(),
                    "-Port",
                    &port.to_string(),
                    "-CleanupOnly",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = port;
    }
}

fn spawn_background_sync_if_idle(
    state: &Arc<SharedRadarState>,
    sync_running: &Arc<AtomicBool>,
    shared_roots_for_sync: Option<Vec<PathBuf>>,
    listen_port: u16,
) -> bool {
    if sync_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }
    let bg_state = Arc::clone(state);
    let bg_flag = Arc::clone(sync_running);
    std::thread::spawn(move || {
        // Cargar caché RAM de búsqueda rápida y vectors.bin en segundo plano sin bloquear el arranque HTTP
        bg_state.hydrate_background_caches();
        let res = match shared_roots_for_sync {
            Some(roots) => run_startup_auto_sync_with_targets(&bg_state, false, Some(&roots)),
            None => run_startup_auto_sync_and_validate(&bg_state, false),
        };
        bg_flag.store(false, Ordering::SeqCst);
        match res {
            Ok(_) => {
                // Al finalizar completamente la indexación en segundo plano, cerrar ventanas de comandos auxiliares
                trigger_console_cleanup_script_debounced(listen_port);
            }
            Err(e) => {
                log::error!("Error en sincronización automática de inicio: {}", e);
                if let Ok(mut t) = bg_state.telemetry.write() {
                    t.phase = "READY".to_string();
                    t.last_checkpoint = format!("Error: {}", e);
                }
            }
        }
    });
    true
}

fn configured_libreoffice_path_store() -> &'static RwLock<Option<PathBuf>> {
    static STORE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(None))
}

fn set_configured_libreoffice_path(path: Option<&str>) {
    if let Ok(mut guard) = configured_libreoffice_path_store().write() {
        *guard = path
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
    }
}

pub async fn run_ui_server(options: UiServerOptions) -> Result<()> {
    let site_config = LoadedSiteConfig::load(options.config_path.as_deref())
        .with_lan_mode_override(options.lan_mode_override);
    set_configured_libreoffice_path(site_config.config.libreoffice_path.as_deref());

    #[cfg(feature = "lan")]
    let bind_ip: IpAddr = options
        .host
        .parse()
        .unwrap_or_else(|_| IpAddr::from([127, 0, 0, 1]));
    #[cfg(not(feature = "lan"))]
    let bind_ip: IpAddr = {
        let _ = &options.host;
        IpAddr::from([127, 0, 0, 1])
    };
    let bind_lan = !bind_ip.is_loopback();
    let active_shared_server = bind_lan && site_config.is_shared_server();
    let machine_name = get_machine_name();

    // 1. Enlazar el puerto HTTP PRIMERO (< 5 ms), ANTES de abrir/recuperar radar.redb o Tantivy.
    //    Así la plataforma, /health y el HUD cargan instantáneamente incluso tras un corte eléctrico.
    let requested_port = options.port.unwrap_or(site_config.config.port);
    let mut port = requested_port;
    let mut listener = None;
    let max_port_offset = if options.strict_port { 1 } else { 30 };
    for offset in 0..max_port_offset {
        let Some(p) = requested_port.checked_add(offset) else {
            break;
        };
        let addr = SocketAddr::new(bind_ip, p);
        match TcpListener::bind(addr).await {
            Ok(l) => {
                port = p;
                listener = Some(l);
                break;
            }
            Err(_) => continue,
        }
    }
    let listener = listener.ok_or_else(|| {
        if options.strict_port {
            anyhow::anyhow!("El puerto {}:{} ya está ocupado", bind_ip, requested_port)
        } else {
            anyhow::anyhow!(
                "No se pudo enlazar ningún puerto entre {} y {}",
                requested_port,
                requested_port.saturating_add(29)
            )
        }
    })?;

    let sync_running = Arc::new(AtomicBool::new(false));
    let shared_roots_for_sync = if active_shared_server {
        Some(site_config.canonical_shared_roots.clone())
    } else {
        None
    };
    let machine_paths_slot: Arc<RwLock<Option<MachineStoragePaths>>> = Arc::new(RwLock::new(None));
    let boot_state: Arc<RwLock<ServerBootState>> = Arc::new(RwLock::new(ServerBootState::Loading {
        started_at_ms: now_ms_u64(),
        stage: "Abriendo / verificando base de datos radar.redb...".to_string(),
    }));
    let shutdown_notify = Arc::new(Notify::new());

    let shared_ctx = Arc::new(RuntimeServerContext {
        machine_name: machine_name.clone(),
        machine_paths: Arc::clone(&machine_paths_slot),
        boot_state: Arc::clone(&boot_state),
        site_config,
        listen_port: port,
        bind_lan,
        sync_running: Arc::clone(&sync_running),
        shared_roots_for_sync: shared_roots_for_sync.clone(),
        shutdown_notify: Arc::clone(&shutdown_notify),
    });

    // 2. Abrir Redb + Tantivy + LibreOffice + sincronización en hilo de fondo sin bloquear el puerto HTTP
    {
        let boot_ctx = Arc::clone(&shared_ctx);
        let force_local = options.force_local;
        std::thread::spawn(move || {
            let base_paths = resolve_machine_storage_paths(force_local);
            let resolved_paths = if active_shared_server {
                resolve_shared_server_storage_paths(&base_paths)
            } else {
                base_paths
            };
            if let Ok(mut mp) = boot_ctx.machine_paths.write() {
                *mp = Some(resolved_paths.clone());
            }
            if let Ok(mut bs) = boot_ctx.boot_state.write() {
                if let ServerBootState::Loading { ref mut stage, .. } = *bs {
                    *stage = format!(
                        "Abriendo / recuperando base de datos ({})...",
                        resolved_paths.db_path.display()
                    );
                }
            }

            match SharedRadarState::open_fast(
                &resolved_paths.index_dir,
                &resolved_paths.db_path,
                &resolved_paths.vectors_file,
            ) {
                Ok(state) => {
                    let lo_status = ensure_libreoffice_first_run_check(&state.storage);
                    if lo_status.installed {
                        log::info!(
                            "LibreOffice detectado: {}",
                            lo_status.path.as_deref().unwrap_or("soffice")
                        );
                    }
                    if let Ok(mut bs) = boot_ctx.boot_state.write() {
                        *bs = ServerBootState::Ready(Arc::clone(&state));
                    }
                    log::info!(
                        "Base de datos e índice listos ({}); iniciando hidratación y sincronización en segundo plano...",
                        resolved_paths.data_root.display()
                    );
                    spawn_background_sync_if_idle(
                        &state,
                        &boot_ctx.sync_running,
                        boot_ctx.shared_roots_for_sync.clone(),
                        boot_ctx.listen_port,
                    );
                }
                Err(err) => {
                    let msg = format!("Error abriendo almacenamiento local: {:#}", err);
                    log::error!("{}", msg);
                    if let Ok(mut bs) = boot_ctx.boot_state.write() {
                        *bs = ServerBootState::Failed(msg);
                    }
                }
            }
        });
    }

    // 3. Regla de mantenimiento continuo: sincronización incremental periódica una vez que el estado esté Ready
    let sync_interval_min = shared_ctx.site_config.config.sync_interval_minutes;
    if sync_interval_min > 0 {
        let periodic_ctx = Arc::clone(&shared_ctx);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                sync_interval_min.max(1) * 60,
            ));
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Some(st) = get_ready_state(&periodic_ctx) {
                    let started = spawn_background_sync_if_idle(
                        &st,
                        &periodic_ctx.sync_running,
                        periodic_ctx.shared_roots_for_sync.clone(),
                        periodic_ctx.listen_port,
                    );
                    if started {
                        log::info!(
                            "[AUTO-SYNC] Iniciando actualización periódica de DB (cada {} min)...",
                            sync_interval_min
                        );
                    }
                }
            }
        });
    }

    let local_url = format!("http://127.0.0.1:{}", port);
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║       DRAGON RADAR TOOL - TACTICAL HUD UI SERVER         ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("  ⚡ Equipo activo  : {} (Puerto HTTP activo inmediatamente)", machine_name);
    println!("  ⚡ Radar local    : {}", local_url);
    if let Some(ref cfg_path) = shared_ctx.site_config.source_path {
        println!("  ⚡ Configuración  : {}", cfg_path.display());
    }
    if let Some(ref subnet) = shared_ctx.site_config.config.authorized_subnet {
        println!("  ⚡ Subred LAN     : {} (aislamiento CIDR activo)", subnet);
    }
    #[cfg(feature = "lan")]
    if bind_lan {
        if shared_ctx.site_config.is_shared_server() {
            println!(
                "  ⚡ Modo Red LAN   : SERVIDOR COMPARTIDO en puerto {} ({} carpeta(s) autorizada(s))",
                port,
                shared_ctx.site_config.canonical_shared_roots.len()
            );
            for r in &shared_ctx.site_config.canonical_shared_roots {
                println!("     └─ Compartida  : {}", r.display());
            }
            if shared_ctx.site_config.canonical_shared_roots.is_empty() {
                println!("  ⚠️ ATENCIÓN       : shared_paths está vacío en config/site_config.json (ninguna carpeta expuesta en LAN)");
            }
        } else {
            println!(
                "  ⚡ Modo Red LAN   : AGENTE LOCAL POR EQUIPO en puerto {} (cada PC busca solo en sus discos)",
                port
            );
        }
    }
    #[cfg(not(feature = "lan"))]
    println!("  ⚡ Edición        : PORTABLE (Solo equipo local 127.0.0.1, sin red LAN)");
    println!("  ⚡ Base de datos  : Cargando/verificando en segundo plano (HTTP ya en línea)");
    if !options.no_browser {
        println!("  ⚡ Abriendo el navegador automáticamente...");
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", &local_url])
                .spawn();
        }
    }
    println!("  ⚡ (Mantén esta ventana abierta mientras usas el radar)\n");

    loop {
        tokio::select! {
            _ = shutdown_notify.notified() => {
                log::info!("Apagado controlado recibido por /api/shutdown; cerrando base de datos limpiamente...");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("Señal Ctrl+C recibida; cerrando base de datos limpiamente...");
                break;
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let conn_ctx = Arc::clone(&shared_ctx);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(socket, conn_ctx).await {
                        log::debug!("Error en conexión HTTP: {}", e);
                    }
                });
            }
        }
    }
    // Apagado cooperativo real: avisar al hilo indexador en segundo plano, esperar que suelte
    // radar.redb limpiamente y liberar explícitamente `SharedRadarState` antes de salir de `main()`.
    if let Some(st) = get_ready_state(&shared_ctx) {
        st.shutdown_requested.store(true, Ordering::SeqCst);
        drop(st);
    }
    for _ in 0..60 {
        if !shared_ctx.sync_running.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if let Ok(mut bs) = shared_ctx.boot_state.write() {
        *bs = ServerBootState::Failed("Servidor apagado limpiamente".to_string());
    }
    Ok(())
}

fn can_access_data_api(peer_is_loopback: bool, ctx: &RuntimeServerContext) -> bool {
    if peer_is_loopback {
        return true;
    }
    ctx.bind_lan && ctx.site_config.is_shared_server()
}

fn can_access_file(file: &str, peer_is_loopback: bool, ctx: &RuntimeServerContext) -> bool {
    if ctx.bind_lan && ctx.site_config.is_shared_server() {
        return ctx.site_config.is_path_allowed_for_remote(file);
    }
    peer_is_loopback
}

fn build_boot_or_status_json(
    ctx: &RuntimeServerContext,
    peer_is_loopback: bool,
    is_active_shared_server: bool,
) -> serde_json::Value {
    if let Some(state) = get_ready_state(ctx) {
        return build_status_json(&state, ctx, peer_is_loopback, is_active_shared_server);
    }

    let (status_label, phase_label, current_target, current_file, running_bg) =
        if let Ok(guard) = ctx.boot_state.read() {
            match &*guard {
                ServerBootState::Loading { started_at_ms, stage } => {
                    let elapsed_secs = now_ms_u64().saturating_sub(*started_at_ms) / 1000;
                    (
                        "RECOVERING_DB",
                        "RECOVERING_DB",
                        stage.clone(),
                        format!("Verificando / recuperando base de datos ({} s)...", elapsed_secs),
                        true,
                    )
                }
                ServerBootState::Failed(err) => (
                    "ERROR",
                    "ERROR",
                    err.clone(),
                    err.clone(),
                    false,
                ),
                ServerBootState::Ready(_) => (
                    "OPERATIONAL",
                    "READY",
                    String::new(),
                    String::new(),
                    false,
                ),
            }
        } else {
            (
                "RECOVERING_DB",
                "RECOVERING_DB",
                "Inicializando almacenamiento...".to_string(),
                "Iniciando motor...".to_string(),
                true,
            )
        };

    let (storage_mode, data_dir) = ctx
        .machine_paths
        .read()
        .ok()
        .and_then(|g| {
            g.as_ref().map(|mp| {
                (
                    mp.mode_label.clone(),
                    mp.data_root.to_string_lossy().to_string(),
                )
            })
        })
        .unwrap_or_else(|| ("STARTING".to_string(), ".".to_string()));

    let soffice_path = find_soffice_exe();
    json!({
        "status": status_label,
        "mft_armed": true,
        "machine_name": ctx.machine_name,
        "storage_mode": storage_mode,
        "data_dir": data_dir,
        "redb_records": 0,
        "tracked_files": 0,
        "vector_count": 0,
        "running_in_background": running_bg,
        "phase": phase_label,
        "percent": 0.0,
        "target_index": 0,
        "total_targets": 0,
        "current_target": current_target,
        "current_file": current_file,
        "target_processed": 0,
        "target_total": 0,
        "new_files": 0,
        "updated_files": 0,
        "moved_files": 0,
        "deleted_files": 0,
        "skipped_files": 0,
        "last_checkpoint": current_file,
        "last_sync_ms": now_ms_u64(),
        "initial_scan_completed": false,
        "drives": [],
        "sync_interval_minutes": ctx.site_config.config.sync_interval_minutes,
        "office_pdf_available": soffice_path.is_some(),
        "libreoffice_installed": soffice_path.is_some(),
        "libreoffice_first_run_checked": false,
        "libreoffice_path": soffice_path.map(|p| p.to_string_lossy().to_string()),
        "lan_enabled": cfg!(feature = "lan"),
        "bind_lan": ctx.bind_lan,
        "lan_mode": ctx.site_config.config.lan_mode.as_str(),
        "authorized_subnet": ctx.site_config.config.authorized_subnet,
        "shared_server_active": is_active_shared_server,
        "shared_paths_count": ctx.site_config.canonical_shared_roots.len(),
        "client_is_local": peer_is_loopback,
        "port": ctx.listen_port,
        "portable_edition": cfg!(not(feature = "lan")),
        "version": if cfg!(feature = "lan") { format!("{}-MFT", APP_VERSION) } else { format!("{}-PORTABLE", APP_VERSION) }
    })
}

fn build_status_json(
    state: &SharedRadarState,
    ctx: &RuntimeServerContext,
    peer_is_loopback: bool,
    is_active_shared_server: bool,
) -> serde_json::Value {
    let records_count = state.storage.count_records().unwrap_or(0);
    let tracked_files = state.storage.count_index_states().unwrap_or(0);
    let vectors_count = state.vector_store.read().map(|vs| vs.len()).unwrap_or(0);
    let telemetry = state.telemetry.read().map(|t| t.clone()).unwrap_or_default();
    let running_in_background = ctx.sync_running.load(Ordering::SeqCst);

    let soffice_path = find_soffice_exe();
    let lo_checked = state
        .storage
        .get_setting(LIBREOFFICE_FIRST_RUN_CHECKED_KEY)
        .ok()
        .flatten()
        .is_some();

    let (storage_mode, data_dir) = ctx
        .machine_paths
        .read()
        .ok()
        .and_then(|g| {
            g.as_ref().map(|mp| {
                (
                    mp.mode_label.clone(),
                    mp.data_root.to_string_lossy().to_string(),
                )
            })
        })
        .unwrap_or_else(|| ("LOCAL_PORTABLE".to_string(), ".".to_string()));

    json!({
        "status": "OPERATIONAL",
        "mft_armed": true,
        "machine_name": ctx.machine_name,
        "storage_mode": storage_mode,
        "data_dir": data_dir,
        "redb_records": records_count,
        "tracked_files": tracked_files,
        "vector_count": vectors_count,
        "running_in_background": running_in_background,
        "phase": telemetry.phase,
        "percent": telemetry.percent,
        "target_index": telemetry.target_index,
        "total_targets": telemetry.total_targets,
        "current_target": telemetry.current_target,
        "current_file": telemetry.current_file,
        "target_processed": telemetry.target_processed,
        "target_total": telemetry.target_total,
        "new_files": telemetry.new_files,
        "updated_files": telemetry.updated_files,
        "moved_files": telemetry.moved_files,
        "deleted_files": telemetry.deleted_files,
        "skipped_files": telemetry.skipped_files,
        "last_checkpoint": telemetry.last_checkpoint,
        "last_sync_ms": telemetry.last_sync_ms,
        "initial_scan_completed": telemetry.initial_scan_completed,
        "drives": telemetry.drives,
        "sync_interval_minutes": ctx.site_config.config.sync_interval_minutes,
        "office_pdf_available": soffice_path.is_some(),
        "libreoffice_installed": soffice_path.is_some(),
        "libreoffice_first_run_checked": lo_checked,
        "libreoffice_path": soffice_path.map(|p| p.to_string_lossy().to_string()),
        "lan_enabled": cfg!(feature = "lan"),
        "bind_lan": ctx.bind_lan,
        "lan_mode": ctx.site_config.config.lan_mode.as_str(),
        "authorized_subnet": ctx.site_config.config.authorized_subnet,
        "shared_server_active": is_active_shared_server,
        "shared_paths_count": ctx.site_config.canonical_shared_roots.len(),
        "client_is_local": peer_is_loopback,
        "port": ctx.listen_port,
        "portable_edition": cfg!(not(feature = "lan")),
        "version": if cfg!(feature = "lan") { format!("{}-MFT", APP_VERSION) } else { format!("{}-PORTABLE", APP_VERSION) }
    })
}

async fn handle_connection(
    mut socket: TcpStream,
    ctx: Arc<RuntimeServerContext>,
) -> Result<()> {
    let peer_ip = socket.peer_addr().ok().map(|addr| addr.ip());
    let peer_is_loopback = peer_ip.map(|ip| ip.is_loopback()).unwrap_or(false);

    if let Some(ip) = peer_ip {
        if !ctx.site_config.is_peer_ip_authorized(ip) {
            let err = json!({
                "error": "Acceso rechazado: IP fuera de la subred LAN autorizada",
                "authorized_subnet": ctx.site_config.config.authorized_subnet
            });
            send_response(
                &mut socket,
                "403 Forbidden",
                "application/json; charset=utf-8",
                err.to_string().as_bytes(),
            )
            .await?;
            return Ok(());
        }
    }

    let mut buffer = [0u8; 4096];
    let bytes_read = socket.read(&mut buffer).await?;
    if bytes_read == 0 {
        return Ok(());
    }

    let req_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let mut lines = req_str.lines();
    let first_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        return Ok(());
    }

    #[cfg(feature = "lan")]
    let mut host_header = format!("127.0.0.1:{}", ctx.listen_port);
    #[cfg(feature = "lan")]
    let mut forwarded_proto = String::from("http");
    let mut range_header: Option<String> = None;
    for line in lines {
        if let Some(_rest) = line.strip_prefix("Host:").or_else(|| line.strip_prefix("host:")) {
            #[cfg(feature = "lan")]
            {
                let trimmed = _rest.trim();
                if !trimmed.is_empty() {
                    host_header = trimmed.to_string();
                }
            }
        } else if let Some(_rest) = line
            .strip_prefix("X-Forwarded-Proto:")
            .or_else(|| line.strip_prefix("x-forwarded-proto:"))
        {
            #[cfg(feature = "lan")]
            {
                let proto = _rest.trim().to_ascii_lowercase();
                if proto == "https" || proto == "http" {
                    forwarded_proto = proto;
                }
            }
        } else if let Some(rest) = line.strip_prefix("Range:").or_else(|| line.strip_prefix("range:")) {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                range_header = Some(trimmed.to_string());
            }
        }
    }

    let method = parts[0];
    let full_path = parts[1];
    let (path, query_params) = parse_url(full_path);

    if method == "OPTIONS" {
        send_preflight_ok(&mut socket).await?;
        return Ok(());
    }

    let is_active_shared_server = ctx.bind_lan && ctx.site_config.is_shared_server();

    match (method, path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let content = include_str!("../../../ui/index.html");
            send_response(&mut socket, "200 OK", "text/html; charset=utf-8", content.as_bytes()).await?;
        }
        ("GET", "/styles.css") => {
            let content = include_str!("../../../ui/styles.css");
            send_response(&mut socket, "200 OK", "text/css; charset=utf-8", content.as_bytes()).await?;
        }
        ("GET", "/radar.js") => {
            let content = include_str!("../../../ui/radar.js");
            send_response(&mut socket, "200 OK", "application/javascript; charset=utf-8", content.as_bytes()).await?;
        }
        ("GET", "/health") | ("GET", "/api/server-info") => {
            let db_ready = get_ready_state(&ctx).is_some();
            let res = json!({
                "app": "dragon-radar",
                "status": "ok",
                "db_ready": db_ready,
                "version": APP_VERSION,
                "port": ctx.listen_port,
                "bind_lan": ctx.bind_lan,
                "lan_enabled": cfg!(feature = "lan"),
                "portable_edition": cfg!(not(feature = "lan")),
                "lan_mode": ctx.site_config.config.lan_mode.as_str(),
                "authorized_subnet": ctx.site_config.config.authorized_subnet,
                "shared_server_active": is_active_shared_server,
                "shared_paths_count": ctx.site_config.canonical_shared_roots.len(),
                "client_is_local": peer_is_loopback,
                "machine_name": ctx.machine_name
            });
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/index/stream") => {
            if !can_access_data_api(peer_is_loopback, &ctx) {
                let err = json!({
                    "error": "Modo LAN local_agent activo: los clientes deben usar su agente local en 127.0.0.1",
                    "lan_mode": "local_agent"
                });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }
            let headers = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream; charset=utf-8\r\n\
                Cache-Control: no-store, no-cache, must-revalidate\r\n\
                X-Content-Type-Options: nosniff\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Private-Network: true\r\n\
                Connection: keep-alive\r\n\r\n";
            if socket.write_all(headers.as_bytes()).await.is_err() {
                return Ok(());
            }
            let _ = socket.flush().await;

            loop {
                let payload = build_boot_or_status_json(&ctx, peer_is_loopback, is_active_shared_server);
                let is_active = ctx.sync_running.load(Ordering::SeqCst)
                    || payload.get("phase").and_then(|v| v.as_str()).unwrap_or("READY") != "READY";
                let frame = format!("data: {}\n\n", payload);
                if socket.write_all(frame.as_bytes()).await.is_err() {
                    break;
                }
                if socket.flush().await.is_err() {
                    break;
                }
                let sleep_ms = if is_active { 500 } else { 2500 };
                tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
            }
            return Ok(());
        }
        ("GET", "/api/index/validate") | ("POST", "/api/index/validate") => {
            let Some(state) = get_ready_state(&ctx) else {
                let res = json!({
                    "app": "dragon-radar",
                    "complete": false,
                    "running_in_background": true,
                    "resumed_just_now": false,
                    "close_console_windows": false,
                    "phase": "RECOVERING_DB",
                    "percent": 0.0,
                    "current_target": "Abriendo / recuperando radar.redb...",
                    "current_file": "Recuperando base de datos en segundo plano...",
                    "machine_name": ctx.machine_name,
                    "port": ctx.listen_port
                });
                send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
                return Ok(());
            };
            let cp = state.storage.load_checkpoint().unwrap_or_default();
            let telemetry = state.telemetry.read().map(|t| t.clone()).unwrap_or_default();
            let mut is_running = ctx.sync_running.load(Ordering::SeqCst);

            let total_targets = if cp.targets.is_empty() {
                telemetry.total_targets
            } else {
                cp.targets.len()
            };
            let pending_targets = if cp.in_progress && !cp.targets.is_empty() {
                cp.targets.len().saturating_sub(cp.current_target_idx)
            } else if !cp.initial_scan_completed {
                total_targets.max(1)
            } else {
                0
            };

            let missing_drives = !is_active_shared_server
                && telemetry.drives.iter().any(|d| d.status == "MISSING" || d.status == "PENDING");

            let mut is_complete = !is_running
                && cp.initial_scan_completed
                && !cp.in_progress
                && !missing_drives
                && telemetry.phase == "READY";
            let mut resumed_just_now = false;

            // Si la indexación está incompleta o falta alguna unidad y no hay un hilo activo, reanudar automáticamente en segundo plano
            if !is_complete && !is_running {
                resumed_just_now = spawn_background_sync_if_idle(
                    &state,
                    &ctx.sync_running,
                    ctx.shared_roots_for_sync.clone(),
                    ctx.listen_port,
                );
                if resumed_just_now {
                    is_running = true;
                    is_complete = false;
                }
            }

            let trigger_cleanup = query_params
                .get("trigger_script")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            if is_complete && trigger_cleanup {
                trigger_console_cleanup_script_debounced(ctx.listen_port);
            }

            let records_count = state.storage.count_records().unwrap_or(0);
            let tracked_files = state.storage.count_index_states().unwrap_or(0);
            let vectors_count = state.vector_store.read().map(|vs| vs.len()).unwrap_or(0);

            let res = json!({
                "app": "dragon-radar",
                "complete": is_complete,
                "running_in_background": is_running,
                "resumed_just_now": resumed_just_now,
                "close_console_windows": is_complete,
                "phase": telemetry.phase,
                "percent": telemetry.percent,
                "current_target": telemetry.current_target,
                "current_file": telemetry.current_file,
                "target_processed": telemetry.target_processed,
                "target_total": telemetry.target_total,
                "initial_scan_completed": cp.initial_scan_completed,
                "in_progress": cp.in_progress,
                "current_target_idx": cp.current_target_idx,
                "total_targets": total_targets,
                "pending_targets": pending_targets,
                "redb_records": records_count,
                "tracked_files": tracked_files,
                "vector_count": vectors_count,
                "last_checkpoint": cp.last_checkpoint_path.unwrap_or(telemetry.last_checkpoint),
                "last_sync_ms": telemetry.last_sync_ms,
                "drives": telemetry.drives,
                "drives_all_ok": !missing_drives,
                "machine_name": ctx.machine_name,
                "port": ctx.listen_port
            });
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/status") => {
            if !can_access_data_api(peer_is_loopback, &ctx) {
                let err = json!({
                    "error": "Modo LAN local_agent activo: los clientes deben usar su agente local en 127.0.0.1",
                    "lan_mode": "local_agent"
                });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }

            let res = build_boot_or_status_json(&ctx, peer_is_loopback, is_active_shared_server);
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        #[cfg(feature = "lan")]
        ("GET", "/api/agent/launcher") => {
            let server_url = format!("{}://{}", forwarded_proto, host_header);
            let bat = build_client_launcher_bat(&server_url, ctx.listen_port);
            send_download(
                &mut socket,
                "application/octet-stream",
                "Activar_Radar_Local.bat",
                bat.as_bytes(),
            )
            .await?;
        }
        #[cfg(feature = "lan")]
        ("GET", "/api/agent/binary") => {
            if let Ok(exe_path) = std::env::current_exe() {
                if let Ok(bytes) = tokio::fs::read(&exe_path).await {
                    send_download(&mut socket, "application/octet-stream", "dragon.exe", &bytes).await?;
                    return Ok(());
                }
            }
            send_response(&mut socket, "404 Not Found", "text/plain", b"Ejecutable no disponible").await?;
        }
        #[cfg(feature = "lan")]
        ("GET", "/api/agent/libreoffice-info") => {
            let res = if let Some(msi_path) = find_bundled_libreoffice_msi() {
                let filename = msi_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("LibreOffice_26.8.1_Win_x86-64.msi")
                    .to_string();
                let size_bytes = std::fs::metadata(&msi_path).map(|m| m.len()).unwrap_or(0);
                json!({
                    "available": true,
                    "filename": filename,
                    "size_bytes": size_bytes
                })
            } else {
                json!({
                    "available": false,
                    "filename": null,
                    "size_bytes": 0
                })
            };
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        #[cfg(feature = "lan")]
        ("GET", "/api/agent/libreoffice-installer") => {
            if let Some(msi_path) = find_bundled_libreoffice_msi() {
                let msi_str = msi_path.to_string_lossy().to_string();
                serve_download_file(&mut socket, &msi_str).await?;
            } else {
                send_response(
                    &mut socket,
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    b"Instalador de LibreOffice no encontrado en la carpeta del proyecto",
                )
                .await?;
            }
        }
        ("GET", "/api/search") => {
            if !can_access_data_api(peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", b"[]").await?;
                return Ok(());
            }
            let Some(state) = get_ready_state(&ctx) else {
                send_response(&mut socket, "200 OK", "application/json; charset=utf-8", b"[]").await?;
                return Ok(());
            };
            let q = query_params.get("q").cloned().unwrap_or_default();
            let mode = query_params.get("mode").cloned().unwrap_or_else(|| "all".to_string());
            let raw_limit = query_params.get("limit").and_then(|v| v.parse::<usize>().ok());
            let want_all = query_params
                .get("all")
                .map(|v| {
                    let s = v.trim().to_ascii_lowercase();
                    s == "1" || s == "true" || s == "yes" || s == "all"
                })
                .unwrap_or(false)
                || raw_limit == Some(0);
            let limit = if want_all {
                usize::MAX
            } else {
                raw_limit.unwrap_or(50)
            };

            let state_clone = Arc::clone(&state);
            let q_clone = q.clone();
            let mode_clone = mode.clone();
            let mut results = tokio::task::spawn_blocking(move || {
                execute_search(&state_clone, &q_clone, &mode_clone, limit)
            })
            .await
            .unwrap_or_else(|_| json!([]));
            if is_active_shared_server {
                if let Some(arr) = results.as_array_mut() {
                    arr.retain(|item| {
                        item.get("path")
                            .and_then(|v| v.as_str())
                            .map(|p| ctx.site_config.is_indexed_path_in_shared_roots(p))
                            .unwrap_or(false)
                    });
                }
            }
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", results.to_string().as_bytes()).await?;
        }
        ("GET", "/api/extensions") => {
            if !can_access_data_api(peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", br#"{"extensions":[]}"#).await?;
                return Ok(());
            }
            let Some(state) = get_ready_state(&ctx) else {
                send_response(&mut socket, "200 OK", "application/json; charset=utf-8", br#"{"extensions":[]}"#).await?;
                return Ok(());
            };
            if is_active_shared_server && ctx.site_config.canonical_shared_roots.is_empty() {
                send_response(&mut socket, "200 OK", "application/json; charset=utf-8", br#"{"extensions":[]}"#).await?;
                return Ok(());
            }
            let q = query_params
                .get("q")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let limit = query_params
                .get("limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(18)
                .clamp(1, 60);

            let items = state.storage.list_top_extensions(q, limit).unwrap_or_default();
            let res = json!({
                "extensions": items.into_iter().map(|s| json!({
                    "extension": s.extension,
                    "count": s.count,
                    "category": format!("{:?}", s.category),
                })).collect::<Vec<_>>()
            });
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/extract") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                let err = json!({ "success": false, "error": "Acceso denegado fuera de las carpetas compartidas autorizadas" });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }
            let p = Path::new(&file);
            let res = if p.exists() {
                let fmt = detect_format(p);
                match extract_content(p, 10_000) {
                    Ok(text) => json!({
                        "success": true,
                        "file": file,
                        "format": format!("{:?}", fmt),
                        "content": text
                    }),
                    Err(e) => json!({
                        "success": false,
                        "error": e.to_string()
                    }),
                }
            } else {
                json!({ "success": false, "error": "Archivo no encontrado" })
            };
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/preview") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                let err = json!({ "success": false, "error": "Acceso denegado fuera de las carpetas compartidas autorizadas" });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }
            let file_clone = file.clone();
            let res = tokio::task::spawn_blocking(move || build_preview_payload(&file_clone))
                .await
                .unwrap_or_else(|e| json!({ "success": false, "error": e.to_string() }));
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/raw") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "text/plain; charset=utf-8", b"Acceso denegado").await?;
                return Ok(());
            }
            serve_raw_file(&mut socket, &file, range_header.as_deref()).await?;
        }
        ("GET", "/api/download") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "text/plain; charset=utf-8", b"Acceso denegado").await?;
                return Ok(());
            }
            serve_download_file(&mut socket, &file).await?;
        }
        ("GET", "/api/zip-asset") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "text/plain; charset=utf-8", b"Acceso denegado").await?;
                return Ok(());
            }
            let entry = query_params.get("entry").cloned().unwrap_or_default();
            serve_zip_asset(&mut socket, &file, &entry).await?;
        }
        ("GET", "/api/office-pdf") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            if !can_access_file(&file, peer_is_loopback, &ctx) {
                send_response(&mut socket, "403 Forbidden", "text/plain; charset=utf-8", b"Acceso denegado").await?;
                return Ok(());
            }
            serve_office_pdf(&mut socket, &file, range_header.as_deref()).await?;
        }
        ("POST", "/api/shutdown") => {
            if !peer_is_loopback {
                let err = json!({ "success": false, "error": "Apagado controlado solo permitido desde 127.0.0.1" });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }
            let ok = json!({ "success": true, "message": "Apagado limpio iniciado" });
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", ok.to_string().as_bytes()).await?;
            ctx.shutdown_notify.notify_waiters();
        }
        ("GET", "/api/open") => {
            if !peer_is_loopback {
                let err = json!({
                    "success": false,
                    "remote": true,
                    "error": "Abrir en Explorador solo aplica en el equipo local. Use el botón Descargar."
                });
                send_response(&mut socket, "403 Forbidden", "application/json; charset=utf-8", err.to_string().as_bytes()).await?;
                return Ok(());
            }
            let file = query_params.get("file").cloned().unwrap_or_default();
            let res = open_in_explorer(&file);
            send_response(&mut socket, "200 OK", "application/json; charset=utf-8", res.to_string().as_bytes()).await?;
        }
        _ => {
            send_response(&mut socket, "404 Not Found", "text/plain", b"Ruta no encontrada").await?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Default)]
struct ParsedExtensionQuery {
    extensions: Vec<String>,
    text_tokens: Vec<String>,
    explicit_extension_filter: bool,
}

fn is_valid_ext_chars(s: &str) -> bool {
    !s.is_empty() && s.len() <= 14 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_')
}

fn push_unique_ext(out: &mut Vec<String>, raw: &str) {
    let clean = raw
        .trim()
        .trim_start_matches('*')
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if is_valid_ext_chars(&clean) && !out.contains(&clean) {
        out.push(clean);
    }
}

fn parse_search_extensions(
    storage: &dragon_storage::DragonStorage,
    query: &str,
    extension_mode: bool,
) -> ParsedExtensionQuery {
    let mut parsed = ParsedExtensionQuery::default();
    let raw_parts: Vec<&str> = query.split_whitespace().collect();

    for part in raw_parts {
        let lower = part.to_ascii_lowercase();
        let explicit_rhs = lower
            .strip_prefix("ext:")
            .or_else(|| lower.strip_prefix("extension:"))
            .or_else(|| lower.strip_prefix("tipo:"));

        if let Some(rhs) = explicit_rhs {
            parsed.explicit_extension_filter = true;
            for piece in rhs.split([',', ';']) {
                push_unique_ext(&mut parsed.extensions, piece);
            }
            continue;
        }

        if part.starts_with("*.") || (part.starts_with('.') && part.len() > 1) {
            let mut added = false;
            for piece in part.split([',', ';']) {
                let clean = piece.trim().trim_start_matches('*').trim_start_matches('.');
                if is_valid_ext_chars(clean) {
                    push_unique_ext(&mut parsed.extensions, clean);
                    added = true;
                }
            }
            if added {
                parsed.explicit_extension_filter = true;
                continue;
            }
        }

        if extension_mode {
            let has_list_sep = part.contains(',') || part.contains(';');
            for piece in part.split([',', ';']).filter(|s| !s.trim().is_empty()) {
                let clean = piece
                    .trim()
                    .trim_start_matches('*')
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                let is_ext_candidate = is_valid_ext_chars(&clean)
                    && (has_list_sep
                        || parsed.extensions.is_empty()
                        || storage.extension_exists(&clean).unwrap_or(false));
                if is_ext_candidate {
                    push_unique_ext(&mut parsed.extensions, &clean);
                } else {
                    parsed.text_tokens.push(piece.trim().to_string());
                }
            }
        } else {
            parsed.text_tokens.push(part.to_string());
        }
    }

    parsed
}

fn path_matches_extensions(path: &str, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    extract_extension_lower(path)
        .map(|ext| extensions.iter().any(|e| e == &ext))
        .unwrap_or(false)
}

fn execute_extension_search_items(
    state: &SharedRadarState,
    parsed: &ParsedExtensionQuery,
    category_filter: Option<FileCategory>,
    allow_prefix_fallback: bool,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    if let Ok(records) = state.storage.search_by_extension(
        &parsed.extensions,
        &parsed.text_tokens,
        category_filter,
        allow_prefix_fallback,
        limit,
    ) {
        for (rec, score) in records {
            let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let badge = determine_badge(&path_str);
            let ext_upper = extract_extension_lower(&rec.name)
                .unwrap_or_else(|| badge.to_string())
                .to_ascii_uppercase();
            items.push(json!({
                "name": rec.name,
                "path": path_str,
                "badge": badge,
                "size": format_file_size(rec.size),
                "score": format!("{:.0} (.{})", score, ext_upper),
                "snippet": format!("Indexado por extensión (.{}) · {} ({:?})", ext_upper, format_file_size(rec.size), classify_path(&path_str))
            }));
        }
    }
    items
}

fn execute_search(state: &SharedRadarState, query: &str, mode: &str, limit: usize) -> serde_json::Value {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return json!([]);
    }

    let parsed = parse_search_extensions(&state.storage, trimmed, mode == "extension");
    let mut output_items = Vec::new();

    match mode {
        "extension" => {
            output_items = execute_extension_search_items(state, &parsed, None, true, limit);
        }
        "filename" => {
            if parsed.explicit_extension_filter {
                output_items = execute_extension_search_items(state, &parsed, None, false, limit);
            } else {
                let ranked = execute_name_or_extension_category(state, trimmed, &parsed, None, "Nombre", limit);
                return json!(ranked);
            }
        }
        "media" => {
            if parsed.explicit_extension_filter {
                output_items = execute_extension_search_items(state, &parsed, Some(FileCategory::Media), false, limit);
            } else {
                let ranked = execute_name_or_extension_category(state, trimmed, &parsed, Some(FileCategory::Media), "Media", limit);
                return json!(ranked);
            }
        }
        "design" => {
            if parsed.explicit_extension_filter {
                output_items = execute_extension_search_items(state, &parsed, Some(FileCategory::ImageOrDesign), false, limit);
            } else {
                let ranked = execute_name_or_extension_category(state, trimmed, &parsed, Some(FileCategory::ImageOrDesign), "Diseño", limit);
                return json!(ranked);
            }
        }
        "content" => {
            if parsed.explicit_extension_filter && parsed.text_tokens.is_empty() {
                output_items = execute_extension_search_items(state, &parsed, Some(FileCategory::Document), false, limit);
            } else {
                let effective_q = if parsed.explicit_extension_filter {
                    parsed.text_tokens.join(" ")
                } else {
                    trimmed.to_string()
                };
                let fetch_limit = if parsed.explicit_extension_filter {
                    limit.saturating_mul(4)
                } else {
                    limit
                };
                if let Ok(hits) = state.index_engine.search_content(&effective_q, fetch_limit) {
                    let filtered_hits: Vec<_> = hits
                        .into_iter()
                        .filter(|hit| !parsed.explicit_extension_filter || path_matches_extensions(&hit.path, &parsed.extensions))
                        .take(limit)
                        .collect();
                    const MAX_DISK_SNIPPETS: usize = 8;
                    output_items = filtered_hits
                        .into_par_iter()
                        .enumerate()
                        .map(|(idx, hit)| {
                            let real_size = if hit.size > 0 {
                                hit.size
                            } else {
                                std::fs::metadata(&hit.path).map(|m| m.len()).unwrap_or(0)
                            };
                            let snippet = extract_matching_snippet_fast(
                                state,
                                &hit.path,
                                &effective_q,
                                165,
                                idx < MAX_DISK_SNIPPETS,
                            )
                            .unwrap_or_else(|| format!("Coincidencia en contenido (BM25 {:.2})", hit.score));
                            let badge = determine_badge(&hit.path);
                            json!({
                                "name": hit.filename,
                                "path": hit.path,
                                "badge": badge,
                                "size": format_file_size(real_size),
                                "score": format!("{:.2}", hit.score),
                                "snippet": snippet
                            })
                        })
                        .collect();
                }
            }
        }
        "semantic" => {
            let q_vec = state.embedder.embed(trimmed);
            if let Ok(vs) = state.vector_store.read() {
                let hits = vs.search(&q_vec, limit.saturating_mul(2));
                for hit in hits {
                    if classify_path(&hit.path) != FileCategory::Document || hit.score < 0.22 {
                        continue;
                    }
                    let filename = Path::new(&hit.path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| hit.path.clone());
                    let badge = determine_badge(&hit.path);
                    output_items.push(json!({
                        "name": filename,
                        "path": hit.path,
                        "badge": badge,
                        "size": "Vector",
                        "score": format!("{:.1}%", hit.score * 100.0),
                        "snippet": hit.snippet
                    }));
                    if output_items.len() >= limit {
                        break;
                    }
                }
            }
        }
        "documents" => {
            // Documentos conocidos por nombre, extensión o contenido + semántica de alta confianza
            let ranked = execute_unified_ranked(state, trimmed, &parsed, Some(FileCategory::Document), limit);
            return json!(ranked);
        }
        _ => {
            // Modo "all" (Híbrido):
            // - Documentos por nombre, extensión o contenido + semántica filtrada
            // - Multimedia e Imágenes/Diseño por nombre o extensión
            // - Sistema/Binarios por nombre o extensión únicamente (menor prioridad que archivos de usuario)
            let ranked = execute_unified_ranked(state, trimmed, &parsed, None, limit);
            return json!(ranked);
        }
    }

    output_items.truncate(limit);
    json!(output_items)
}

fn execute_name_or_extension_category(
    state: &SharedRadarState,
    query: &str,
    parsed: &ParsedExtensionQuery,
    category_filter: Option<FileCategory>,
    label: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    struct HitItem {
        name: String,
        path: String,
        size: u64,
        score: f32,
        snippet: String,
    }

    let mut by_path: HashMap<String, HitItem> = HashMap::new();
    let norm_key = |p: &str| if cfg!(windows) { p.to_lowercase() } else { p.to_string() };

    if let Ok(records) = state.storage.search_by_name_ranked(query, category_filter, limit.saturating_mul(2)) {
        for (rec, score) in records {
            let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let key = norm_key(&path_str);
            by_path.insert(
                key,
                HitItem {
                    name: rec.name,
                    path: path_str.clone(),
                    size: rec.size,
                    score,
                    snippet: format!(
                        "Coincidencia por nombre ({:?}) · {}",
                        classify_path(&path_str),
                        format_file_size(rec.size)
                    ),
                },
            );
        }
    }

    if !parsed.explicit_extension_filter && parsed.text_tokens.len() == 1 {
        let bare = parsed.text_tokens[0].trim().to_ascii_lowercase();
        if is_valid_ext_chars(&bare) && state.storage.extension_exists(&bare).unwrap_or(false) {
            if let Ok(ext_recs) = state.storage.search_by_extension(
                &[bare.clone()],
                &[],
                category_filter,
                false,
                limit,
            ) {
                let ext_upper = bare.to_ascii_uppercase();
                for (rec, ext_score) in ext_recs {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let key = norm_key(&path_str);
                    let adjusted = (ext_score * 0.45).max(120.0);
                    by_path
                        .entry(key)
                        .and_modify(|item| {
                            item.score += 180.0;
                            item.snippet = format!("Nombre + Extensión (.{}) · {}", ext_upper, format_file_size(rec.size));
                        })
                        .or_insert_with(|| HitItem {
                            name: rec.name,
                            path: path_str,
                            size: rec.size,
                            score: adjusted,
                            snippet: format!("Indexado por extensión (.{}) · {}", ext_upper, format_file_size(rec.size)),
                        });
                }
            }
        }
    }

    let mut hits: Vec<HitItem> = by_path.into_values().collect();
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    hits.into_iter()
        .map(|h| {
            let badge = determine_badge(&h.path);
            json!({
                "name": h.name,
                "path": h.path,
                "badge": badge,
                "size": format_file_size(h.size),
                "score": format!("{:.0} ({})", h.score, label),
                "snippet": h.snippet
            })
        })
        .collect()
}

/// Motor de ranking unificado de alta precisión combinando coincidencia exacta de nombre, índice de extensión, BM25 de contenido y vectores confiables.
fn execute_unified_ranked(
    state: &SharedRadarState,
    query: &str,
    parsed: &ParsedExtensionQuery,
    category_filter: Option<FileCategory>,
    limit: usize,
) -> Vec<serde_json::Value> {
    struct MergedCandidate {
        name: String,
        path: String,
        size_label: String,
        score: f32,
        snippet: String,
    }

    let mut by_path: HashMap<String, MergedCandidate> = HashMap::new();
    let norm_key = |p: &str| {
        if cfg!(windows) { p.to_lowercase() } else { p.to_string() }
    };

    if parsed.explicit_extension_filter {
        // 1A. Filtro explícito de extensión (.pdf, *.xlsx, ext:docx) -> consulta directa al índice B-Tree
        if let Ok(ext_hits) = state.storage.search_by_extension(
            &parsed.extensions,
            &parsed.text_tokens,
            category_filter,
            false,
            limit.saturating_mul(2),
        ) {
            for (rec, ext_score) in ext_hits {
                let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let key = norm_key(&path_str);
                let ext_upper = extract_extension_lower(&rec.name)
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let cat = classify_path(&path_str);
                by_path.insert(
                    key,
                    MergedCandidate {
                        name: rec.name,
                        path: path_str,
                        size_label: format_file_size(rec.size),
                        score: ext_score,
                        snippet: format!("Indexado por extensión (.{}) · {:?} · {}", ext_upper, cat, format_file_size(rec.size)),
                    },
                );
            }
        }
    } else {
        // 1B. Coincidencias por nombre en Redb (ordenadas por precisión)
        if let Ok(name_hits) = state.storage.search_by_name_ranked(query, category_filter, limit.saturating_mul(2)) {
            for (rec, name_score) in name_hits {
                let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                let key = norm_key(&path_str);
                let cat = classify_path(&path_str);
                by_path.insert(
                    key,
                    MergedCandidate {
                        name: rec.name,
                        path: path_str,
                        size_label: format_file_size(rec.size),
                        score: name_score,
                        snippet: format!("Coincidencia por nombre ({:?}) · {}", cat, format_file_size(rec.size)),
                    },
                );
            }
        }

        // 1C. Si la consulta es un único token de extensión existente (ej: "pdf", "xlsx", "dwg"), también sumar el índice de extensiones
        if parsed.text_tokens.len() == 1 {
            let bare = parsed.text_tokens[0].trim().to_ascii_lowercase();
            if is_valid_ext_chars(&bare) && state.storage.extension_exists(&bare).unwrap_or(false) {
                if let Ok(ext_hits) = state.storage.search_by_extension(
                    &[bare.clone()],
                    &[],
                    category_filter,
                    false,
                    limit.saturating_mul(2),
                ) {
                    let ext_upper = bare.to_ascii_uppercase();
                    for (rec, ext_score) in ext_hits {
                        let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                        let key = norm_key(&path_str);
                        let merged_score = (ext_score * 0.48).max(140.0);
                        by_path
                            .entry(key)
                            .and_modify(|c| {
                                c.score += 180.0;
                                c.snippet = format!("Nombre + Extensión (.{}) · {}", ext_upper, format_file_size(rec.size));
                            })
                            .or_insert_with(|| MergedCandidate {
                                name: rec.name,
                                path: path_str,
                                size_label: format_file_size(rec.size),
                                score: merged_score,
                                snippet: format!("Indexado por extensión (.{}) · {}", ext_upper, format_file_size(rec.size)),
                            });
                    }
                }
            }
        }
    }

    // 2. Búsqueda de contenido BM25 (solo aplica si no se filtró a Multimedia/Diseño, y si hay texto cuando hay filtro de extensión)
    let text_query = if parsed.explicit_extension_filter {
        parsed.text_tokens.join(" ")
    } else {
        query.to_string()
    };
    let allow_content = matches!(category_filter, None | Some(FileCategory::Document))
        && !text_query.trim().is_empty();
    if allow_content {
        let fetch_limit = if parsed.explicit_extension_filter {
            limit.saturating_mul(4)
        } else {
            limit.saturating_mul(2)
        };
        if let Ok(bm25_hits) = state.index_engine.search_content(&text_query, fetch_limit) {
            for hit in bm25_hits {
                if parsed.explicit_extension_filter && !path_matches_extensions(&hit.path, &parsed.extensions) {
                    continue;
                }
                let key = norm_key(&hit.path);
                let bm25_boost = (hit.score * 25.0).min(450.0);
                let real_size = if hit.size > 0 {
                    hit.size
                } else {
                    std::fs::metadata(&hit.path).map(|m| m.len()).unwrap_or(0)
                };
                by_path
                    .entry(key)
                    .and_modify(|c| {
                        c.score += bm25_boost;
                        c.snippet = format!("Nombre + Contenido BM25 (Relevancia {:.2})", hit.score);
                    })
                    .or_insert_with(|| MergedCandidate {
                        name: hit.filename,
                        path: hit.path,
                        size_label: format_file_size(real_size),
                        score: 140.0 + bm25_boost,
                        snippet: format!("Contenido BM25 (Relevancia {:.2})", hit.score),
                    });
            }
        }

        // 3. Búsqueda semántica solo cuando nombre + contenido BM25 arrojaron pocos resultados (< 10),
        //    evitando escaneo vectorial innecesario de 100k+ vectores y ruido semántico en consultas exactas.
        if by_path.len() < 10 {
            let q_vec = state.embedder.embed(&text_query);
            if let Ok(vs) = state.vector_store.read() {
                let sem_limit = limit.min(40);
                let sem_hits = vs.search(&q_vec, sem_limit);
                for hit in sem_hits {
                    if hit.score < 0.34 || classify_path(&hit.path) != FileCategory::Document {
                        continue;
                    }
                    if parsed.explicit_extension_filter && !path_matches_extensions(&hit.path, &parsed.extensions) {
                        continue;
                    }
                    if is_noisy_build_or_cache_path(&hit.path) {
                        continue;
                    }
                    let key = norm_key(&hit.path);
                    let sem_boost = hit.score * 160.0;
                    let filename = Path::new(&hit.path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| hit.path.clone());

                    by_path
                        .entry(key)
                        .and_modify(|c| {
                            c.score += sem_boost;
                            if !hit.snippet.is_empty() {
                                c.snippet = hit.snippet.clone();
                            }
                        })
                        .or_insert_with(|| MergedCandidate {
                            name: filename,
                            path: hit.path,
                            size_label: "Semántico".to_string(),
                            score: sem_boost,
                            snippet: hit.snippet,
                        });
                }
            }
        }
    }

    let mut candidates: Vec<MergedCandidate> = by_path.into_values().collect();
    candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(limit);

    // Enriquecer extractos de contenido usando primero RAM (0 ms) y solo disco ligero (<= 2 MB) para los top 8
    if allow_content {
        const MAX_RAM_SNIPPETS: usize = 60;
        const MAX_DISK_SNIPPETS: usize = 8;
        let snippet_slice_len = candidates.len().min(MAX_RAM_SNIPPETS);
        candidates[..snippet_slice_len]
            .par_iter_mut()
            .enumerate()
            .for_each(|(idx, c)| {
                if c.snippet.contains("BM25") {
                    if let Some(real_snip) = extract_matching_snippet_fast(
                        state,
                        &c.path,
                        &text_query,
                        165,
                        idx < MAX_DISK_SNIPPETS,
                    ) {
                        c.snippet = real_snip;
                    }
                }
            });
    }

    candidates
        .into_iter()
        .map(|c| {
            let badge = determine_badge(&c.path);
            json!({
                "name": c.name,
                "path": c.path,
                "badge": badge,
                "size": c.size_label,
                "score": format!("{:.1}", c.score),
                "snippet": c.snippet
            })
        })
        .collect()
}

/// Extrae un fragmento textual alrededor de la consulta priorizando caché en RAM (`VectorStore`, 0 ms)
/// y limitando lecturas de disco a archivos livianos (<= 2 MB) para mantener latencias < 100 ms.
fn extract_matching_snippet_fast(
    state: &SharedRadarState,
    path: &str,
    query: &str,
    max_chars: usize,
    allow_disk_fallback: bool,
) -> Option<String> {
    // 1. Intento instantáneo desde RAM (sin abrir el archivo en disco)
    let ram_text = state
        .vector_store
        .read()
        .ok()
        .and_then(|vs| vs.get_snippet_by_path(path).map(|s| s.to_string()));
    if let Some(ref cached) = ram_text {
        if let Some(snip) = build_snippet_from_text(cached, query, max_chars) {
            return Some(snip);
        }
    }

    // 2. Respaldo en disco acotado: jamás parsear PDFs/Excels pesados (> 2 MB) durante cada tecleo
    if allow_disk_fallback {
        const MAX_LIVE_SNIPPET_FILE_BYTES: u64 = 2 * 1024 * 1024;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() <= MAX_LIVE_SNIPPET_FILE_BYTES {
                let p = Path::new(path);
                let disk_text = if detect_format(p) == DocumentFormat::Pdf {
                    extract_pdf_preview(p, 10, 32_000).ok().map(|pr| pr.text)
                } else {
                    extract_content(p, 32_000).ok()
                };
                if let Some(ref text) = disk_text {
                    if let Some(snip) = build_snippet_from_text(text, query, max_chars) {
                        return Some(snip);
                    }
                }
            }
        }
    }

    // 3. Si había texto en RAM aunque la coincidencia esté más adelante en el documento, mostrar encabezado limpio
    if let Some(cached) = ram_text {
        let compact = cached.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            let head: String = compact.chars().take(max_chars).collect();
            return Some(if compact.chars().count() > max_chars {
                format!("{}...", head)
            } else {
                head
            });
        }
    }
    None
}

/// Construye un fragmento contextual alrededor de la consulta dentro de un texto ya disponible.
fn build_snippet_from_text(text: &str, query: &str, max_chars: usize) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }

    let orig_chars: Vec<(usize, char)> = text.char_indices().collect();
    if orig_chars.is_empty() {
        return None;
    }
    let folded: Vec<char> = orig_chars.iter().map(|&(_, c)| fold_char_lower(c)).collect();

    let query_tokens: Vec<Vec<char>> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().map(fold_char_lower).collect())
        .collect();

    if query_tokens.is_empty() {
        return None;
    }

    let full_needle: Vec<char> = query.trim().chars().map(fold_char_lower).collect();
    let match_pos = find_subslice(&folded, &full_needle).or_else(|| {
        query_tokens
            .iter()
            .filter_map(|tok| find_subslice(&folded, tok))
            .min()
    })?;

    let before = 42usize;
    let start_c = match_pos.saturating_sub(before);
    let end_c = (start_c + max_chars).min(orig_chars.len());
    let start_b = orig_chars[start_c].0;
    let end_b = orig_chars.get(end_c).map(|x| x.0).unwrap_or(text.len());

    let compact = text[start_b..end_b]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(compact.len() + 8);
    if start_c > 0 {
        out.push_str("...");
    }
    out.push_str(&compact);
    if end_c < orig_chars.len() {
        out.push_str("...");
    }
    Some(out)
}

fn find_subslice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn fold_char_lower(c: char) -> char {
    let lc = c.to_lowercase().next().unwrap_or(c);
    match lc {
        'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        _ => lc,
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let exp = (bytes as f64).log(1024.0).floor() as usize;
    let exp = exp.min(UNITS.len() - 1);
    let val = bytes as f64 / 1024_f64.powi(exp as i32);
    format!("{:.1} {}", val, UNITS[exp])
}

fn determine_badge(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "pdf" => "PDF",
        "doc" | "docx" | "docm" | "dotx" | "odt" | "rtf" => "WORD",
        "xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv" | "tsv" => "EXCEL",
        "ppt" | "pptx" | "pptm" | "ppsx" | "odp" => "PPTX",
        "epub" | "mobi" | "azw3" | "djvu" => "EBOOK",
        "md" | "txt" | "log" | "rst" => "DOC",
        "rs" => "RUST",
        "js" | "ts" | "jsx" | "tsx" => "JS",
        "py" | "go" | "c" | "cpp" | "java" | "cs" | "php" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "sql" => "CODE",
        "svg" | "ai" | "eps" | "cdr" | "cmx" => "VECTOR",
        "psd" | "psb" | "xcf" | "afdesign" | "afphoto" | "fig" | "sketch" | "xd" | "indd" | "kra" => "DESIGN",
        "dwg" | "dxf" | "dwf" | "step" | "stp" | "stl" | "obj" | "fbx" | "glb" | "blend" | "skp" | "rvt" => "CAD/3D",
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw" | "dng" | "heic" => "IMG",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "flv" | "m4v" => "VIDEO",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "opus" | "wma" | "m4a" | "aiff" => "AUDIO",
        "exe" | "dll" | "sys" | "msi" | "iso" | "bin" | "dat" => "SYS/BIN",
        _ => match dragon_core::classify_extension(&ext) {
            FileCategory::Document => "DOC",
            FileCategory::Media => "MEDIA",
            FileCategory::ImageOrDesign => "DESIGN",
            FileCategory::SystemOrBinary => "BIN",
        },
    }
}

fn parse_url(url: &str) -> (String, HashMap<String, String>) {
    let mut parts = url.splitn(2, '?');
    let path = parts.next().unwrap_or("/").to_string();
    let mut params = HashMap::new();

    if let Some(query) = parts.next() {
        for pair in query.split('&') {
            let mut kv = pair.splitn(2, '=');
            if let Some(key) = kv.next() {
                let val = kv.next().unwrap_or("");
                let decoded = urlencoding_decode(val);
                params.insert(key.to_string(), decoded);
            }
        }
    }

    (path, params)
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                decoded.push(val);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            decoded.push(b' ');
            i += 1;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

const LIBREOFFICE_FIRST_RUN_CHECKED_KEY: &str = "libreoffice_first_run_checked";
const LIBREOFFICE_DETECTED_PATH_KEY: &str = "libreoffice_detected_path";

#[derive(Debug, Clone)]
struct LibreOfficeStatus {
    installed: bool,
    path: Option<String>,
    checked_just_now: bool,
}

fn ensure_libreoffice_first_run_check(storage: &dragon_storage::DragonStorage) -> LibreOfficeStatus {
    let already_checked = storage
        .get_setting(LIBREOFFICE_FIRST_RUN_CHECKED_KEY)
        .ok()
        .flatten()
        .is_some();

    let detected = find_soffice_exe();
    let detected_str = detected
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());

    if !already_checked {
        let _ = storage.set_setting(LIBREOFFICE_FIRST_RUN_CHECKED_KEY, "true");
        let _ = storage.set_setting(
            LIBREOFFICE_DETECTED_PATH_KEY,
            detected_str.as_deref().unwrap_or(""),
        );
    } else if let Some(ref path_s) = detected_str {
        let prev = storage
            .get_setting(LIBREOFFICE_DETECTED_PATH_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        if prev != *path_s {
            let _ = storage.set_setting(LIBREOFFICE_DETECTED_PATH_KEY, path_s);
        }
    }

    LibreOfficeStatus {
        installed: detected.is_some(),
        path: detected_str,
        checked_just_now: !already_checked,
    }
}

fn open_in_explorer(file: &str) -> serde_json::Value {
    let p = Path::new(file);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if p.is_file() {
            let clean = file.replace('"', "");
            let res = std::process::Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{}\"", clean))
                .spawn();
            return match res {
                Ok(_) => json!({ "opened": true, "mode": "select_file", "target": clean }),
                Err(e) => json!({ "opened": false, "error": e.to_string() }),
            };
        } else if p.is_dir() {
            let res = std::process::Command::new("explorer.exe").arg(p).spawn();
            return match res {
                Ok(_) => json!({ "opened": true, "mode": "open_dir", "target": file }),
                Err(e) => json!({ "opened": false, "error": e.to_string() }),
            };
        } else if let Some(parent) = p.parent().filter(|d| d.is_dir()) {
            let res = std::process::Command::new("explorer.exe").arg(parent).spawn();
            return match res {
                Ok(_) => json!({
                    "opened": true,
                    "mode": "open_parent",
                    "target": parent.to_string_lossy()
                }),
                Err(e) => json!({ "opened": false, "error": e.to_string() }),
            };
        }
        return json!({ "opened": false, "error": "Ruta no encontrada en el equipo" });
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        json!({ "opened": false, "error": "Solo soportado en Windows" })
    }
}

fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        _ => "application/octet-stream",
    }
}

fn build_preview_payload(file: &str) -> serde_json::Value {
    let p = Path::new(file);
    if !p.exists() || !p.is_file() {
        return json!({ "success": false, "error": "Archivo no encontrado en el disco" });
    }

    let meta = std::fs::metadata(p).ok();
    let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: DateTime<Local> = t.into();
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        })
        .unwrap_or_else(|| "Desconocida".to_string());

    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file.to_string());
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = mime_for_extension(&ext);
    let office_pdf_available = find_soffice_exe().is_some();
    let office_pdf_cached = office_pdf_cache_path(p).is_some_and(|cp| cp.is_file());

    let mut payload = match ext.as_str() {
        "pdf" => {
            let pdf_info = extract_pdf_preview(p, 50, 30_000).ok();
            json!({
                "success": true,
                "kind": "pdf",
                "page_count": pdf_info.as_ref().map(|i| i.page_count),
                "content": pdf_info.map(|i| i.text).unwrap_or_default(),
            })
        }
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico" | "svg" | "avif" => {
            json!({
                "success": true,
                "kind": "image",
            })
        }
        "mp4" | "webm" | "mov" | "m4v" | "ogv" => {
            json!({
                "success": true,
                "kind": "video",
            })
        }
        "mp3" | "wav" | "ogg" | "oga" | "opus" | "flac" | "m4a" | "aac" => {
            json!({
                "success": true,
                "kind": "audio",
            })
        }
        "md" | "markdown" | "mdx" => {
            let content = extract_content(p, 80_000).unwrap_or_default();
            json!({
                "success": true,
                "kind": "markdown",
                "content": content,
            })
        }
        "csv" | "tsv" => {
            let delim = if ext == "tsv" { '\t' } else { ',' };
            let raw_text = extract_content(p, 120_000).unwrap_or_default();
            let (rows, total_rows) = parse_delimited_table(&raw_text, delim, 200, 24);
            json!({
                "success": true,
                "kind": "table",
                "sheets": [{
                    "name": name,
                    "rows": rows,
                    "total_rows": total_rows
                }],
                "content": raw_text,
            })
        }
        "xlsx" | "xlsm" => {
            let visual = extract_excel_visual(p, 8, 180, 26).ok();
            let content = extract_content(p, 20_000).unwrap_or_default();
            if let Some(v) = visual.filter(|v| !v.sheets.is_empty()) {
                json!({
                    "success": true,
                    "kind": "excel",
                    "excel_visual": v,
                    "content": content,
                })
            } else {
                let sheets = extract_xlsx_sheets(p, 6, 150, 20).unwrap_or_default();
                json!({
                    "success": true,
                    "kind": "excel",
                    "sheets": sheets,
                    "content": content,
                })
            }
        }
        "xls" | "xlsb" | "ods" => {
            let content = extract_content(p, 40_000).unwrap_or_default();
            json!({
                "success": true,
                "kind": "excel",
                "legacy": true,
                "content": content,
            })
        }
        "pptx" | "pptm" | "ppsx" => {
            let visual = extract_pptx_visual(p, 60).ok();
            let content = extract_content(p, 40_000).unwrap_or_default();
            if let Some(v) = visual.filter(|v| !v.slides.is_empty()) {
                json!({
                    "success": true,
                    "kind": "powerpoint",
                    "ppt_visual": v,
                    "content": content,
                })
            } else {
                let slides = extract_pptx_slides(p, 60).unwrap_or_default();
                json!({
                    "success": true,
                    "kind": "powerpoint",
                    "slides": slides,
                    "content": content,
                })
            }
        }
        "ppt" | "odp" => {
            let content = extract_content(p, 40_000).unwrap_or_default();
            json!({
                "success": true,
                "kind": "powerpoint",
                "legacy": true,
                "content": content,
            })
        }
        "docx" | "docm" | "dotx" | "dotm" => {
            match extract_word_visual(p, 450) {
                Ok(v) if !v.blocks.is_empty() => {
                    let content = if v.plain_text.trim().is_empty() {
                        extract_content(p, 40_000).unwrap_or_default()
                    } else {
                        v.plain_text.clone()
                    };
                    json!({
                        "success": true,
                        "kind": "word",
                        "word_visual": v,
                        "content": content,
                    })
                }
                _ => {
                    let paragraphs = extract_docx_paragraphs(p, 40_000).unwrap_or_default();
                    let content = if paragraphs.is_empty() {
                        extract_content(p, 40_000).unwrap_or_default()
                    } else {
                        paragraphs.join("\n\n")
                    };
                    json!({
                        "success": true,
                        "kind": "word",
                        "paragraphs": paragraphs,
                        "content": content,
                    })
                }
            }
        }
        "doc" | "odt" | "ott" | "rtf" => {
            let content = extract_content(p, 40_000).unwrap_or_default();
            json!({
                "success": true,
                "kind": "word",
                "legacy": true,
                "content": content,
            })
        }
        "zip" | "qlplugin" | "jar" | "apk" | "nupkg" => {
            match extract_zip_entries(p, 200) {
                Ok((total, entries)) => json!({
                    "success": true,
                    "kind": "archive",
                    "archive_total": total,
                    "archive_entries": entries,
                }),
                Err(e) => json!({
                    "success": true,
                    "kind": "unsupported",
                    "note": format!("No se pudo leer el paquete ZIP: {}", e),
                }),
            }
        }
        "html" | "htm" => {
            let content = extract_content(p, 60_000).unwrap_or_default();
            json!({
                "success": true,
                "kind": "html",
                "content": content,
            })
        }
        _ => {
            let fmt = detect_format(p);
            match fmt {
                DocumentFormat::LegacyOffice
                | DocumentFormat::OpenDocument
                | DocumentFormat::Epub
                | DocumentFormat::Rtf => {
                    let content = extract_content(p, 40_000).unwrap_or_default();
                    json!({
                        "success": true,
                        "kind": "document",
                        "content": content,
                    })
                }
                DocumentFormat::TextOrCode => {
                    let content = extract_content(p, 60_000).unwrap_or_default();
                    json!({
                        "success": true,
                        "kind": "code",
                        "content": content,
                    })
                }
                _ => json!({
                    "success": true,
                    "kind": "unsupported",
                    "note": "Este formato binario/especializado no tiene renderizado web integrado.",
                }),
            }
        }
    };

    if let Some(obj) = payload.as_object_mut() {
        obj.insert("file".to_string(), json!(file));
        obj.insert("name".to_string(), json!(name));
        obj.insert("extension".to_string(), json!(ext));
        obj.insert("mime".to_string(), json!(mime));
        obj.insert("size_bytes".to_string(), json!(size_bytes));
        obj.insert("size_label".to_string(), json!(format_file_size(size_bytes)));
        obj.insert("modified".to_string(), json!(modified));
        obj.insert("office_pdf_available".to_string(), json!(office_pdf_available));
        obj.insert("office_pdf_cached".to_string(), json!(office_pdf_cached));
    }
    payload
}

fn parse_delimited_table(
    text: &str,
    delimiter: char,
    max_rows: usize,
    max_cols: usize,
) -> (Vec<Vec<String>>, usize) {
    let mut rows = Vec::new();
    let mut total_rows = 0usize;
    let mut max_seen_cols = 0usize;

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        total_rows += 1;
        if rows.len() >= max_rows {
            continue;
        }

        let mut cols = Vec::new();
        let mut cell = String::new();
        let mut in_quotes = false;
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '"' {
                if in_quotes && chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            } else if ch == delimiter && !in_quotes {
                cols.push(cell.trim().to_string());
                cell.clear();
                if cols.len() >= max_cols {
                    break;
                }
            } else {
                cell.push(ch);
            }
        }
        if cols.len() < max_cols {
            cols.push(cell.trim().to_string());
        }
        max_seen_cols = max_seen_cols.max(cols.len());
        rows.push(cols);
    }

    let norm = max_seen_cols.max(1);
    for r in &mut rows {
        r.resize(norm, String::new());
    }
    (rows, total_rows)
}

fn parse_http_range(range_header: Option<&str>, total_len: u64) -> Option<(u64, u64)> {
    let hdr = range_header?.trim();
    let spec = hdr.strip_prefix("bytes=")?;
    let (start_s, end_s) = spec.split_once('-')?;
    if total_len == 0 {
        return None;
    }
    if start_s.is_empty() {
        let suffix_len = end_s.parse::<u64>().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        return Some((start, total_len - 1));
    }
    let start = start_s.parse::<u64>().ok()?;
    if start >= total_len {
        return None;
    }
    let end = if end_s.is_empty() {
        total_len - 1
    } else {
        end_s.parse::<u64>().ok()?.min(total_len - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

async fn serve_raw_file(
    socket: &mut TcpStream,
    file: &str,
    range_header: Option<&str>,
) -> Result<()> {
    let p = Path::new(file);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = mime_for_extension(&ext);
    serve_path_with_mime(socket, p, mime, range_header).await
}

fn sanitize_ascii_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "archivo".to_string()
    } else {
        trimmed.to_string()
    }
}

fn rfc5987_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'.' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

async fn serve_download_file(socket: &mut TcpStream, file: &str) -> Result<()> {
    let p = Path::new(file);
    if !p.exists() || !p.is_file() {
        send_response(socket, "404 Not Found", "text/plain; charset=utf-8", b"Archivo no encontrado").await?;
        return Ok(());
    }

    let mut f = match tokio::fs::File::open(p).await {
        Ok(f) => f,
        Err(_) => {
            send_response(socket, "403 Forbidden", "text/plain; charset=utf-8", b"No se pudo leer el archivo").await?;
            return Ok(());
        }
    };
    let total_len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let raw_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archivo");
    let ascii_name = sanitize_ascii_filename(raw_name);
    let utf8_encoded = rfc5987_encode(raw_name);

    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
        Content-Type: application/octet-stream\r\n\
        Content-Length: {total_len}\r\n\
        Content-Disposition: attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{utf8_encoded}\r\n\
        X-Content-Type-Options: nosniff\r\n\
        Cache-Control: no-store\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Private-Network: true\r\n\
        Connection: close\r\n\r\n"
    );
    socket.write_all(headers.as_bytes()).await?;

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        socket.write_all(&buf[..n]).await?;
    }
    socket.flush().await?;
    Ok(())
}

async fn serve_path_with_mime(
    socket: &mut TcpStream,
    p: &Path,
    mime: &str,
    range_header: Option<&str>,
) -> Result<()> {
    if !p.exists() || !p.is_file() {
        send_response(socket, "404 Not Found", "text/plain; charset=utf-8", b"Archivo no encontrado").await?;
        return Ok(());
    }

    let mut f = match tokio::fs::File::open(p).await {
        Ok(f) => f,
        Err(_) => {
            send_response(socket, "403 Forbidden", "text/plain; charset=utf-8", b"No se pudo leer el archivo").await?;
            return Ok(());
        }
    };
    let total_len = f.metadata().await.map(|m| m.len()).unwrap_or(0);

    let (status, start, len, content_range_hdr) = if let Some((s, e)) = parse_http_range(range_header, total_len) {
        let len = e - s + 1;
        let cr = format!("Content-Range: bytes {}-{}/{}\r\n", s, e, total_len);
        ("206 Partial Content", s, len, cr)
    } else {
        ("200 OK", 0, total_len, String::new())
    };

    if start > 0 {
        f.seek(std::io::SeekFrom::Start(start)).await?;
    }

    let headers = format!(
        "HTTP/1.1 {status}\r\n\
        Content-Type: {mime}\r\n\
        Content-Length: {len}\r\n\
        Accept-Ranges: bytes\r\n\
        Content-Disposition: inline\r\n\
        X-Content-Type-Options: nosniff\r\n\
        {content_range_hdr}\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Private-Network: true\r\n\
        Connection: close\r\n\r\n"
    );
    socket.write_all(headers.as_bytes()).await?;

    let mut limited = f.take(len);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = limited.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        socket.write_all(&buf[..n]).await?;
    }
    socket.flush().await?;
    Ok(())
}

async fn serve_zip_asset(socket: &mut TcpStream, file: &str, entry: &str) -> Result<()> {
    let file_owned = file.to_string();
    let entry_owned = entry.to_string();
    let ext = Path::new(&entry_owned)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = mime_for_extension(&ext);

    let result = tokio::task::spawn_blocking(move || {
        read_zip_entry_bytes(&file_owned, &entry_owned, 12 * 1024 * 1024)
    })
    .await;

    match result {
        Ok(Ok(bytes)) => {
            let headers = format!(
                "HTTP/1.1 200 OK\r\n\
                Content-Type: {mime}\r\n\
                Content-Length: {}\r\n\
                Cache-Control: max-age=300\r\n\
                X-Content-Type-Options: nosniff\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Private-Network: true\r\n\
                Connection: close\r\n\r\n",
                bytes.len()
            );
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(&bytes).await?;
            socket.flush().await?;
        }
        _ => {
            send_response(
                socket,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Recurso embebido no encontrado",
            )
            .await?;
        }
    }
    Ok(())
}

fn find_soffice_exe() -> Option<PathBuf> {
    if let Ok(guard) = configured_libreoffice_path_store().read() {
        if let Some(ref custom) = *guard {
            if custom.is_file() {
                return Some(custom.clone());
            }
        }
    }
    #[cfg(windows)]
    {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(base) = std::env::var(env_key) {
                let dir = PathBuf::from(base).join("LibreOffice").join("program");
                candidates.push(dir.join("soffice.com"));
                candidates.push(dir.join("soffice.exe"));
            }
        }
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let dir = PathBuf::from(local_app_data)
                .join("Programs")
                .join("LibreOffice")
                .join("program");
            candidates.push(dir.join("soffice.com"));
            candidates.push(dir.join("soffice.exe"));
        }
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                candidates.push(dir.join("soffice.com"));
                candidates.push(dir.join("soffice.exe"));
            }
        }
        for c in candidates {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let c = dir.join("soffice");
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn office_pdf_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(1))
}

fn fnv1a_hex(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn office_pdf_cache_dir() -> PathBuf {
    std::env::temp_dir().join("dragon-office-preview")
}

fn office_pdf_cache_path(p: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(p).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let key_input = format!("{}|{}|{}", p.to_string_lossy().to_lowercase(), size, mtime);
    let key = fnv1a_hex(key_input.as_bytes());
    Some(office_pdf_cache_dir().join(format!("{}.pdf", key)))
}

async fn serve_office_pdf(
    socket: &mut TcpStream,
    file: &str,
    range_header: Option<&str>,
) -> Result<()> {
    let p = Path::new(file);
    if !p.exists() || !p.is_file() {
        send_response(socket, "404 Not Found", "text/plain; charset=utf-8", b"Archivo no encontrado").await?;
        return Ok(());
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let allowed = matches!(
        ext.as_str(),
        "doc" | "docx" | "docm" | "dotx" | "dotm" | "xls" | "xlsx" | "xlsm" | "xlsb" | "ppt"
            | "pptx" | "pptm" | "ppsx" | "odt" | "ott" | "ods" | "odp" | "rtf"
    );
    if !allowed {
        send_response(socket, "400 Bad Request", "text/plain; charset=utf-8", b"Formato no compatible").await?;
        return Ok(());
    }

    let Some(cache_pdf) = office_pdf_cache_path(p) else {
        send_response(socket, "500 Internal Server Error", "text/plain; charset=utf-8", b"No se pudo leer metadata").await?;
        return Ok(());
    };

    if cache_pdf.is_file() {
        return serve_path_with_mime(socket, &cache_pdf, "application/pdf", range_header).await;
    }

    let Some(soffice) = find_soffice_exe() else {
        send_response(
            socket,
            "503 Service Unavailable",
            "text/plain; charset=utf-8",
            b"Motor de conversion Office PDF no disponible",
        )
        .await?;
        return Ok(());
    };

    let _permit = office_pdf_semaphore().acquire().await;
    if cache_pdf.is_file() {
        return serve_path_with_mime(socket, &cache_pdf, "application/pdf", range_header).await;
    }

    let src_path = p.to_path_buf();
    let cache_clone = cache_pdf.clone();
    let ext_clone = ext.clone();
    let conv_res = tokio::task::spawn_blocking(move || -> Result<()> {
        let base_dir = office_pdf_cache_dir();
        let profile_dir = base_dir.join("lo_profile");
        let work_dir = base_dir.join(format!("work_{}", std::process::id()));
        std::fs::create_dir_all(&profile_dir)?;
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir)?;

        let in_copy = work_dir.join(format!("source.{}", ext_clone));
        std::fs::copy(&src_path, &in_copy)?;
        let out_pdf = work_dir.join("source.pdf");

        let profile_url = format!(
            "file:///{}",
            profile_dir.to_string_lossy().replace('\\', "/")
        );

        let mut cmd = std::process::Command::new(&soffice);
        cmd.args([
            "--headless",
            "--norestore",
            "--nolockcheck",
            "--nodefault",
            &format!("-env:UserInstallation={}", profile_url),
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(&work_dir)
        .arg(&in_copy);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let status = cmd.status()?;
        if status.success() && out_pdf.is_file() {
            let _ = std::fs::rename(&out_pdf, &cache_clone)
                .or_else(|_| std::fs::copy(&out_pdf, &cache_clone).map(|_| ()));
        }
        let _ = std::fs::remove_dir_all(&work_dir);
        if cache_clone.is_file() {
            Ok(())
        } else {
            anyhow::bail!("La conversión PDF no generó salida")
        }
    })
    .await;

    match conv_res {
        Ok(Ok(())) if cache_pdf.is_file() => {
            serve_path_with_mime(socket, &cache_pdf, "application/pdf", range_header).await
        }
        _ => {
            send_response(
                socket,
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                b"Error al renderizar documento Office",
            )
            .await
        }
    }
}

#[cfg(feature = "lan")]
fn find_bundled_libreoffice_msi() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
            if let Some(repo_root) = parent.parent().and_then(|p| p.parent()) {
                dirs.push(repo_root.to_path_buf());
            }
        }
    }

    for dir in dirs {
        let exact = dir.join("LibreOffice_26.8.1_Win_x86-64.msi");
        if exact.is_file() {
            return Some(exact);
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut matches: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| {
                                let l = n.to_ascii_lowercase();
                                l.starts_with("libreoffice") && l.ends_with(".msi")
                            })
                            .unwrap_or(false)
                })
                .collect();
            matches.sort();
            if let Some(found) = matches.pop() {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(feature = "lan")]
fn build_client_launcher_bat(server_url: &str, port: u16) -> String {
    format!(
        "@echo off\r\n\
        setlocal\r\n\
        title Radar del Dragon - Agente Local del Equipo\r\n\
        set \"APP_DIR=%LOCALAPPDATA%\\DragonRadar\\bin\"\r\n\
        set \"EXE_PATH=%APP_DIR%\\dragon.exe\"\r\n\
        if not exist \"%APP_DIR%\" mkdir \"%APP_DIR%\"\r\n\
        if not exist \"%EXE_PATH%\" (\r\n\
            echo Descargando motor local desde {server_url}...\r\n\
            powershell -NoProfile -Command \"$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '{server_url}/api/agent/binary' -OutFile '%EXE_PATH%' -UseBasicParsing\"\r\n\
        )\r\n\
        set \"LO_FOUND=0\"\r\n\
        if exist \"%ProgramFiles%\\LibreOffice\\program\\soffice.com\" set \"LO_FOUND=1\"\r\n\
        if exist \"%ProgramFiles%\\LibreOffice\\program\\soffice.exe\" set \"LO_FOUND=1\"\r\n\
        if exist \"%ProgramFiles(x86)%\\LibreOffice\\program\\soffice.com\" set \"LO_FOUND=1\"\r\n\
        if exist \"%LOCALAPPDATA%\\Programs\\LibreOffice\\program\\soffice.com\" set \"LO_FOUND=1\"\r\n\
        if \"%LO_FOUND%\"==\"0\" (\r\n\
            echo LibreOffice no detectado en %COMPUTERNAME%. Verificando instalador en {server_url}...\r\n\
            powershell -NoProfile -ExecutionPolicy Bypass -Command \"$ErrorActionPreference='Continue'; $ProgressPreference='SilentlyContinue'; try {{ $info = Invoke-RestMethod -Uri '{server_url}/api/agent/libreoffice-info' -TimeoutSec 5; if ($info.available) {{ $tmpDir = Join-Path $env:TEMP 'DragonRadar'; New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null; $msi = Join-Path $tmpDir $info.filename; Write-Host ('Copiando ' + $info.filename + ' desde el servidor...'); Invoke-WebRequest -Uri '{server_url}/api/agent/libreoffice-installer' -OutFile $msi -UseBasicParsing; Write-Host 'Instalando LibreOffice (se solicitara autorizacion de Administrador UAC)...'; $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList ('/i \"' + $msi + '\" /qn /norestart') -Verb RunAs -Wait -PassThru; if ($p.ExitCode -in @(0,3010)) {{ Write-Host 'LibreOffice instalado correctamente.' }} else {{ Write-Warning ('Instalacion finalizada con codigo ' + $p.ExitCode) }}; Remove-Item -LiteralPath $msi -Force -ErrorAction SilentlyContinue }} }} catch {{ Write-Warning ('No se pudo instalar LibreOffice automaticamente: ' + $_.Exception.Message) }}\"\r\n\
        )\r\n\
        echo Iniciando indice y buscador local en %COMPUTERNAME% (puerto {port})...\r\n\
        \"%EXE_PATH%\" ui --local --port {port} --no-browser\r\n",
        server_url = server_url,
        port = port
    )
}

async fn send_preflight_ok(socket: &mut TcpStream) -> Result<()> {
    let response = "HTTP/1.1 204 No Content\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
        Access-Control-Allow-Headers: *\r\n\
        Access-Control-Allow-Private-Network: true\r\n\
        Content-Length: 0\r\n\
        Connection: close\r\n\r\n";
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

#[cfg(feature = "lan")]
async fn send_download(
    socket: &mut TcpStream,
    content_type: &str,
    filename: &str,
    body: &[u8],
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
        Content-Type: {}\r\n\
        Content-Disposition: attachment; filename=\"{}\"\r\n\
        Content-Length: {}\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Private-Network: true\r\n\
        Connection: close\r\n\r\n",
        content_type,
        filename,
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

async fn send_response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\n\
        Content-Type: {}\r\n\
        Content-Length: {}\r\n\
        Cache-Control: no-store, private, max-age=0\r\n\
        Pragma: no-cache\r\n\
        X-Content-Type-Options: nosniff\r\n\
        Referrer-Policy: no-referrer\r\n\
        X-Frame-Options: SAMEORIGIN\r\n\
        Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: http://127.0.0.1:*; media-src 'self' blob: http://127.0.0.1:*; frame-src 'self' blob: http://127.0.0.1:*; connect-src 'self' http://127.0.0.1:*; object-src 'none'; base-uri 'none'; frame-ancestors 'self'\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Private-Network: true\r\n\
        Connection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );

    socket.write_all(response.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_decode_handles_utf8_spanish_paths() {
        let encoded = "C%3A%5CUsers%5CInformaci%C3%B3n%20T%C3%A9cnica%20%C3%B1.pdf";
        assert_eq!(
            urlencoding_decode(encoded),
            r"C:\Users\Información Técnica ñ.pdf"
        );
    }

    #[test]
    fn parse_http_range_supports_open_closed_and_suffix_ranges() {
        assert_eq!(parse_http_range(Some("bytes=0-99"), 1000), Some((0, 99)));
        assert_eq!(parse_http_range(Some("bytes=500-"), 1000), Some((500, 999)));
        assert_eq!(parse_http_range(Some("bytes=-100"), 1000), Some((900, 999)));
        assert_eq!(parse_http_range(Some("bytes=1200-1500"), 1000), None);
    }

    #[test]
    fn parse_delimited_table_respects_quotes_and_commas() {
        let csv = "Nombre,Ciudad,Nota\n\"Perez, Juan\",Bogotá,\"1,250\"";
        let (rows, total) = parse_delimited_table(csv, ',', 50, 10);
        assert_eq!(total, 2);
        assert_eq!(rows[0], vec!["Nombre", "Ciudad", "Nota"]);
        assert_eq!(rows[1], vec!["Perez, Juan", "Bogotá", "1,250"]);
    }

    #[test]
    fn first_run_libreoffice_check_persists_flag_in_storage() {
        let temp_dir = std::env::temp_dir().join(format!(
            "dragon-lo-first-run-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("radar.redb");

        let storage = dragon_storage::DragonStorage::open(&db_path).unwrap();
        assert!(storage
            .get_setting(LIBREOFFICE_FIRST_RUN_CHECKED_KEY)
            .unwrap()
            .is_none());

        let first = ensure_libreoffice_first_run_check(&storage);
        assert!(first.checked_just_now);
        let stored_flag = storage
            .get_setting(LIBREOFFICE_FIRST_RUN_CHECKED_KEY)
            .unwrap();
        assert!(stored_flag.is_some());

        let second = ensure_libreoffice_first_run_check(&storage);
        assert!(!second.checked_just_now);
        assert_eq!(second.installed, first.installed);
        assert_eq!(second.path, first.path);

        drop(storage);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn parse_search_extensions_detects_explicit_and_mode_queries() {
        let temp_dir = std::env::temp_dir().join(format!(
            "dragon-ext-parse-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("radar.redb");

        let storage = dragon_storage::DragonStorage::open(&db_path).unwrap();
        storage
            .insert_batch(&[dragon_core::FileRecord {
                id: 1,
                parent_id: 0,
                name: "informe_anual.pdf".to_string(),
                path: Some(PathBuf::from(r"C:\Docs\informe_anual.pdf")),
                size: 1024,
                attributes: 0,
                modified_ms: 1000,
                is_directory: false,
            }])
            .unwrap();

        let hybrid = parse_search_extensions(&storage, "contrato *.xlsx ext:pdf", false);
        assert!(hybrid.explicit_extension_filter);
        assert_eq!(hybrid.extensions, vec!["xlsx".to_string(), "pdf".to_string()]);
        assert_eq!(hybrid.text_tokens, vec!["contrato".to_string()]);

        let bare_hybrid = parse_search_extensions(&storage, "pdf", false);
        assert!(!bare_hybrid.explicit_extension_filter);
        assert_eq!(bare_hybrid.text_tokens, vec!["pdf".to_string()]);

        let ext_mode = parse_search_extensions(&storage, "pdf presupuesto", true);
        assert_eq!(ext_mode.extensions, vec!["pdf".to_string()]);
        assert_eq!(ext_mode.text_tokens, vec!["presupuesto".to_string()]);

        let ext_multi = parse_search_extensions(&storage, ".pdf, .docx", true);
        assert!(ext_multi.explicit_extension_filter);
        assert_eq!(ext_multi.extensions, vec!["pdf".to_string(), "docx".to_string()]);

        drop(storage);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
