//! dragon-mcp: Servidor nativo de Model Context Protocol (MCP) para Dragon Radar Tool.
//! Permite que agentes inteligentes (Antigravity, Claude, Cursor, Gemini) interactúen
//! con el hardware de búsqueda de archivos, indexador Tantivy, almacén Redb y extractor.

pub mod handler;
pub use handler::McpHandler;

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// Ejecuta el bucle de procesamiento MCP sobre los flujos estándar (stdin / stdout).
pub async fn run_stdio_server() -> anyhow::Result<()> {
    log::info!("Dragon Radar MCP Server iniciando sobre stdio...");

    let handler = McpHandler::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                log::error!("JSON inválido recibido: {}. Error: {}", line, e);
                let err_res = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": "Parse error"
                    }
                });
                writeln!(stdout, "{}", err_res.to_string())?;
                stdout.flush()?;
                continue;
            }
        };

        let id = parsed.get("id").cloned();
        let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Manejar notificaciones (sin id)
        if id.is_none() {
            if method == "notifications/initialized" {
                log::info!("Cliente MCP inicializado exitosamente");
            }
            continue;
        }

        let req_id = id.unwrap();

        match method {
            "initialize" => {
                let res = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "dragon-radar-mcp",
                            "version": "0.1.0"
                        }
                    }
                });
                writeln!(stdout, "{}", res.to_string())?;
                stdout.flush()?;
            }
            "ping" => {
                let res = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {}
                });
                writeln!(stdout, "{}", res.to_string())?;
                stdout.flush()?;
            }
            "tools/list" => {
                let res = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "tools": McpHandler::list_tools()
                    }
                });
                writeln!(stdout, "{}", res.to_string())?;
                stdout.flush()?;
            }
            "tools/call" => {
                let params = parsed.get("params").cloned().unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));

                let res = match handler.call_tool(tool_name, &args) {
                    Ok(tool_val) => {
                        let text = serde_json::to_string_pretty(&tool_val).unwrap_or_default();
                        json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": text
                                    }
                                ],
                                "isError": false
                            }
                        })
                    }
                    Err(err) => {
                        json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": format!("Error ejecutando herramienta '{}': {}", tool_name, err)
                                    }
                                ],
                                "isError": true
                            }
                        })
                    }
                };

                writeln!(stdout, "{}", res.to_string())?;
                stdout.flush()?;
            }
            _ => {
                let res = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32601,
                        "message": format!("Método no implementado: {}", method)
                    }
                });
                writeln!(stdout, "{}", res.to_string())?;
                stdout.flush()?;
            }
        }
    }

    Ok(())
}
