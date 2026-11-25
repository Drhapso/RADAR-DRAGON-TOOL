//! Extractor de texto de paquetes Office OOXML (DOCX, XLSX, PPTX) y Office clásico (.doc, .xls, .ppt).

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct SheetPreview {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlidePreview {
    pub number: usize,
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveEntryPreview {
    pub path: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
}

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

/// Extrae párrafos estructurados de un documento Word (.docx) preservando saltos de párrafo.
pub fn extract_docx_paragraphs<P: AsRef<Path>>(path: P, max_chars: usize) -> Result<Vec<String>> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut doc_xml = archive
        .by_name("word/document.xml")
        .context("No se encontró word/document.xml en el paquete DOCX")?;
    let mut xml_content = String::new();
    doc_xml.read_to_string(&mut xml_content)?;
    extract_xml_paragraphs(&xml_content, b"w:p", &[b"w:t", b"t"], max_chars)
}

/// Extrae diapositivas estructuradas de una presentación PowerPoint (.pptx).
pub fn extract_pptx_slides<P: AsRef<Path>>(path: P, max_slides: usize) -> Result<Vec<SlidePreview>> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut slide_indices: Vec<(usize, usize)> = Vec::new();

    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name();
        if let Some(rest) = name.strip_prefix("ppt/slides/slide").and_then(|s| s.strip_suffix(".xml")) {
            if let Ok(num) = rest.parse::<usize>() {
                slide_indices.push((num, i));
            }
        }
    }
    slide_indices.sort_by_key(|&(num, _)| num);
    slide_indices.truncate(max_slides);

    let mut slides = Vec::with_capacity(slide_indices.len());
    for (num, idx) in slide_indices {
        let Ok(mut entry) = archive.by_index(idx) else {
            continue;
        };
        let mut xml_content = String::new();
        if entry.read_to_string(&mut xml_content).is_err() {
            continue;
        }
        let paras = extract_xml_paragraphs(&xml_content, b"a:p", &[b"a:t", b"t"], 8_000)
            .unwrap_or_default();
        let title = paras
            .first()
            .cloned()
            .unwrap_or_else(|| format!("Diapositiva {}", num));
        let lines = if paras.len() > 1 {
            paras.into_iter().skip(1).collect()
        } else {
            Vec::new()
        };
        slides.push(SlidePreview {
            number: num,
            title,
            lines,
        });
    }

    Ok(slides)
}

/// Extrae hojas de cálculo (.xlsx / .xlsm) en formato tabular por hoja para vista previa.
pub fn extract_xlsx_sheets<P: AsRef<Path>>(
    path: P,
    max_sheets: usize,
    max_rows_per_sheet: usize,
    max_cols: usize,
) -> Result<Vec<SheetPreview>> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    // 1. Tabla sharedStrings
    let shared_strings = if let Ok(mut shared) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml = String::new();
        if shared.read_to_string(&mut xml).is_ok() {
            parse_xlsx_shared_strings(&xml)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // 2. Nombres de hojas desde workbook.xml + workbook.xml.rels
    let mut targets = resolve_xlsx_sheet_targets(&mut archive);
    if targets.is_empty() {
        let mut fallback: Vec<(usize, String)> = Vec::new();
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                let name = entry.name().to_string();
                if let Some(rest) = name
                    .strip_prefix("xl/worksheets/sheet")
                    .and_then(|s| s.strip_suffix(".xml"))
                {
                    let ord = rest.parse::<usize>().unwrap_or(999);
                    fallback.push((ord, name));
                }
            }
        }
        fallback.sort_by_key(|(ord, _)| *ord);
        targets = fallback
            .into_iter()
            .enumerate()
            .map(|(i, (_, p))| (format!("Hoja {}", i + 1), p))
            .collect();
    }
    targets.truncate(max_sheets);

    let mut sheets = Vec::with_capacity(targets.len());
    for (sheet_name, entry_path) in targets {
        let Ok(mut entry) = archive.by_name(&entry_path) else {
            continue;
        };
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_err() {
            continue;
        }
        let (rows, total_rows) =
            parse_xlsx_worksheet(&xml, &shared_strings, max_rows_per_sheet, max_cols);
        sheets.push(SheetPreview {
            name: sheet_name,
            rows,
            total_rows,
        });
    }

    Ok(sheets)
}

/// Lista las entradas de un archivo comprimido ZIP / .qlplugin / JAR / APK para vista previa.
pub fn extract_zip_entries<P: AsRef<Path>>(
    path: P,
    max_entries: usize,
) -> Result<(usize, Vec<ArchiveEntryPreview>)> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let total = archive.len();
    let count = total.min(max_entries);
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        if let Ok(entry) = archive.by_index(i) {
            entries.push(ArchiveEntryPreview {
                path: entry.name().to_string(),
                size: entry.size(),
                compressed_size: entry.compressed_size(),
                is_dir: entry.is_dir(),
            });
        }
    }
    Ok((total, entries))
}

fn extract_xml_paragraphs(
    xml: &str,
    para_tag: &[u8],
    text_tags: &[&[u8]],
    max_chars: usize,
) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs = Vec::new();
    let mut current_para = String::new();
    let mut in_para = false;
    let mut in_text = false;
    let mut total_chars = 0usize;
    let mut buf = Vec::new();

    loop {
        if total_chars >= max_chars {
            break;
        }
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let q = e.name();
                let name = q.as_ref();
                if name == para_tag {
                    in_para = true;
                    current_para.clear();
                } else if in_para && text_tags.contains(&name) {
                    in_text = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let q = e.name();
                let name = q.as_ref();
                if name == para_tag {
                    let trimmed = current_para.trim();
                    if !trimmed.is_empty() {
                        total_chars += trimmed.len();
                        paragraphs.push(trimmed.to_string());
                    }
                    current_para.clear();
                    in_para = false;
                    in_text = false;
                } else if text_tags.contains(&name) {
                    in_text = false;
                }
            }
            Ok(Event::Text(e)) => {
                if in_para && in_text {
                    if let Ok(t) = e.unescape() {
                        current_para.push_str(&t);
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if in_para && in_text {
                    current_para.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(paragraphs)
}

fn parse_xlsx_shared_strings(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut list = Vec::new();
    let mut current = String::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                if name.as_ref() == b"si" {
                    in_si = true;
                    current.clear();
                } else if in_si && name.as_ref() == b"t" {
                    in_t = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                if name.as_ref() == b"si" {
                    list.push(std::mem::take(&mut current));
                    in_si = false;
                    in_t = false;
                } else if name.as_ref() == b"t" {
                    in_t = false;
                }
            }
            Ok(Event::Text(e)) if in_si && in_t => {
                if let Ok(t) = e.unescape() {
                    current.push_str(&t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    list
}

fn resolve_xlsx_sheet_targets(archive: &mut zip::ZipArchive<File>) -> Vec<(String, String)> {
    let mut rels: HashMap<String, String> = HashMap::new();
    if let Ok(mut rel_file) = archive.by_name("xl/_rels/workbook.xml.rels") {
        let mut xml = String::new();
        if rel_file.read_to_string(&mut xml).is_ok() {
            let mut reader = Reader::from_str(&xml);
            let mut buf = Vec::new();
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                        if e.name().as_ref() == b"Relationship" =>
                    {
                        let mut id = String::new();
                        let mut target = String::new();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"Id" => id = attr.unescape_value().unwrap_or_default().to_string(),
                                b"Target" => target = attr.unescape_value().unwrap_or_default().to_string(),
                                _ => {}
                            }
                        }
                        if !id.is_empty() && !target.is_empty() {
                            let clean = target.trim_start_matches('/');
                            let normalized = if clean.starts_with("xl/") {
                                clean.to_string()
                            } else {
                                format!("xl/{}", clean)
                            };
                            rels.insert(id, normalized);
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => {}
                }
                buf.clear();
            }
        }
    }

    let mut out = Vec::new();
    if let Ok(mut wb_file) = archive.by_name("xl/workbook.xml") {
        let mut xml = String::new();
        if wb_file.read_to_string(&mut xml).is_ok() {
            let mut reader = Reader::from_str(&xml);
            let mut buf = Vec::new();
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                        if e.name().as_ref() == b"sheet" =>
                    {
                        let mut name = String::new();
                        let mut rid = String::new();
                        for attr in e.attributes().flatten() {
                            let k = attr.key.as_ref();
                            if k == b"name" {
                                name = attr.unescape_value().unwrap_or_default().to_string();
                            } else if k == b"r:id" || k == b"id" {
                                rid = attr.unescape_value().unwrap_or_default().to_string();
                            }
                        }
                        if let Some(target) = rels.get(&rid) {
                            if target.contains("worksheets/") {
                                out.push((
                                    if name.is_empty() { "Hoja".to_string() } else { name },
                                    target.clone(),
                                ));
                            }
                        }
                    }
                    Ok(Event::Eof) | Err(_) => break,
                    _ => {}
                }
                buf.clear();
            }
        }
    }
    out
}

fn col_index_from_ref(cell_ref: &str) -> Option<usize> {
    let mut col = 0usize;
    let mut saw_alpha = false;
    for b in cell_ref.bytes() {
        if b.is_ascii_alphabetic() {
            saw_alpha = true;
            col = col * 26 + ((b.to_ascii_uppercase() - b'A') as usize + 1);
        } else {
            break;
        }
    }
    if saw_alpha && col > 0 {
        Some(col - 1)
    } else {
        None
    }
}

fn parse_xlsx_worksheet(
    xml: &str,
    shared_strings: &[String],
    max_rows: usize,
    max_cols: usize,
) -> (Vec<Vec<String>>, usize) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows = 0usize;
    let mut current_row: Vec<String> = Vec::new();
    let mut in_row = false;
    let mut cell_col: Option<usize> = None;
    let mut cell_type = String::new();
    let mut in_v = false;
    let mut in_is = false;
    let mut in_t = false;
    let mut cell_buf = String::new();
    let mut max_seen_cols = 0usize;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"row" => {
                        total_rows += 1;
                        if rows.len() < max_rows {
                            in_row = true;
                            current_row.clear();
                        }
                    }
                    b"c" if in_row => {
                        cell_col = None;
                        cell_type.clear();
                        cell_buf.clear();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"r" => {
                                    let r = attr.unescape_value().unwrap_or_default();
                                    cell_col = col_index_from_ref(&r);
                                }
                                b"t" => {
                                    cell_type = attr.unescape_value().unwrap_or_default().to_string();
                                }
                                _ => {}
                            }
                        }
                    }
                    b"v" if in_row => {
                        in_v = true;
                        cell_buf.clear();
                    }
                    b"is" if in_row => {
                        in_is = true;
                        cell_buf.clear();
                    }
                    b"t" if in_row && in_is => {
                        in_t = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"row" => {
                        if in_row {
                            if current_row.iter().any(|c| !c.is_empty()) {
                                max_seen_cols = max_seen_cols.max(current_row.len());
                                rows.push(std::mem::take(&mut current_row));
                            } else {
                                current_row.clear();
                            }
                            in_row = false;
                        }
                    }
                    b"v" => in_v = false,
                    b"t" => in_t = false,
                    b"is" => in_is = false,
                    b"c" if in_row => {
                        let resolved = match cell_type.as_str() {
                            "s" => cell_buf
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared_strings.get(i))
                                .cloned()
                                .unwrap_or_default(),
                            "b" => {
                                if cell_buf.trim() == "1" {
                                    "TRUE".to_string()
                                } else {
                                    "FALSE".to_string()
                                }
                            }
                            _ => cell_buf.trim().to_string(),
                        };
                        let col_idx = cell_col.unwrap_or(current_row.len());
                        if col_idx < max_cols {
                            if current_row.len() <= col_idx {
                                current_row.resize(col_idx + 1, String::new());
                            }
                            current_row[col_idx] = resolved;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) if in_row && (in_v || (in_is && in_t)) => {
                if let Ok(t) = e.unescape() {
                    cell_buf.push_str(&t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let norm_cols = max_seen_cols.min(max_cols).max(1);
    for row in &mut rows {
        row.resize(norm_cols, String::new());
    }
    (rows, total_rows)
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

    #[test]
    fn col_index_and_xlsx_worksheet_parse_cells_in_position() {
        assert_eq!(col_index_from_ref("A1"), Some(0));
        assert_eq!(col_index_from_ref("C12"), Some(2));
        assert_eq!(col_index_from_ref("AA5"), Some(26));

        let shared = parse_xlsx_shared_strings(
            r#"<sst><si><t>Producto</t></si><si><t>Total</t></si></sst>"#,
        );
        assert_eq!(shared, vec!["Producto", "Total"]);

        let sheet_xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2" t="inlineStr"><is><t>Laptop</t></is></c><c r="C2"><v>1500</v></c></row>
        </sheetData></worksheet>"#;
        let (rows, total) = parse_xlsx_worksheet(sheet_xml, &shared, 50, 10);
        assert_eq!(total, 2);
        assert_eq!(rows[0], vec!["Producto", "", "Total"]);
        assert_eq!(rows[1], vec!["Laptop", "", "1500"]);
    }
}
