//! dragon-cli::ui_server - Servidor HTTP asíncrono para la interfaz táctica del Radar del Dragón.
//! Provee comunicación REST instantánea entre el HUD Canvas del navegador y los motores
//! en Rust (Tantivy, Redb, Model2Vec, MFT y Extractor), y ejecuta en segundo plano la
//! indexación automática inicial con checkpoints y validación de arranque.

use crate::indexer::{run_startup_auto_sync_and_validate, SharedRadarState};
use anyhow::Result;
use dragon_ai::HybridRanker;
use dragon_extract::{detect_format, extract_content};
use serde_json::json;
use std::collections::{HashMap, HashSet};
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
    let mut output_items = Vec::new();

    if query.trim().is_empty() {
        return json!([]);
    }

    match mode {
        "filename" => {
            if let Ok(records) = state.storage.search_by_name(query, limit) {
                for rec in records {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let badge = determine_badge(&path_str);
                    output_items.push(json!({
                        "name": rec.name,
                        "path": path_str,
                        "badge": badge,
                        "size": format_file_size(rec.size),
                        "score": "MFT/Redb",
                        "snippet": format!("Archivo indexado en base de datos MFT redb. Tamaño: {}", format_file_size(rec.size))
                    }));
                }
            }
        }
        "content" => {
            if let Ok(hits) = state.index_engine.search(query, limit) {
                for hit in hits {
                    let badge = determine_badge(&hit.path);
                    output_items.push(json!({
                        "name": hit.filename,
                        "path": hit.path,
                        "badge": badge,
                        "size": "Indexed",
                        "score": format!("{:.2}", hit.score),
                        "snippet": format!("Coincidencia de contenido Tantivy BM25 con relevancia {:.2}", hit.score)
                    }));
                }
            }
        }
        "semantic" => {
            let q_vec = state.embedder.embed(query);
            if let Ok(vs) = state.vector_store.read() {
                let hits = vs.search(&q_vec, limit);
                for hit in hits {
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
                }
            }
        }
        _ => {
            let mut seen_paths = HashSet::new();

            // 1. Coincidencias directas por nombre de archivo en Redb
            if let Ok(records) = state.storage.search_by_name(query, 10) {
                for rec in records {
                    let path_str = rec.path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    if seen_paths.insert(path_str.clone()) {
                        let badge = determine_badge(&path_str);
                        output_items.push(json!({
                            "name": rec.name,
                            "path": path_str,
                            "badge": badge,
                            "size": format_file_size(rec.size),
                            "score": "1.00 (Nombre)",
                            "snippet": format!("Coincidencia exacta por nombre de archivo en MFT/Redb. Tamaño: {}", format_file_size(rec.size))
                        }));
                    }
                }
            }

            // 2. Fusión RRF (BM25 + Semántica)
            let bm25_hits = state.index_engine.search(query, limit * 2).unwrap_or_default();
            let q_vec = state.embedder.embed(query);
            let semantic_hits = state
                .vector_store
                .read()
                .map(|vs| vs.search(&q_vec, limit * 2))
                .unwrap_or_default();

            let fused = HybridRanker::fuse(&bm25_hits, &semantic_hits, 1.0, 1.0, limit);
            for hit in fused {
                if seen_paths.insert(hit.path.clone()) {
                    let badge = determine_badge(&hit.path);
                    let snippet = if !hit.snippet.is_empty() {
                        hit.snippet
                    } else {
                        format!("Puntuación combinada RRF: {:.4} (BM25: {:.2})", hit.rrf_score, hit.bm25_score)
                    };

                    output_items.push(json!({
                        "name": hit.filename,
                        "path": hit.path,
                        "badge": badge,
                        "size": "Hybrid",
                        "score": format!("{:.4}", hit.rrf_score),
                        "snippet": snippet
                    }));
                }
            }
        }
    }

    output_items.truncate(limit);
    json!(output_items)
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
    let lower = path.to_lowercase();
    if lower.ends_with(".rs") {
        "RUST"
    } else if lower.ends_with(".toml") {
        "TOML"
    } else if lower.ends_with(".pdf") {
        "PDF"
    } else if lower.ends_with(".docx") || lower.ends_with(".doc") {
        "WORD"
    } else if lower.ends_with(".xlsx") {
        "EXCEL"
    } else if lower.ends_with(".js") || lower.ends_with(".ts") {
        "JS"
    } else if lower.ends_with(".md") {
        "DOC"
    } else if lower.ends_with(".exe") {
        "BIN"
    } else {
        "FILE"
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
