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

/// Extrae texto de documentos OpenDocument (.odt, .ods, .odp) leyendo `content.xml`.
pub fn extract_opendocument_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut content_xml = archive
        .by_name("content.xml")
        .context("No se encontró content.xml en paquete OpenDocument")?;

    let mut xml_content = String::new();
    content_xml.read_to_string(&mut xml_content)?;
    extract_xml_tags(&xml_content, &["text:p", "text:h", "text:span"], max_chars)
}

/// Extrae texto limpio de libros electrónicos EPUB leyendo archivos XHTML/HTML del paquete ZIP.
pub fn extract_epub_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut full_text = String::new();

    for i in 0..archive.len() {
        if full_text.len() >= max_chars {
            break;
        }
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm") {
            let mut html = String::new();
            if entry.read_to_string(&mut html).is_ok() {
                if let Ok(chunk) = extract_xml_tags(
                    &html,
                    &["p", "h1", "h2", "h3", "h4", "li", "span", "div"],
                    max_chars.saturating_sub(full_text.len()),
                ) {
                    full_text.push_str(&chunk);
                    full_text.push('\n');
                }
            }
        }
    }
    Ok(full_text)
}

/// Extrae texto plano básico desde un archivo RTF omitiendo comandos de control.
pub fn extract_rtf_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let raw = std::fs::read(path.as_ref())?;
    let text = String::from_utf8_lossy(&raw);
    let mut out = String::with_capacity(text.len().min(max_chars));
    let mut chars = text.chars().peekable();
    let mut skip_group = 0usize;

    while let Some(ch) = chars.next() {
        if out.len() >= max_chars {
            break;
        }
        match ch {
            '{' => {
                if let Some(&'\\') = chars.peek() {
                    // Detectar grupos de destino especiales (\fonttbl, \colortbl, \stylesheet, \*\...)
                    let rest: String = chars.clone().take(15).collect();
                    if rest.starts_with("\\*")
                        || rest.starts_with("\\fonttbl")
                        || rest.starts_with("\\colortbl")
                        || rest.starts_with("\\stylesheet")
                        || rest.starts_with("\\info")
                    {
                        skip_group += 1;
                    } else if skip_group > 0 {
                        skip_group += 1;
                    }
                } else if skip_group > 0 {
                    skip_group += 1;
                }
            }
            '}' => {
                skip_group = skip_group.saturating_sub(1);
            }
            '\\' => {
                if let Some(&next) = chars.peek() {
                    if next == '\'' {
                        chars.next();
                        let _ = chars.next();
                        let _ = chars.next();
                        continue;
                    }
                    if next == '\\' || next == '{' || next == '}' {
                        if skip_group == 0 {
                            out.push(next);
                        }
                        chars.next();
                        continue;
                    }
                }
                let mut cmd = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        cmd.push(c);
                        chars.next();
                    } else {
                        if c == ' ' {
                            chars.next();
                        }
                        break;
                    }
                }
                if skip_group == 0 && (cmd == "par" || cmd == "line") {
                    out.push('\n');
                } else if skip_group == 0 && cmd == "tab" {
                    out.push(' ');
                }
            }
            '\r' | '\n' => {}
            _ => {
                if skip_group == 0 {
                    out.push(ch);
                }
            }
        }
    }
    Ok(out)
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
