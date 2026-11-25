//! Extractor de texto para documentos PDF utilizando lopdf.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct PdfPreview {
    pub page_count: usize,
    pub text: String,
}

/// Extrae texto plano de un archivo PDF hasta un número máximo de páginas y caracteres.
pub fn extract_pdf_text<P: AsRef<Path>>(path: P, max_pages: usize, max_chars: usize) -> Result<String> {
    Ok(extract_pdf_preview(path, max_pages, max_chars)?.text)
}

/// Extrae el conteo total de páginas y texto plano de un archivo PDF.
pub fn extract_pdf_preview<P: AsRef<Path>>(
    path: P,
    max_pages: usize,
    max_chars: usize,
) -> Result<PdfPreview> {
    let doc = lopdf::Document::load(path.as_ref())
        .with_context(|| format!("Error al cargar estructura del PDF: {:?}", path.as_ref()))?;

    let pages = doc.get_pages();
    let total_pages = pages.len();
    let mut full_text = String::with_capacity(4096);
    let mut page_count = 0;

    for &page_num in pages.keys() {
        if page_count >= max_pages || full_text.len() >= max_chars {
            break;
        }

        if let Ok(text) = doc.extract_text(&[page_num]) {
            let clean = text.trim();
            if !clean.is_empty() {
                full_text.push_str(clean);
                full_text.push('\n');
            }
        }
        page_count += 1;
    }

    Ok(PdfPreview {
        page_count: total_pages,
        text: full_text,
    })
}
