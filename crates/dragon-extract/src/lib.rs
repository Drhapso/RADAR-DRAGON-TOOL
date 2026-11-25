//! dragon-extract: Motor de extracción de texto y contenido sin dependencias pesadas.
//! Provee extractores especializados para PDF, Word (DOCX), Excel (XLSX),
//! PowerPoint (PPTX), Markdown, JSON, código fuente y texto plano.

pub mod office;
pub mod office_visual;
pub mod pdf;
pub mod text;

pub use office::{
    extract_docx_paragraphs, extract_pptx_slides, extract_xlsx_sheets, extract_zip_entries,
    ArchiveEntryPreview, SheetPreview, SlidePreview,
};
pub use office_visual::{
    extract_excel_visual, extract_pptx_visual, extract_word_visual, read_zip_entry_bytes,
    ExcelCell, ExcelSheetVisual, ExcelVisualPreview, PptElement, PptParagraph, PptSlideVisual,
    PptVisualPreview, WordBlock, WordParagraph, WordRun, WordTableCell, WordVisualPreview,
};
pub use pdf::{extract_pdf_preview, PdfPreview};

use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    LegacyOffice,
    OpenDocument,
    Epub,
    Rtf,
    TextOrCode,
    Unsupported,
}

/// Detecta el formato de documento extraíble a partir de su extensión.
pub fn detect_format<P: AsRef<Path>>(path: P) -> DocumentFormat {
    let p = path.as_ref();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "pdf" => DocumentFormat::Pdf,
        "docx" | "docm" | "dotx" | "dotm" => DocumentFormat::Docx,
        "xlsx" | "xlsm" => DocumentFormat::Xlsx,
        "pptx" | "pptm" | "ppsx" => DocumentFormat::Pptx,
        "doc" | "xls" | "ppt" => DocumentFormat::LegacyOffice,
        "odt" | "ott" | "ods" | "odp" => DocumentFormat::OpenDocument,
        "epub" => DocumentFormat::Epub,
        "rtf" => DocumentFormat::Rtf,
        "txt" | "md" | "markdown" | "rst" | "adoc" | "tex" | "org" | "log"
        | "json" | "jsonl" | "ndjson" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg"
        | "conf" | "env" | "properties" | "sql" | "graphql" | "proto" | "csv" | "tsv"
        | "html" | "htm" | "xhtml" | "css" | "scss" | "sass" | "less"
        | "js" | "mjs" | "cjs" | "ts" | "mts" | "jsx" | "tsx" | "vue" | "svelte" | "astro"
        | "php" | "rs" | "py" | "pyw" | "ipynb" | "java" | "kt" | "kts" | "scala"
        | "go" | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx" | "cs" | "fs" | "vb"
        | "rb" | "pl" | "lua" | "r" | "dart" | "swift" | "m" | "mm"
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd"
        | "gradle" | "cmake" => DocumentFormat::TextOrCode,
        _ => DocumentFormat::Unsupported,
    }
}

/// Extrae el contenido textual de cualquier archivo soportado de forma segura y con límites de tamaño.
pub fn extract_content<P: AsRef<Path>>(path: P, max_bytes: usize) -> Result<String> {
    let p = path.as_ref();
    let format = detect_format(p);

    match format {
        DocumentFormat::Pdf => pdf::extract_pdf_text(p, 300, max_bytes),
        DocumentFormat::Docx => office::extract_docx_text(p, max_bytes),
        DocumentFormat::Xlsx => office::extract_xlsx_text(p, max_bytes),
        DocumentFormat::Pptx => office::extract_pptx_text(p, max_bytes),
        DocumentFormat::LegacyOffice => office::extract_legacy_binary_text(p, max_bytes),
        DocumentFormat::OpenDocument => office::extract_opendocument_text(p, max_bytes),
        DocumentFormat::Epub => office::extract_epub_text(p, max_bytes),
        DocumentFormat::Rtf => office::extract_rtf_text(p, max_bytes),
        DocumentFormat::TextOrCode => text::extract_plain_text(p, max_bytes),
        DocumentFormat::Unsupported => Ok(String::new()),
    }
}
