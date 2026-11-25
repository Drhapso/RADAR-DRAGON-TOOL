//! Extractor de texto plano y código con detección de codificación (UTF-8, UTF-16, ANSI/Windows-1252).

use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Lee archivos de texto plano o código fuente con límite de tamaño y tolerancia a codificaciones antiguas.
pub fn extract_plain_text<P: AsRef<Path>>(path: P, max_bytes: usize) -> Result<String> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("No se pudo abrir archivo: {:?}", path.as_ref()))?;

    // Leer búfer limitado para evitar agotar memoria con archivos gigantes
    let mut buffer = Vec::with_capacity(std::cmp::min(max_bytes, 65536));
    let mut handle = file.take(max_bytes as u64);
    handle.read_to_end(&mut buffer)?;

    if buffer.is_empty() {
        return Ok(String::new());
    }

    // 1. Intento rápido de UTF-8 directo (el 90%+ de casos en desarrollo)
    if let Ok(valid_utf8) = std::str::from_utf8(&buffer) {
        return Ok(valid_utf8.to_string());
    }

    // 2. Detección y decodificación automática con encoding_rs
    let (encoding, _bom_len) = encoding_rs::Encoding::for_bom(&buffer)
        .unwrap_or((encoding_rs::WINDOWS_1252, 0));

    let (decoded, _malformed, _had_errors) = encoding.decode(&buffer);
    Ok(decoded.into_owned())
}
