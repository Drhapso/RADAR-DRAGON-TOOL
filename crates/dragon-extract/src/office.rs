//! Extractor de texto de paquetes Office OOXML (DOCX, XLSX, PPTX) y Office clásico (.doc, .xls, .ppt).

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Extrae texto plano de un documento Word (.docx), incluyendo cuerpo, encabezados, pies y comentarios.
pub fn extract_docx_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut full_text = String::with_capacity(2048);

    if let Ok(mut doc_xml) = archive.by_name("word/document.xml") {
        let mut xml_content = String::new();
        if doc_xml.read_to_string(&mut xml_content).is_ok() {
            if let Ok(text) = extract_xml_tags(&xml_content, &["w:t", "t"], max_chars) {
                full_text.push_str(&text);
            }
        }
    } else {
        anyhow::bail!("No se encontró word/document.xml en el paquete DOCX");
    }

    for i in 0..archive.len() {
        if full_text.len() >= max_chars {
            break;
        }
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        if (name.starts_with("word/header")
            || name.starts_with("word/footer")
            || name == "word/footnotes.xml"
            || name == "word/comments.xml")
            && name.ends_with(".xml")
        {
            let mut xml_content = String::new();
            if entry.read_to_string(&mut xml_content).is_ok() {
                let rem = max_chars.saturating_sub(full_text.len());
                if let Ok(extra) = extract_xml_tags(&xml_content, &["w:t", "t"], rem) {
                    if !extra.is_empty() {
                        full_text.push('\n');
                        full_text.push_str(&extra);
                    }
                }
            }
        }
    }

    Ok(full_text)
}

/// Extrae texto plano de una hoja de cálculo Excel (.xlsx/.xlsm), leyendo sharedStrings y hojas XML.
pub fn extract_xlsx_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut full_text = String::with_capacity(2048);

    // 1. Cadenas compartidas (opcional en XLSX generados por herramientas externas)
    if let Ok(mut shared) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml_content = String::new();
        if shared.read_to_string(&mut xml_content).is_ok() {
            if let Ok(text) = extract_xml_tags(&xml_content, &["t"], max_chars) {
                full_text.push_str(&text);
            }
        }
    }

    // 2. Celdas inlineStr (<t>) y valores numéricos/fórmulas (<v>) en cada hoja
    for i in 0..archive.len() {
        if full_text.len() >= max_chars {
            break;
        }
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
            let mut xml_content = String::new();
            if entry.read_to_string(&mut xml_content).is_ok() {
                let rem = max_chars.saturating_sub(full_text.len());
                if let Ok(sheet_text) = extract_xml_tags(&xml_content, &["t", "is"], rem) {
                    if !sheet_text.is_empty() {
                        full_text.push('\n');
                        full_text.push_str(&sheet_text);
                    }
                }
            }
        }
    }

    Ok(full_text)
}

/// Extrae texto de presentaciones PowerPoint (.pptx), incluyendo diapositivas y notas del orador.
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

        if (name.starts_with("ppt/slides/slide") || name.starts_with("ppt/notesSlides/notesSlide"))
            && name.ends_with(".xml")
        {
            let mut xml_content = String::new();
            if entry.read_to_string(&mut xml_content).is_ok() {
                if let Ok(slide_text) = extract_xml_tags(
                    &xml_content,
                    &["a:t", "t"],
                    max_chars.saturating_sub(full_text.len()),
                ) {
                    if !slide_text.is_empty() {
                        full_text.push_str(&slide_text);
                        full_text.push('\n');
                    }
                }
            }
        }
    }

    Ok(full_text)
}

/// Extrae texto legible (UTF-16LE y secuencias ASCII/Latin-1 limpias) de archivos Office antiguos (.doc, .xls, .ppt).
pub fn extract_legacy_binary_text<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<String> {
    let file = File::open(path.as_ref())?;
    let mut raw = Vec::new();
    file.take(2 * 1024 * 1024).read_to_end(&mut raw)?;

    let mut out = String::with_capacity(raw.len().min(max_chars).min(4096));

    // 1. Secuencias UTF-16LE (típicas de cadenas Unicode en OLE2 Word/Excel/PowerPoint)
    let mut u16_run = String::new();
    let mut i = 0usize;
    while i + 1 < raw.len() && out.len() < max_chars {
        let code = u16::from_le_bytes([raw[i], raw[i + 1]]);
        if let Some(ch) = char::from_u32(code as u32) {
            if is_clean_legacy_char(ch) {
                u16_run.push(ch);
                i += 2;
                continue;
            }
        }
        flush_legacy_run(&mut out, &mut u16_run, 4, 2, max_chars);
        i += 2;
    }
    flush_legacy_run(&mut out, &mut u16_run, 4, 2, max_chars);

    // 2. Fallback ASCII/Windows-1252 para bloques de texto de 8 bits si aún hay presupuesto
    if out.len() < max_chars / 2 {
        let mut ascii_run = String::new();
        for &b in &raw {
            if out.len() >= max_chars {
                break;
            }
            let ch = if (0x20..=0x7E).contains(&b) {
                Some(b as char)
            } else if b == b'\t' || b == b'\n' {
                Some(' ')
            } else {
                None
            };
            if let Some(c) = ch {
                ascii_run.push(c);
            } else {
                flush_legacy_run(&mut out, &mut ascii_run, 6, 3, max_chars);
            }
        }
        flush_legacy_run(&mut out, &mut ascii_run, 6, 3, max_chars);
    }

    Ok(out)
}

fn is_clean_legacy_char(ch: char) -> bool {
    ch.is_alphanumeric()
        || matches!(
            ch,
            ' ' | '\t' | '.' | ',' | ':' | ';' | '-' | '_' | '/' | '(' | ')' | '¿' | '?' | '¡' | '!' | '%' | '$' | '+'
        )
}

fn flush_legacy_run(
    out: &mut String,
    run: &mut String,
    min_chars: usize,
    min_alpha: usize,
    max_chars: usize,
) {
    let trimmed = run.trim();
    if trimmed.chars().count() >= min_chars
        && trimmed.chars().filter(|c| c.is_alphabetic()).count() >= min_alpha
        && !is_ole_metadata_noise(trimmed)
    {
        if !out.is_empty() && out.len() < max_chars {
            out.push(' ');
        }
        let rem = max_chars.saturating_sub(out.len());
        if rem > 0 {
            let chunk: String = trimmed.chars().take(rem).collect();
            out.push_str(&chunk);
        }
    }
    run.clear();
}

fn is_ole_metadata_noise(s: &str) -> bool {
    matches!(
        s,
        "Root Entry"
            | "WordDocument"
            | "SummaryInformation"
            | "DocumentSummaryInformation"
            | "Workbook"
            | "PowerPoint Document"
            | "Current User"
            | "1Table"
            | "0Table"
            | "CompObj"
    )
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

/// Parsea un string XML y extrae el texto contenido en las etiquetas especificadas soportando anidamiento.
fn extract_xml_tags(xml: &str, target_tags: &[&str], max_chars: usize) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut output = String::with_capacity(2048);
    let mut tag_stack: Vec<bool> = Vec::new();
    let mut active_matches = 0usize;
    let mut buf = Vec::new();

    loop {
        if output.len() >= max_chars {
            break;
        }

        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name_bytes = qname.as_ref();
                let matched = target_tags.iter().any(|&t| t.as_bytes() == name_bytes);
                tag_stack.push(matched);
                if matched {
                    active_matches += 1;
                }
            }
            Ok(Event::End(_)) => {
                if let Some(true) = tag_stack.pop() {
                    active_matches = active_matches.saturating_sub(1);
                }
            }
            Ok(Event::Text(e)) => {
                if active_matches > 0 {
                    if let Ok(text) = e.unescape() {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            output.push_str(trimmed);
                            output.push(' ');
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if active_matches > 0 {
                    let text = String::from_utf8_lossy(e.as_ref());
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        output.push_str(trimmed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_xml_tags_preserve_outer_and_inner_text() {
        let xml = "<root><p>Hola <b>mundo</b> crucial!</p><other>ignorar</other></root>";
        let text = extract_xml_tags(xml, &["p"], 1000).unwrap();
        assert!(text.contains("Hola"));
        assert!(text.contains("mundo"));
        assert!(text.contains("crucial!"));
        assert!(!text.contains("ignorar"));
    }

    #[test]
    fn flush_legacy_run_filters_metadata_and_keeps_real_text() {
        let mut out = String::new();
        let mut run = "WordDocument".to_string();
        flush_legacy_run(&mut out, &mut run, 4, 2, 500);
        assert!(out.is_empty());

        let mut valid = "Contrato de arrendamiento 2026".to_string();
        flush_legacy_run(&mut out, &mut valid, 4, 2, 500);
        assert_eq!(out, "Contrato de arrendamiento 2026");
    }
}
