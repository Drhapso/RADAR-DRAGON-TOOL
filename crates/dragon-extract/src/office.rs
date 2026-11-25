//! Extractor de texto de paquetes Office OOXML (DOCX, XLSX, PPTX) mediante zip y quick-xml.

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Extrae texto plano de un documento Word (.docx).
pub fn extract_docx_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    // El contenido principal en DOCX reside en word/document.xml
    let mut doc_xml = archive
        .by_name("word/document.xml")
        .context("No se encontró word/document.xml en el paquete DOCX")?;

    let mut xml_content = String::new();
    doc_xml.read_to_string(&mut xml_content)?;

    extract_xml_tags(&xml_content, &["w:t", "t"], max_chars)
}

/// Extrae texto plano de una hoja de cálculo Excel (.xlsx).
pub fn extract_xlsx_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    // En XLSX las cadenas únicas se almacenan en xl/sharedStrings.xml
    let mut doc_xml = archive
        .by_name("xl/sharedStrings.xml")
        .context("No se encontró xl/sharedStrings.xml en el paquete XLSX")?;

    let mut xml_content = String::new();
    doc_xml.read_to_string(&mut xml_content)?;

    extract_xml_tags(&xml_content, &["t"], max_chars)
}

/// Extrae texto de presentaciones PowerPoint (.pptx).
pub fn extract_pptx_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut full_text = String::new();

    for i in 0..archive.len() {
        if full_text.len() >= max_chars {
            break;
        }

        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();

        if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
            let mut xml_content = String::new();
            if entry.read_to_string(&mut xml_content).is_ok() {
                if let Ok(slide_text) = extract_xml_tags(&xml_content, &["a:t", "t"], max_chars.saturating_sub(full_text.len())) {
                    full_text.push_str(&slide_text);
                    full_text.push('\n');
                }
            }
        }
    }

    Ok(full_text)
}

/// Parsea un string XML y extrae el texto contenido en las etiquetas especificadas.
fn extract_xml_tags(xml: &str, target_tags: &[&str], max_chars: usize) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut output = String::with_capacity(2048);
    let mut in_target_tag = false;
    let mut buf = Vec::new();

    loop {
        if output.len() >= max_chars {
            break;
        }

        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name_bytes = qname.as_ref();
                in_target_tag = target_tags.iter().any(|&t| t.as_bytes() == name_bytes);
            }
            Ok(Event::End(ref e)) => {
                let qname = e.name();
                let name_bytes = qname.as_ref();
                if target_tags.iter().any(|&t| t.as_bytes() == name_bytes) {
                    in_target_tag = false;
                }
            }
            Ok(Event::Text(e)) => {
                if in_target_tag {
                    if let Ok(text) = e.unescape() {
                        output.push_str(&text);
                        output.push(' ');
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                log::debug!("Error parcial parseando XML: {:?}", e);
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(output)
}
