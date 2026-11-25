//! dragon-extract: Motor de extracción de texto y contenido sin dependencias pesadas.
//! Provee extractores especializados para PDF, Word (DOCX), Excel (XLSX),
//! PowerPoint (PPTX), Markdown, JSON, código fuente y texto plano.

pub mod office;
pub mod pdf;
pub mod text;

use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    TextOrCode,
    Unsupported,
}

/// Detecta el formato del documento a partir de su extensión.
pub fn detect_format<P: AsRef<Path>>(path: P) -> DocumentFormat {
    let p = path.as_ref();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "pdf" => DocumentFormat::Pdf,
        "docx" => DocumentFormat::Docx,
        "xlsx" => DocumentFormat::Xlsx,
        "pptx" => DocumentFormat::Pptx,
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "json" | "toml" | "yaml"
        | "yml" | "html" | "css" | "xml" | "c" | "cpp" | "h" | "hpp" | "cs" | "java" | "go"
        | "sql" | "sh" | "ps1" | "bat" | "log" | "ini" | "env" | "csv" | "tsv" => {
            DocumentFormat::TextOrCode
        }
        _ => DocumentFormat::Unsupported,
    }
}

/// Extrae el contenido textual de cualquier archivo soportado de forma segura y con límites de tamaño.
pub fn extract_content<P: AsRef<Path>>(path: P, max_bytes: usize) -> Result<String> {
    let p = path.as_ref();
    let format = detect_format(p);

    match format {
        DocumentFormat::Pdf => pdf::extract_pdf_text(p, 50, max_bytes),
        DocumentFormat::Docx => office::extract_docx_text(p, max_bytes),
        DocumentFormat::Xlsx => office::extract_xlsx_text(p, max_bytes),
        DocumentFormat::Pptx => office::extract_pptx_text(p, max_bytes),
        DocumentFormat::TextOrCode => text::extract_plain_text(p, max_bytes),
        DocumentFormat::Unsupported => Ok(String::new()),
    }
}
