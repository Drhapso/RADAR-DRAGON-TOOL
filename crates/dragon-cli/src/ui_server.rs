//! dragon-cli::ui_server - Servidor HTTP asíncrono para la interfaz táctica del Radar del Dragón.
//! Provee comunicación REST instantánea entre el HUD Canvas del navegador y los motores
//! en Rust (Tantivy, Redb, Model2Vec, MFT y Extractor), y ejecuta en segundo plano la
//! indexación automática inicial con checkpoints y validación de arranque.

use crate::indexer::{run_startup_auto_sync_and_validate, SharedRadarState};
use anyhow::Result;
use dragon_core::{classify_path, is_noisy_build_or_cache_path, FileCategory};
use dragon_extract::{detect_format, extract_content};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Resuelve rutas de datos buscando en '.', '../..', o relativo al ejecutable.
pub fn resolve_data_path(rel_path: &str) -> PathBuf {
    let p = Path::new(rel_path);
    if p.exists() {
        return p.to_path_buf();
    }
    let up2 = Path::new("../../").join(rel_path);
    if up2.exists() || Path::new("../../Cargo.toml").exists() {
        return up2;
    }
    let up1 = Path::new("../").join(rel_path);
    if up1.exists() {
        return up1;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let next_to_exe = dir.join(rel_path);
            if next_to_exe.exists() {
                return next_to_exe;
            }
            if let Some(repo_root) = dir.parent().and_then(|p| p.parent()) {
                let from_repo = repo_root.join(rel_path);
                if from_repo.exists() || repo_root.join("Cargo.toml").exists() {
                    return from_repo;
                }
            }
        }
    }
    p.to_path_buf()
}

pub async fn run_ui_server(mut port: u16) -> Result<()> {
    let index_dir = resolve_data_path(".dragon-index");
    let db_path = resolve_data_path(".dragon-data/radar.redb");
    let vectors_file = resolve_data_path(".dragon-data/vectors.bin");

    let state = SharedRadarState::open(&index_dir, &db_path, &vectors_file)?;

    // Regla de inicio:
    // 1. Primera ejecución -> indexación automática de todas las unidades con checkpoints.
    // 2. Interrupción previa -> reanudación desde el último punto de control.
    // 3. Arranques posteriores -> validación de nuevos, modificados por peso/fecha, movidos y eliminados.
    let bg_state = Arc::clone(&state);
    std::thread::spawn(move || {
        if let Err(e) = run_startup_auto_sync_and_validate(&bg_state, false) {
            log::error!("Error en sincronización automática de inicio: {}", e);
        }
    });

    let mut listener = None;
    for p in port..port + 30 {
        let addr = SocketAddr::from(([127, 0, 0, 1], p));
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
        anyhow::anyhow!("No se pudo enlazar ningún puerto entre {} y {}", port, port + 30)
    })?;

    let url = format!("http://127.0.0.1:{}", port);
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║       DRAGON RADAR TOOL - TACTICAL HUD UI SERVER         ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("  ⚡ Radar activado en: {}", url);
    println!("  ⚡ Sincronización y validación de índice activa en segundo plano");
    println!("  ⚡ Abriendo el navegador automáticamente...");
    println!("  ⚡ (Mantén esta ventana abierta mientras usas el radar)\n");

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", &url])
            .spawn();
    }

    loop {
        let (socket, _) = listener.accept().await?;
        let conn_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, conn_state).await {
                log::debug!("Error en conexión HTTP: {}", e);
            }
        });
    }
}

async fn handle_connection(mut socket: TcpStream, state: Arc<SharedRadarState>) -> Result<()> {
    let mut buffer = [0u8; 4096];
    let bytes_read = socket.read(&mut buffer).await?;
    if bytes_read == 0 {
        return Ok(());
    }

    let req_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = req_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        return Ok(());
    }

    let method = parts[0];
    let full_path = parts[1];
    let (path, query_params) = parse_url(full_path);

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
        ("GET", "/api/status") => {
            let records_count = state.storage.count_records().unwrap_or(0);
            let tracked_files = state.storage.count_index_states().unwrap_or(0);
            let vectors_count = state.vector_store.read().map(|vs| vs.len()).unwrap_or(0);
            let telemetry = state.telemetry.read().map(|t| t.clone()).unwrap_or_default();

            let res = json!({
                "status": "OPERATIONAL",
                "mft_armed": true,
                "redb_records": records_count,
                "tracked_files": tracked_files,
                "vector_count": vectors_count,
                "phase": telemetry.phase,
                "target_index": telemetry.target_index,
                "total_targets": telemetry.total_targets,
                "current_target": telemetry.current_target,
                "new_files": telemetry.new_files,
                "updated_files": telemetry.updated_files,
                "moved_files": telemetry.moved_files,
                "deleted_files": telemetry.deleted_files,
                "skipped_files": telemetry.skipped_files,
                "last_checkpoint": telemetry.last_checkpoint,
                "initial_scan_completed": telemetry.initial_scan_completed,
                "version": "0.1.0-MFT"
            });
            send_response(&mut socket, "200 OK", "application/json", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/search") => {
            let q = query_params.get("q").cloned().unwrap_or_default();
            let mode = query_params.get("mode").cloned().unwrap_or_else(|| "all".to_string());
            let limit = query_params.get("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(20);

            let results = execute_search(&state, &q, &mode, limit);
            send_response(&mut socket, "200 OK", "application/json", results.to_string().as_bytes()).await?;
        }
        ("GET", "/api/extract") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
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
            send_response(&mut socket, "200 OK", "application/json", res.to_string().as_bytes()).await?;
        }
        ("GET", "/api/open") => {
            let file = query_params.get("file").cloned().unwrap_or_default();
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("explorer")
                    .args(["/select,", &file])
                    .spawn();
            }
            send_response(&mut socket, "200 OK", "application/json", b"{\"opened\":true}").await?;
        }
        _ => {
            send_response(&mut socket, "404 Not Found", "text/plain", b"Ruta no encontrada").await?;
        }
    }

    Ok(())
}

fn execute_search(state: &SharedRadarState, query: &str, mode: &str, limit: usize) -> serde_json::Value {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return json!([]);
    }

    let mut output_items = Vec::new();

    match mode {
        "filename" => {
            // Búsqueda por nombre en todas las categorías (incluyendo sistema y binarios por nombre únicamente)
            if let Ok(records) = state.storage.search_by_name_ranked(trimmed, None, limit) {
                for (rec, score) in records {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let badge = determine_badge(&path_str);
                    output_items.push(json!({
                        "name": rec.name,
                        "path": path_str,
                        "badge": badge,
                        "size": format_file_size(rec.size),
                        "score": format!("{:.0} (Nombre)", score),
                        "snippet": format!("Coincidencia por nombre ({:?}). Tamaño: {}", classify_path(&path_str), format_file_size(rec.size))
                    }));
                }
            }
        }
        "media" => {
            // Multimedia (audio y video conocidos) -> únicamente por nombre
            if let Ok(records) = state
                .storage
                .search_by_name_ranked(trimmed, Some(FileCategory::Media), limit)
            {
                for (rec, score) in records {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let badge = determine_badge(&path_str);
                    output_items.push(json!({
                        "name": rec.name,
                        "path": path_str,
                        "badge": badge,
                        "size": format_file_size(rec.size),
                        "score": format!("{:.0} (Media)", score),
                        "snippet": format!("Archivo multimedia indexado por nombre. Tamaño: {}", format_file_size(rec.size))
                    }));
                }
            }
        }
        "design" => {
            // Imágenes, vectores, CAD, 3D y archivos de diseño -> únicamente por nombre
            if let Ok(records) = state
                .storage
                .search_by_name_ranked(trimmed, Some(FileCategory::ImageOrDesign), limit)
            {
                for (rec, score) in records {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let badge = determine_badge(&path_str);
                    output_items.push(json!({
                        "name": rec.name,
                        "path": path_str,
                        "badge": badge,
                        "size": format_file_size(rec.size),
                        "score": format!("{:.0} (Diseño)", score),
                        "snippet": format!("Archivo gráfico/vectorial/diseño indexado por nombre. Tamaño: {}", format_file_size(rec.size))
                    }));
                }
            }
        }
        "content" => {
            // Búsqueda estricta de contenido BM25 sobre Documentos conocidos
            if let Ok(hits) = state.index_engine.search(trimmed, limit) {
                for hit in hits {
                    let badge = determine_badge(&hit.path);
                    output_items.push(json!({
                        "name": hit.filename,
                        "path": hit.path,
                        "badge": badge,
                        "size": "Documento",
                        "score": format!("{:.2}", hit.score),
                        "snippet": format!("Coincidencia de contenido Tantivy BM25 con relevancia {:.2}", hit.score)
                    }));
                }
            }
        }
        "semantic" => {
            let q_vec = state.embedder.embed(trimmed);
            if let Ok(vs) = state.vector_store.read() {
                let hits = vs.search(&q_vec, limit * 2);
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
            // Documentos conocidos por nombre OR contenido + semántica de alta confianza
            let ranked = execute_unified_ranked(state, trimmed, Some(FileCategory::Document), limit);
            return json!(ranked);
        }
        _ => {
            // Modo "all" (Híbrido):
            // - Documentos por nombre o contenido + semántica filtrada
            // - Multimedia e Imágenes/Diseño por nombre
            // - Sistema/Binarios por nombre únicamente (menor prioridad que archivos de usuario)
            let ranked = execute_unified_ranked(state, trimmed, None, limit);
            return json!(ranked);
        }
    }

    output_items.truncate(limit);
    json!(output_items)
}

/// Motor de ranking unificado de alta precisión combinando coincidencia exacta de nombre, BM25 de contenido y vectores confiables.
fn execute_unified_ranked(
    state: &SharedRadarState,
    query: &str,
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

    // 1. Coincidencias por nombre en Redb (ordenadas por precisión)
    if let Ok(name_hits) = state.storage.search_by_name_ranked(query, category_filter, limit * 2) {
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

    // 2. Búsqueda de contenido BM25 (solo aplica si no se filtró a Multimedia/Diseño)
    let allow_content = matches!(category_filter, None | Some(FileCategory::Document));
    if allow_content {
        if let Ok(bm25_hits) = state.index_engine.search(query, limit * 2) {
            for hit in bm25_hits {
                let key = norm_key(&hit.path);
                let bm25_boost = (hit.score * 25.0).min(450.0);
                by_path
                    .entry(key)
                    .and_modify(|c| {
                        c.score += bm25_boost;
                        c.snippet = format!("Nombre + Contenido BM25 (Relevancia {:.2})", hit.score);
                    })
                    .or_insert_with(|| MergedCandidate {
                        name: hit.filename,
                        path: hit.path,
                        size_label: "Documento".to_string(),
                        score: 120.0 + bm25_boost,
                        snippet: format!("Contenido de documento BM25 (Relevancia {:.2})", hit.score),
                    });
            }
        }

        // 3. Búsqueda semántica solo con umbral estricto (>= 0.28) para evitar ruido irrelevante
        let q_vec = state.embedder.embed(query);
        if let Ok(vs) = state.vector_store.read() {
            let sem_hits = vs.search(&q_vec, limit * 2);
            for hit in sem_hits {
                if hit.score < 0.28 || classify_path(&hit.path) != FileCategory::Document {
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

    let mut candidates: Vec<MergedCandidate> = by_path.into_values().collect();
    candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(limit);

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
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                result.push(val as char);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(' ');
            i += 1;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

async fn send_response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );

    socket.write_all(response.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}
