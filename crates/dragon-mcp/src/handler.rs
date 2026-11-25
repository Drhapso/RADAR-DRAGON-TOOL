//! dragon-mcp::handler - Implementación de herramientas del protocolo MCP.

use anyhow::Result;
use dragon_ai::{DragonEmbedder, HybridRanker, VectorStore};
use dragon_extract::{detect_format, extract_content};
use dragon_index::DragonIndexEngine;
use dragon_mft::{UnifiedScanResult, UnifiedScanner};
use dragon_storage::DragonStorage;
use serde_json::{json, Value};
use std::path::Path;

pub struct McpHandler {
    embedder: DragonEmbedder,
}

impl Default for McpHandler {
    fn default() -> Self {
        Self {
            embedder: DragonEmbedder::load_or_detect::<&str>(None),
        }
    }
}

impl McpHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lista de herramientas ofrecidas por Dragon Radar al agente IA.
    pub fn list_tools() -> Value {
        json!([
            {
                "name": "dragon_find_files",
                "description": "Encuentra instantáneamente archivos en disco por nombre o extensión mediante escaneo MFT ultra-rápido o fallback multi-hilo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "Ruta o unidad a escanear (ej. 'C', 'C:\\Users', '.')"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "Subcadena o extensión opcional a filtrar (ej. '.pdf', 'Cargo')"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Número máximo de archivos a retornar (default: 25)"
                        }
                    },
                    "required": ["target"]
                }
            },
            {
                "name": "dragon_search_content",
                "description": "Busca texto exacto o palabras clave dentro de archivos y documentos (PDF, DOCX, código) indexados con Tantivy BM25.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Término o consulta de texto a buscar"
                        },
                        "index_dir": {
                            "type": "string",
                            "description": "Directorio de índice Tantivy (default: '.dragon-index')"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Límite de resultados (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "dragon_semantic_search",
                "description": "Búsqueda semántica conceptual sobre contenidos indexados utilizando embeddings locales Model2Vec de 256 dimensiones.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Concepto o frase semántica (ej. 'estados de cuenta bancarios', 'configuración de base de datos')"
                        },
                        "vectors_file": {
                            "type": "string",
                            "description": "Ruta al almacén de vectores (default: '.dragon-data/vectors.bin')"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Límite de resultados (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "dragon_hybrid_search",
                "description": "Búsqueda híbrida de alta precisión fusionando BM25 y similitud vectorial con Reciprocal Rank Fusion (RRF).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Consulta de búsqueda híbrida"
                        },
                        "index_dir": {
                            "type": "string",
                            "description": "Directorio de índice Tantivy (default: '.dragon-index')"
                        },
                        "vectors_file": {
                            "type": "string",
                            "description": "Ruta al almacén vectorial (default: '.dragon-data/vectors.bin')"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Límite de resultados (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "dragon_extract_document",
                "description": "Extrae texto limpio y estructurado de documentos PDF, Word (.docx), Excel (.xlsx), PowerPoint (.pptx) o código.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Ruta absoluta o relativa del archivo a examinar"
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "Máximo de caracteres a extraer (default: 30000)"
                        }
                    },
                    "required": ["file_path"]
                }
            },
            {
                "name": "dragon_status",
                "description": "Retorna el estado del sistema Dragon Radar: base de datos Redb, índices Tantivy, vectores IA y privilegios MFT.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }
        ])
    }

    /// Ejecuta una herramienta por nombre con los argumentos proporcionados.
    pub fn call_tool(&self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "dragon_find_files" => {
                let target = args.get("target").and_then(|v| v.as_str()).unwrap_or(".");
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(25) as usize;

                let scan_res = UnifiedScanner::scan(target).map_err(|e| anyhow::anyhow!(e))?;
                let mut matches = Vec::new();

                match scan_res {
                    #[cfg(windows)]
                    UnifiedScanResult::Mft(mft) => {
                        for (&id, &(_, ref name, attr)) in &mft.records {
                            if matches.len() >= limit {
                                break;
                            }
                            let is_dir = (attr & dragon_core::FileRecord::ATTR_DIRECTORY) != 0;
                            if !is_dir && (pattern.is_empty() || name.to_lowercase().contains(&pattern.to_lowercase())) {
                                if let Some(path) = mft.resolve_path(id) {
                                    matches.push(json!({
                                        "id": id,
                                        "name": name,
                                        "path": path.to_string_lossy(),
                                        "is_directory": false
                                    }));
                                }
                            }
                        }
                    }
                    UnifiedScanResult::Fallback(fallback) => {
                        for rec in &fallback.records {
                            if matches.len() >= limit {
                                break;
                            }
                            if !rec.is_directory && (pattern.is_empty() || rec.name.to_lowercase().contains(&pattern.to_lowercase())) {
                                matches.push(json!({
                                    "id": rec.id,
                                    "name": rec.name,
                                    "path": rec.path.as_deref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                                    "is_directory": rec.is_directory,
                                    "size": rec.size
                                }));
                            }
                        }
                    }
                }

                Ok(json!({
                    "total_matches": matches.len(),
                    "files": matches
                }))
            }
            "dragon_search_content" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let index_dir = args.get("index_dir").and_then(|v| v.as_str()).unwrap_or(".dragon-index");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                let engine = DragonIndexEngine::open_or_create(index_dir)?;
                let hits = engine.search(query, limit)?;

                let results: Vec<Value> = hits.into_iter().map(|h| json!({
                    "score": h.score,
                    "filename": h.filename,
                    "path": h.path,
                })).collect();

                Ok(json!({
                    "query": query,
                    "results_count": results.len(),
                    "hits": results
                }))
            }
            "dragon_semantic_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let vectors_file = args.get("vectors_file").and_then(|v| v.as_str()).unwrap_or(".dragon-data/vectors.bin");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                let store = VectorStore::load_from_file(vectors_file)?;
                let q_vec = self.embedder.embed(query);
                let hits = store.search(&q_vec, limit);

                let results: Vec<Value> = hits.into_iter().map(|h| json!({
                    "similarity": format!("{:.2}%", h.score * 100.0),
                    "path": h.path,
                    "snippet": h.snippet
                })).collect();

                Ok(json!({
                    "query": query,
                    "model": if self.embedder.is_real_model() { "Model2Vec (256-d)" } else { "HashEmbedder (256-d)" },
                    "results_count": results.len(),
                    "hits": results
                }))
            }
            "dragon_hybrid_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let index_dir = args.get("index_dir").and_then(|v| v.as_str()).unwrap_or(".dragon-index");
                let vectors_file = args.get("vectors_file").and_then(|v| v.as_str()).unwrap_or(".dragon-data/vectors.bin");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                let engine = DragonIndexEngine::open_or_create(index_dir)?;
                let bm25_hits = engine.search(query, limit * 2).unwrap_or_default();

                let semantic_hits = if let Ok(store) = VectorStore::load_from_file(vectors_file) {
                    let q_vec = self.embedder.embed(query);
                    store.search(&q_vec, limit * 2)
                } else {
                    Vec::new()
                };

                let fused = HybridRanker::fuse(&bm25_hits, &semantic_hits, 1.0, 1.0, limit);

                let results: Vec<Value> = fused.into_iter().map(|h| json!({
                    "rrf_score": h.rrf_score,
                    "bm25_score": h.bm25_score,
                    "semantic_score": format!("{:.1}%", h.semantic_score * 100.0),
                    "filename": h.filename,
                    "path": h.path,
                    "snippet": h.snippet
                })).collect();

                Ok(json!({
                    "query": query,
                    "results_count": results.len(),
                    "hits": results
                }))
            }
            "dragon_extract_document" => {
                let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                let max_chars = args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(30000) as usize;

                let path = Path::new(file_path);
                if !path.exists() {
                    anyhow::bail!("El archivo no existe: {}", file_path);
                }

                let format = detect_format(path);
                let content = extract_content(path, max_chars)?;

                Ok(json!({
                    "file_path": file_path,
                    "format": format!("{:?}", format),
                    "extracted_chars": content.len(),
                    "content": content
                }))
            }
            "dragon_status" => {
                let default_db = ".dragon-data/radar.redb";
                let records_count = if Path::new(default_db).exists() {
                    DragonStorage::open(default_db).and_then(|s| s.count_records()).unwrap_or(0)
                } else {
                    0
                };

                let vectors_file = ".dragon-data/vectors.bin";
                let vectors_count = if Path::new(vectors_file).exists() {
                    VectorStore::load_from_file(vectors_file).map(|s| s.len()).unwrap_or(0)
                } else {
                    0
                };

                Ok(json!({
                    "version": "0.1.0",
                    "status": "operational",
                    "architecture": std::env::consts::ARCH,
                    "os": std::env::consts::OS,
                    "redb_records": records_count,
                    "vector_store_units": vectors_count,
                    "ai_model": if self.embedder.is_real_model() { "Model2Vec (256-d weights)" } else { "HashEmbedder (256-d)" },
                    "pipe_channel": "\\\\.\\pipe\\dragon_radar_mft"
                }))
            }
            _ => anyhow::bail!("Herramienta desconocida: {}", name),
        }
    }
}
