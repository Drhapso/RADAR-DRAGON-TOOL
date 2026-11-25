//! Dragon Radar CLI: Interfaz de comando principal de Dragon Radar Tool.
//! Provee comandos de escaneo MFT de alta velocidad, búsqueda instantánea
//! y pruebas de rendimiento comparativas.

use clap::{Parser, Subcommand};
use dragon_ai::{DragonEmbedder, HybridRanker, VectorStore};
use dragon_extract::{detect_format, extract_content};
use dragon_index::DragonIndexEngine;
use dragon_mft::{UnifiedScanResult, UnifiedScanner};
use dragon_storage::DragonStorage;
use std::path::Path;
use std::time::Instant;

mod config;
mod indexer;
mod ui_server;

#[derive(Parser)]
#[command(name = "dragon")]
#[command(author = "Drhapso <radar-del-dragon@local>")]
#[command(version = "0.1.0")]
#[command(about = "Dragon Radar Tool: Buscador híbrido ultra-rápido de disco y contenido")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Escanea una unidad o carpeta y mide el rendimiento en milisegundos
    Scan {
        /// Unidad o carpeta a escanear (ej. "C", "D:\", "C:\Users\Drhapso")
        #[arg(default_value = "C")]
        target: String,

        /// Limite de archivos a imprimir en el resumen
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Indexa archivos de forma incremental (solo nuevos o modificados por peso/fecha)
    Index {
        /// Carpeta a indexar
        target: String,

        /// Carpeta destino para almacenar los índices de texto (opcional, por defecto se aísla por equipo)
        #[arg(short, long)]
        index_dir: Option<String>,

        /// Archivo de base de datos local (opcional, por defecto se aísla por equipo)
        #[arg(short, long)]
        db_path: Option<String>,

        /// Archivo para almacenar los vectores semánticos (opcional)
        #[arg(short, long)]
        vectors_file: Option<String>,

        /// Forzar reindexación completa ignorando la base de datos incremental
        #[arg(short, long, default_value_t = false)]
        force: bool,

        /// Fuerza el almacenamiento local aislado para este equipo
        #[arg(long, default_value_t = false)]
        local: bool,
    },
    /// Sincroniza automáticamente todas las unidades locales con checkpoints y validación de movidos/eliminados
    Sync {
        /// Carpeta destino para almacenar los índices de texto (opcional)
        #[arg(short, long)]
        index_dir: Option<String>,

        /// Archivo de base de datos local (opcional)
        #[arg(short, long)]
        db_path: Option<String>,

        /// Archivo para almacenar los vectores semánticos (opcional)
        #[arg(short, long)]
        vectors_file: Option<String>,

        /// Forzar escaneo completo reiniciando el punto de control inicial
        #[arg(short, long, default_value_t = false)]
        force: bool,

        /// Fuerza el almacenamiento local aislado para este equipo
        #[arg(long, default_value_t = false)]
        local: bool,
    },
    /// Busca instantáneamente en el índice local por nombre o contenido
    Search {
        /// Término de búsqueda
        query: String,

        /// Carpeta del índice (opcional)
        #[arg(short, long)]
        index_dir: Option<String>,

        /// Límite de resultados
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Búsqueda semántica conceptual sobre el índice local
    Semantic {
        /// Consulta conceptual (ej. "informes financieros", "controladores de hardware")
        query: String,

        /// Archivo de vectores (opcional)
        #[arg(short, long)]
        vectors_file: Option<String>,

        /// Límite de resultados
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Búsqueda híbrida avanzada en el equipo local
    Hybrid {
        /// Consulta para búsqueda combinada
        query: String,

        /// Carpeta del índice (opcional)
        #[arg(short, long)]
        index_dir: Option<String>,

        /// Archivo de vectores (opcional)
        #[arg(short, long)]
        vectors_file: Option<String>,

        /// Peso relativo de coincidencia exacta BM25
        #[arg(long, default_value_t = 1.0)]
        bm25_weight: f32,

        /// Peso relativo de similitud conceptual semántica
        #[arg(long, default_value_t = 1.0)]
        semantic_weight: f32,

        /// Límite de resultados
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Extrae y muestra el texto de un documento (PDF, DOCX, XLSX, PPTX, Código)
    Extract {
        /// Ruta del archivo a examinar
        file: String,

        /// Máximo de bytes de texto a mostrar
        #[arg(short, long, default_value_t = 2000)]
        max_bytes: usize,
    },
    /// Inicia el servidor stdio de Model Context Protocol (MCP) para conectar agentes IA
    Mcp,
    /// Abre la interfaz visual Dragon Radar conectada al índice local o compartido en LAN
    Ui {
        /// Puerto TCP para el servidor web (por defecto 8092 o el definido en config/site_config.json)
        #[arg(short, long)]
        port: Option<u16>,

        /// Dirección IP de escucha (127.0.0.1 local; en compilación estándar 0.0.0.0 para red LAN)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Habilita acceso de interfaz web en la red LAN cerrada (--host 0.0.0.0)
        #[cfg(feature = "lan")]
        #[arg(long, default_value_t = false)]
        lan: bool,

        /// Modo LAN híbrido: "local_agent" (cada PC indexa sus discos) o "shared_server" (carpetas compartidas)
        #[cfg(feature = "lan")]
        #[arg(long)]
        lan_mode: Option<String>,

        /// Ruta explícita al archivo config/site_config.json
        #[arg(long)]
        config: Option<String>,

        /// No abrir el navegador automáticamente al iniciar el servidor
        #[arg(long, default_value_t = false)]
        no_browser: bool,

        /// Falla si el puerto solicitado está ocupado en lugar de saltar a otro puerto
        #[arg(long, default_value_t = false)]
        strict_port: bool,

        /// Fuerza guardar el índice exclusivamente en la carpeta local de este equipo
        #[arg(long, default_value_t = false)]
        local: bool,
    },
    /// Información del sistema, unidades detectadas y estado de privilegios
    Info,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Ui {
        port: None,
        host: "127.0.0.1".to_string(),
        #[cfg(feature = "lan")]
        lan: false,
        #[cfg(feature = "lan")]
        lan_mode: None,
        config: None,
        no_browser: false,
        strict_port: false,
        local: false,
    });

    match command {
        Commands::Scan { target, limit } => {
            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║              DRAGON RADAR TOOL - RADAR SCAN              ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            println!("Objetivo del radar: {}", target);
            println!("Iniciando escaneo de alta velocidad...\n");

            let t0 = Instant::now();
            match UnifiedScanner::scan(&target) {
                Ok(result) => {
                    let total_time = t0.elapsed();
                    let metrics = result.metrics();

                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    println!("  RESULTADO DEL ESCANEO");
                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    println!("  Método utilizado    : {:?}", metrics.scan_method);
                    println!("  Archivos encontrados: {}", metrics.total_files);
                    println!("  Carpetas detectadas : {}", metrics.total_directories);
                    println!("  Entradas totales    : {}", metrics.total_files + metrics.total_directories);
                    println!("  Tiempo del escáner  : {} ms", metrics.duration_ms);
                    println!("  Tiempo total proceso: {:?}", total_time);
                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

                    match result {
                        #[cfg(windows)]
                        UnifiedScanResult::Mft(mft) => {
                            println!("\nMuestra de archivos detectados por MFT (primeros {}):", limit);
                            let mut shown = 0;
                            for (&id, &(_, _, attr)) in &mft.records {
                                if shown >= limit {
                                    break;
                                }
                                let is_dir = (attr & dragon_core::FileRecord::ATTR_DIRECTORY) != 0;
                                let kind = if is_dir { "[DIR] " } else { "[FILE]" };
                                if let Some(path) = mft.resolve_path(id) {
                                    println!("  {} {}", kind, path.display());
                                    shown += 1;
                                }
                            }
                        }
                        UnifiedScanResult::Fallback(fallback) => {
                            println!("\nMuestra de archivos detectados por Fallback (primeros {}):", limit);
                            for rec in fallback.records.iter().take(limit) {
                                let kind = if rec.is_directory { "[DIR] " } else { "[FILE]" };
                                let path_str = rec.path.as_deref().map(|p| p.to_string_lossy()).unwrap_or_default();
                                println!("  {} {}", kind, path_str);
                            }
                        }
                    }
                }
                Err(err) => {
                    eprintln!("Error durante el escaneo: {}", err);
                }
            }
        }
        Commands::Index { target, index_dir, db_path, vectors_file, force, local } => {
            let paths = indexer::resolve_machine_storage_paths(local);
            let index_dir = index_dir.unwrap_or_else(|| paths.index_dir.to_string_lossy().to_string());
            let db_path = db_path.unwrap_or_else(|| paths.db_path.to_string_lossy().to_string());
            let vectors_file = vectors_file.unwrap_or_else(|| paths.vectors_file.to_string_lossy().to_string());

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║   DRAGON RADAR TOOL - INDEXADOR INCREMENTAL (REDB+AI)    ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            let abs_target = indexer::clean_abs_path(Path::new(&target));
            let scan_target_str = abs_target.to_string_lossy().to_string();

            println!("Equipo local       : {} ({})", paths.machine_name, paths.mode_label);
            println!("Objetivo a indexar : {}", scan_target_str);
            println!("Directorio Tantivy : {}", index_dir);
            println!("Base de datos Redb : {}", db_path);
            println!("Almacén Vectorial  : {}", vectors_file);
            println!("Modo               : {}", if force { "FORZADO (Reindexación completa)" } else { "INCREMENTAL (Nuevos, peso/fecha, movidos, eliminados)" });

            let t0 = Instant::now();
            let state = indexer::SharedRadarState::open(
                Path::new(&index_dir),
                Path::new(&db_path),
                Path::new(&vectors_file),
            )?;
            println!(
                "Motor Semántico AI : {}",
                if state.embedder.is_real_model() { "Model2Vec (256-d weights)" } else { "HashEmbedder (256-d)" }
            );
            println!(
                "Huellas previas    : {} entradas rastreadas",
                state.storage.count_index_states().unwrap_or(0)
            );

            let sum = indexer::index_single_target(&state, &scan_target_str, force)?;
            let total_vectors = state.vector_store.read().map(|vs| vs.len()).unwrap_or(0);
            let total_tracked = state.storage.count_index_states().unwrap_or(0);

            let elapsed = t0.elapsed();
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  INDEXACIÓN INCREMENTAL FINALIZADA");
            println!("  Archivos nuevos                : {}", sum.new_files);
            println!("  Archivos modificados (peso/fec): {}", sum.updated_files);
            println!("  Archivos movidos reubicados    : {}", sum.moved_files);
            println!("  Archivos sin cambios omitidos  : {}", sum.skipped_files);
            println!("  Entradas eliminadas purgadas   : {}", sum.deleted_files);
            println!("  Registros escritos en Redb     : {}", sum.stored_records);
            println!("  Total rastreado en index_state : {}", total_tracked);
            println!("  Vectores semánticos totales    : {} en {}", total_vectors, vectors_file);
            println!("  Tiempo total                   : {:?}", elapsed);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        }
        Commands::Sync { index_dir, db_path, vectors_file, force, local } => {
            let paths = indexer::resolve_machine_storage_paths(local);
            let index_dir = index_dir.unwrap_or_else(|| paths.index_dir.to_string_lossy().to_string());
            let db_path = db_path.unwrap_or_else(|| paths.db_path.to_string_lossy().to_string());
            let vectors_file = vectors_file.unwrap_or_else(|| paths.vectors_file.to_string_lossy().to_string());

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║    DRAGON RADAR TOOL - SINCRONIZACION Y CHECKPOINTS      ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            println!("Equipo local       : {} ({})", paths.machine_name, paths.mode_label);
            let t0 = Instant::now();
            let state = indexer::SharedRadarState::open(
                Path::new(&index_dir),
                Path::new(&db_path),
                Path::new(&vectors_file),
            )?;
            let cp = state.storage.load_checkpoint().unwrap_or_default();
            if force {
                println!("Modo: Escaneo completo FORZADO de todas las unidades locales");
            } else if cp.in_progress {
                println!(
                    "Modo: Reanudando desde punto de control {}/{} ({})",
                    cp.current_target_idx + 1,
                    cp.targets.len(),
                    cp.last_checkpoint_path.as_deref().unwrap_or("-")
                );
            } else if !cp.initial_scan_completed {
                println!("Modo: Indexación inicial automática de todas las unidades locales");
            } else {
                println!("Modo: Validación integral de índice (nuevos, peso/fecha, movidos, eliminados)");
            }

            let sum = indexer::run_startup_auto_sync_and_validate(&state, force)?;
            let total_tracked = state.storage.count_index_states().unwrap_or(0);
            let total_vectors = state.vector_store.read().map(|vs| vs.len()).unwrap_or(0);

            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  SINCRONIZACIÓN FINALIZADA ({:?})", t0.elapsed());
            println!("  Reanudado desde checkpoint     : {}", if sum.resumed_from_checkpoint { "Sí" } else { "No" });
            println!("  Archivos nuevos                : {}", sum.new_files);
            println!("  Archivos modificados (peso/fec): {}", sum.updated_files);
            println!("  Archivos movidos reubicados    : {}", sum.moved_files);
            println!("  Archivos sin cambios omitidos  : {}", sum.skipped_files);
            println!("  Entradas eliminadas purgadas   : {}", sum.deleted_files);
            println!("  Total rastreado en index_state : {}", total_tracked);
            println!("  Vectores semánticos totales    : {}", total_vectors);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        }
        Commands::Search { query, index_dir, limit } => {
            let paths = indexer::resolve_machine_storage_paths(false);
            let index_dir = index_dir.unwrap_or_else(|| paths.index_dir.to_string_lossy().to_string());

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║              DRAGON RADAR TOOL - RADAR SEARCH            ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            let engine = DragonIndexEngine::open_or_create(&index_dir)?;
            let t0 = Instant::now();
            let hits = engine.search(&query, limit)?;
            let elapsed = t0.elapsed();

            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  CONSULTA: \"{}\"", query);
            println!("  Resultados encontrados: {} (Tiempo: {:?})", hits.len(), elapsed);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            for (idx, hit) in hits.iter().enumerate() {
                println!("  {}. [Score: {:.2}] {} -> {}", idx + 1, hit.score, hit.filename, hit.path);
            }
        }
        Commands::Semantic { query, vectors_file, limit } => {
            let paths = indexer::resolve_machine_storage_paths(false);
            let vectors_file = vectors_file.unwrap_or_else(|| paths.vectors_file.to_string_lossy().to_string());

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║            DRAGON RADAR TOOL - SEMANTIC SEARCH           ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            let embedder = DragonEmbedder::load_or_detect::<&str>(None);
            println!("Modelo AI activo   : {}", if embedder.is_real_model() { "Model2Vec (256-d weights)" } else { "HashEmbedder (256-d)" });
            println!("Almacén de vectores: {}", vectors_file);

            let t0 = Instant::now();
            let store = VectorStore::load_from_file(&vectors_file)?;
            let q_vec = embedder.embed(&query);
            let hits = store.search(&q_vec, limit);
            let elapsed = t0.elapsed();

            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  CONSULTA SEMÁNTICA: \"{}\"", query);
            println!("  Resultados encontrados: {} (Tiempo: {:?})", hits.len(), elapsed);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            for (idx, hit) in hits.iter().enumerate() {
                println!("  {}. [Similitud: {:.2}%] {}", idx + 1, hit.score * 100.0, hit.path);
                if !hit.snippet.is_empty() {
                    let preview = hit.snippet.chars().take(120).collect::<String>().replace('\n', " ");
                    println!("     Resumen: \"{}\"", preview);
                }
            }
        }
        Commands::Hybrid { query, index_dir, vectors_file, bm25_weight, semantic_weight, limit } => {
            let paths = indexer::resolve_machine_storage_paths(false);
            let index_dir = index_dir.unwrap_or_else(|| paths.index_dir.to_string_lossy().to_string());
            let vectors_file = vectors_file.unwrap_or_else(|| paths.vectors_file.to_string_lossy().to_string());

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║        DRAGON RADAR TOOL - HYBRID RRF SEARCH (AI)        ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            let t0 = Instant::now();
            let engine = DragonIndexEngine::open_or_create(&index_dir)?;
            let bm25_hits = engine.search_content(&query, limit * 2).unwrap_or_default();

            let embedder = DragonEmbedder::load_or_detect::<&str>(None);
            let semantic_hits = if let Ok(store) = VectorStore::load_from_file(&vectors_file) {
                let q_vec = embedder.embed(&query);
                store.search(&q_vec, limit * 2)
            } else {
                Vec::new()
            };

            let hybrid_hits = HybridRanker::fuse(&bm25_hits, &semantic_hits, bm25_weight, semantic_weight, limit);
            let elapsed = t0.elapsed();

            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("  CONSULTA HÍBRIDA: \"{}\"", query);
            println!("  Resultados combinados: {} (Tiempo: {:?})", hybrid_hits.len(), elapsed);
            println!("  Fusión: BM25 (Tantivy) + Vector (Model2Vec 256-d) con RRF");
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            for (idx, hit) in hybrid_hits.iter().enumerate() {
                println!("  {}. [RRF: {:.4} | BM25: {:.2} | Semántico: {:.1}%] {}",
                    idx + 1, hit.rrf_score, hit.bm25_score, hit.semantic_score * 100.0, hit.path);
                if !hit.snippet.is_empty() {
                    let preview = hit.snippet.chars().take(120).collect::<String>().replace('\n', " ");
                    println!("     Contexto: \"{}\"", preview);
                }
            }
        }
        Commands::Extract { file, max_bytes } => {
            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║            DRAGON RADAR TOOL - EXTRACTOR TEST            ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            let path = std::path::Path::new(&file);
            println!("Archivo  : {}", file);
            let format = detect_format(path);
            println!("Formato  : {:?}", format);

            let t0 = Instant::now();
            match extract_content(path, max_bytes) {
                Ok(text) => {
                    let elapsed = t0.elapsed();
                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    println!("Caracteres extraídos: {} en {:?}", text.len(), elapsed);
                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    let preview = text.chars().take(800).collect::<String>();
                    println!("{}\n[...]", preview);
                }
                Err(e) => {
                    eprintln!("Error durante la extracción: {}", e);
                }
            }
        }
        Commands::Mcp => {
            dragon_mcp::run_stdio_server().await?;
        }
        #[cfg(feature = "lan")]
        Commands::Ui {
            port,
            host,
            lan,
            lan_mode,
            config,
            no_browser,
            strict_port,
            local,
        } => {
            let bind_host = if lan { "0.0.0.0".to_string() } else { host };
            let lan_mode_override = lan_mode
                .as_deref()
                .map(|raw| {
                    config::LanMode::parse_loose(raw).ok_or_else(|| {
                        anyhow::anyhow!(
                            "Valor inválido para --lan-mode: '{}' (use 'local_agent' o 'shared_server')",
                            raw
                        )
                    })
                })
                .transpose()?;
            ui_server::run_ui_server(ui_server::UiServerOptions {
                host: bind_host,
                port,
                force_local: local,
                no_browser,
                strict_port,
                config_path: config.map(std::path::PathBuf::from),
                lan_mode_override,
            })
            .await?;
        }
        #[cfg(not(feature = "lan"))]
        Commands::Ui {
            port,
            host: _,
            config,
            no_browser,
            strict_port,
            local,
        } => {
            ui_server::run_ui_server(ui_server::UiServerOptions {
                host: "127.0.0.1".to_string(),
                port,
                force_local: local,
                no_browser,
                strict_port,
                config_path: config.map(std::path::PathBuf::from),
                lan_mode_override: None,
            })
            .await?;
        }
        Commands::Info => {
            let paths = indexer::resolve_machine_storage_paths(false);
            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║              DRAGON RADAR TOOL - SYSTEM INFO             ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            println!("Versión del binario: 0.1.0 (Phase 4: AI & MCP Operational)");
            println!("Equipo local       : {} ({})", paths.machine_name, paths.mode_label);
            println!("Arquitectura       : {}", std::env::consts::ARCH);
            println!("Sistema Operativo  : {}", std::env::consts::OS);

            if paths.db_path.exists() {
                if let Ok(storage) = DragonStorage::open(&paths.db_path) {
                    if let Ok(count) = storage.count_records() {
                        println!("Base de datos Redb : {} ({} registros almacenados)", paths.db_path.display(), count);
                    }
                    if let Ok(tracked) = storage.count_index_states() {
                        println!("Huellas incrementales: {} rutas rastreadas (tamaño/fecha)", tracked);
                    }
                    if let Ok(cp) = storage.load_checkpoint() {
                        println!(
                            "Checkpoint inicial : {}",
                            if cp.initial_scan_completed {
                                "Completado".to_string()
                            } else if cp.in_progress {
                                format!(
                                    "En progreso ({}/{} - {})",
                                    cp.current_target_idx,
                                    cp.targets.len(),
                                    cp.last_checkpoint_path.as_deref().unwrap_or("-")
                                )
                            } else {
                                "Pendiente de escaneo inicial".to_string()
                            }
                        );
                    }
                }
            } else {
                println!("Base de datos Redb : No inicializada (ejecute 'dragon sync' o 'dragon ui')");
            }

            if paths.vectors_file.exists() {
                if let Ok(store) = dragon_ai::VectorStore::load_from_file(&paths.vectors_file) {
                    println!("Almacén Vectorial  : {} ({} vectores 256-d indexados)", paths.vectors_file.display(), store.len());
                }
            }

            println!("Servidor MCP       : Habilitado (ejecute 'dragon mcp' para stdio JSON-RPC)");

            #[cfg(windows)]
            {
                println!("Plataforma Windows : Soporte nativo para NTFS MFT, USN Journal y Named Pipe");
                println!("Pipe del Servicio  : \\\\.\\pipe\\dragon_radar_mft");
            }
        }
    }

    Ok(())
}
