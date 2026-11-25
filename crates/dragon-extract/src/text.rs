//! Extractor de texto plano y código con detección de codificación (UTF-8, UTF-16, ANSI/Windows-1252).

use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Lee archivos de texto plano o código fuente con límite de tamaño y tolerancia a codificaciones antiguas.
pub fn extract_plain_text<P: AsRef<Path>>(path: P, max_bytes: usize) -> Result<String> {
    let p = path.as_ref();
    let is_ipynb = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ipynb"));

    let read_cap = if is_ipynb {
        max_bytes.saturating_mul(6).min(1_500_000)
    } else {
        max_bytes
    };

    let file = File::open(p).with_context(|| format!("No se pudo abrir archivo: {:?}", p))?;
    let mut buffer = Vec::with_capacity(std::cmp::min(read_cap, 65536));
    let mut handle = file.take(read_cap as u64);
    handle.read_to_end(&mut buffer)?;

    if buffer.is_empty() {
        return Ok(String::new());
    }

    let decoded = decode_bytes(&buffer);
    if is_ipynb {
        return Ok(strip_ipynb_base64(&decoded, max_bytes));
    }

    if decoded.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !decoded.is_char_boundary(end) {
            end -= 1;
        }
        return Ok(decoded[..end].to_string());
    }

    Ok(decoded)
}

fn decode_bytes(buffer: &[u8]) -> String {
    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(buffer) {
        let (decoded, _, _) = encoding.decode(&buffer[bom_len..]);
        return decoded.replace('\0', "");
    }

    if looks_like_utf16le(buffer) {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(buffer);
        return decoded.replace('\0', "");
    }

    if let Ok(valid_utf8) = std::str::from_utf8(buffer) {
        return valid_utf8.replace('\0', "");
    }

    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(buffer);
    decoded.replace('\0', "")
}

fn looks_like_utf16le(buf: &[u8]) -> bool {
    let sample_len = buf.len().min(512);
    if sample_len < 8 {
        return false;
    }
    let pairs = sample_len / 2;
    let odd_zeros = (0..pairs).filter(|&i| buf[i * 2] != 0 && buf[i * 2 + 1] == 0).count();
    odd_zeros * 10 >= pairs * 6
}

fn strip_ipynb_base64(raw: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max_bytes));
    for line in raw.lines() {
        if out.len() >= max_bytes {
            break;
        }
        let trimmed = line.trim();
        if trimmed.contains("\"image/") || (trimmed.len() > 260 && !trimmed.contains(' ')) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
