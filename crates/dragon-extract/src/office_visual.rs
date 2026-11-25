//! Extractor visual de alta fidelidad para formatos Office OOXML (.docx, .xlsx, .pptx).
//! Recupera estructura, estilos, colores, tablas, celdas combinadas, coordenadas de diapositivas
//! y rutas de imágenes embebidas sin modificar los archivos originales.

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct WordRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<String>,
    pub size_pt: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordTableCell {
    pub colspan: usize,
    pub bg: Option<String>,
    pub paragraphs: Vec<WordParagraph>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordParagraph {
    pub style: String,
    pub align: String,
    pub is_list: bool,
    pub runs: Vec<WordRun>,
    pub image_entry: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum WordBlock {
    #[serde(rename = "paragraph")]
    Paragraph(WordParagraph),
    #[serde(rename = "table")]
    Table { rows: Vec<Vec<WordTableCell>> },
}

#[derive(Debug, Clone, Serialize)]
pub struct WordVisualPreview {
    pub blocks: Vec<WordBlock>,
    pub truncated: bool,
    pub plain_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcelCell {
    pub value: String,
    pub bold: bool,
    pub italic: bool,
    pub align: Option<String>,
    pub color: Option<String>,
    pub bg: Option<String>,
    pub is_number: bool,
    pub colspan: usize,
    pub rowspan: usize,
    pub hidden_by_merge: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcelSheetVisual {
    pub name: String,
    pub col_widths: Vec<u32>,
    pub rows: Vec<Vec<ExcelCell>>,
    pub total_rows: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcelVisualPreview {
    pub sheets: Vec<ExcelSheetVisual>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PptParagraph {
    pub align: String,
    pub bullet: bool,
    pub runs: Vec<WordRun>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum PptElement {
    #[serde(rename = "textbox")]
    TextBox {
        left_pct: f32,
        top_pct: f32,
        width_pct: f32,
        height_pct: f32,
        bg: Option<String>,
        role: String,
        paragraphs: Vec<PptParagraph>,
    },
    #[serde(rename = "image")]
    Image {
        left_pct: f32,
        top_pct: f32,
        width_pct: f32,
        height_pct: f32,
        entry: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PptSlideVisual {
    pub number: usize,
    pub title: String,
    pub bg: Option<String>,
    pub elements: Vec<PptElement>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PptVisualPreview {
    pub aspect_ratio: f32,
    pub slides: Vec<PptSlideVisual>,
}

/// Lee de forma segura los bytes de una entrada dentro de un paquete ZIP/OOXML (ej. word/media/image1.png).
pub fn read_zip_entry_bytes<P: AsRef<Path>>(
    path: P,
    entry_path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let normalized = entry_path.trim_start_matches('/').replace('\\', "/");
    if normalized.is_empty() || normalized.contains("..") {
        anyhow::bail!("Ruta interna inválida");
    }
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let entry = archive
        .by_name(&normalized)
        .with_context(|| format!("Entrada no encontrada en el archivo: {}", normalized))?;
    if entry.size() as usize > max_bytes {
        anyhow::bail!("El recurso embebido excede el límite de tamaño");
    }
    let mut buf = Vec::with_capacity((entry.size() as usize).min(max_bytes));
    entry.take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn parse_rels_map(archive: &mut zip::ZipArchive<File>, rels_path: &str, base_dir: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(mut rel_file) = archive.by_name(rels_path) else {
        return map;
    };
    let mut xml = String::new();
    if rel_file.read_to_string(&mut xml).is_err() {
        return map;
    }
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                if e.name().as_ref() == b"Relationship" =>
            {
                let mut id = String::new();
                let mut target = String::new();
                let mut target_mode = String::new();
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"Id" => id = attr.unescape_value().unwrap_or_default().to_string(),
                        b"Target" => target = attr.unescape_value().unwrap_or_default().to_string(),
                        b"TargetMode" => target_mode = attr.unescape_value().unwrap_or_default().to_string(),
                        _ => {}
                    }
                }
                if !id.is_empty() && !target.is_empty() && !target_mode.eq_ignore_ascii_case("External") {
                    let resolved = resolve_ooxml_rel(base_dir, &target);
                    map.insert(id, resolved);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    map
}

fn resolve_ooxml_rel(base_dir: &str, target: &str) -> String {
    let clean = target.replace('\\', "/");
    if let Some(abs) = clean.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in clean.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn sanitize_hex_color(raw: &str) -> Option<String> {
    let s = raw.trim().trim_start_matches('#');
    if s.eq_ignore_ascii_case("auto") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    if s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(format!("#{}", s.to_ascii_uppercase()));
    }
    if s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        // ARGB -> RGB
        return Some(format!("#{}", s[2..].to_ascii_uppercase()));
    }
    None
}

fn attr_val(e: &quick_xml::events::BytesStart<'_>, target_keys: &[&[u8]]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let k = attr.key.as_ref();
        if target_keys.contains(&k) {
            return attr.unescape_value().ok().map(|v| v.to_string());
        }
    }
    None
}

/// Extrae un documento Word (.docx/.docm/.dotx) preservando títulos, formato inline, tablas e imágenes.
pub fn extract_word_visual<P: AsRef<Path>>(path: P, max_blocks: usize) -> Result<WordVisualPreview> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;
    let rels = parse_rels_map(&mut archive, "word/_rels/document.xml.rels", "word");

    let mut doc_xml = archive
        .by_name("word/document.xml")
        .context("No se encontró word/document.xml")?;
    let mut xml = String::new();
    doc_xml.read_to_string(&mut xml)?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut blocks: Vec<WordBlock> = Vec::new();
    let mut plain_text = String::with_capacity(4096);
    let mut truncated = false;

    let mut in_table = false;
    let mut table_rows: Vec<Vec<WordTableCell>> = Vec::new();
    let mut current_row: Vec<WordTableCell> = Vec::new();
    let mut in_cell = false;
    let mut cell_colspan = 1usize;
    let mut cell_bg: Option<String> = None;
    let mut cell_paras: Vec<WordParagraph> = Vec::new();

    let mut in_para = false;
    let mut para_style = String::new();
    let mut para_align = String::from("left");
    let mut para_is_list = false;
    let mut para_runs: Vec<WordRun> = Vec::new();
    let mut para_image: Option<String> = None;

    let mut in_run = false;
    let mut run_bold = false;
    let mut run_italic = false;
    let mut run_underline = false;
    let mut run_strike = false;
    let mut run_color: Option<String> = None;
    let mut run_size: Option<f32> = None;
    let mut in_text = false;
    let mut run_text = String::new();
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut in_tcpr = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let q = e.name();
                let name = q.as_ref();
                match name {
                    b"w:tbl" if !in_table => {
                        if blocks.len() >= max_blocks {
                            truncated = true;
                            break;
                        }
                        in_table = true;
                        table_rows.clear();
                    }
                    b"w:tr" if in_table => {
                        current_row.clear();
                    }
                    b"w:tc" if in_table => {
                        in_cell = true;
                        cell_colspan = 1;
                        cell_bg = None;
                        cell_paras.clear();
                    }
                    b"w:tcPr" if in_cell => in_tcpr = true,
                    b"w:p" => {
                        if !in_table && blocks.len() >= max_blocks {
                            truncated = true;
                            break;
                        }
                        in_para = true;
                        para_style.clear();
                        para_align = String::from("left");
                        para_is_list = false;
                        para_runs.clear();
                        para_image = None;
                    }
                    b"w:pPr" if in_para => in_ppr = true,
                    b"w:numPr" if in_ppr => para_is_list = true,
                    b"w:r" if in_para => {
                        in_run = true;
                        run_bold = false;
                        run_italic = false;
                        run_underline = false;
                        run_strike = false;
                        run_color = None;
                        run_size = None;
                        run_text.clear();
                    }
                    b"w:rPr" if in_run => in_rpr = true,
                    b"w:t" if in_run => in_text = true,
                    b"w:pStyle" if in_ppr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]) {
                            para_style = v;
                        }
                    }
                    b"w:jc" if in_ppr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]) {
                            para_align = match v.as_str() {
                                "center" => "center",
                                "right" | "end" => "right",
                                "both" => "justify",
                                _ => "left",
                            }
                            .to_string();
                        }
                    }
                    b"w:gridSpan" if in_tcpr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| s.parse::<usize>().ok()) {
                            cell_colspan = v.max(1);
                        }
                    }
                    b"w:shd" if in_tcpr => {
                        if let Some(v) = attr_val(e, &[b"w:fill", b"fill"]).and_then(|s| sanitize_hex_color(&s)) {
                            if v != "#FFFFFF" {
                                cell_bg = Some(v);
                            }
                        }
                    }
                    b"w:b" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_bold = !off;
                    }
                    b"w:i" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_italic = !off;
                    }
                    b"w:u" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "none");
                        run_underline = !off;
                    }
                    b"w:strike" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_strike = !off;
                    }
                    b"w:color" if in_rpr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| sanitize_hex_color(&s)) {
                            run_color = Some(v);
                        }
                    }
                    b"w:sz" if in_rpr => {
                        if let Some(half_pt) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| s.parse::<f32>().ok()) {
                            run_size = Some((half_pt / 2.0).clamp(8.0, 42.0));
                        }
                    }
                    b"a:blip" if in_para => {
                        if let Some(rid) = attr_val(e, &[b"r:embed", b"embed", b"r:link"]) {
                            if let Some(target) = rels.get(&rid) {
                                if is_previewable_image(target) {
                                    para_image = Some(target.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let q = e.name();
                let name = q.as_ref();
                match name {
                    b"w:pStyle" if in_ppr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]) {
                            para_style = v;
                        }
                    }
                    b"w:jc" if in_ppr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]) {
                            para_align = match v.as_str() {
                                "center" => "center",
                                "right" | "end" => "right",
                                "both" => "justify",
                                _ => "left",
                            }
                            .to_string();
                        }
                    }
                    b"w:gridSpan" if in_tcpr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| s.parse::<usize>().ok()) {
                            cell_colspan = v.max(1);
                        }
                    }
                    b"w:shd" if in_tcpr => {
                        if let Some(v) = attr_val(e, &[b"w:fill", b"fill"]).and_then(|s| sanitize_hex_color(&s)) {
                            if v != "#FFFFFF" {
                                cell_bg = Some(v);
                            }
                        }
                    }
                    b"w:b" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_bold = !off;
                    }
                    b"w:i" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_italic = !off;
                    }
                    b"w:u" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "none");
                        run_underline = !off;
                    }
                    b"w:strike" if in_rpr => {
                        let off = attr_val(e, &[b"w:val", b"val"]).is_some_and(|v| v == "0" || v == "false");
                        run_strike = !off;
                    }
                    b"w:color" if in_rpr => {
                        if let Some(v) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| sanitize_hex_color(&s)) {
                            run_color = Some(v);
                        }
                    }
                    b"w:sz" if in_rpr => {
                        if let Some(half_pt) = attr_val(e, &[b"w:val", b"val"]).and_then(|s| s.parse::<f32>().ok()) {
                            run_size = Some((half_pt / 2.0).clamp(8.0, 42.0));
                        }
                    }
                    b"w:br" if in_run => run_text.push('\n'),
                    b"w:tab" if in_run => run_text.push('\t'),
                    b"a:blip" if in_para => {
                        if let Some(rid) = attr_val(e, &[b"r:embed", b"embed", b"r:link"]) {
                            if let Some(target) = rels.get(&rid) {
                                if is_previewable_image(target) {
                                    para_image = Some(target.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) if in_run && in_text => {
                if let Ok(t) = e.unescape() {
                    run_text.push_str(&t);
                }
            }
            Ok(Event::End(ref e)) => {
                let q = e.name();
                let name = q.as_ref();
                match name {
                    b"w:t" => in_text = false,
                    b"w:rPr" => in_rpr = false,
                    b"w:pPr" => in_ppr = false,
                    b"w:tcPr" => in_tcpr = false,
                    b"w:r" if in_run => {
                        if !run_text.is_empty() {
                            if plain_text.len() < 40_000 {
                                plain_text.push_str(&run_text);
                            }
                            para_runs.push(WordRun {
                                text: std::mem::take(&mut run_text),
                                bold: run_bold,
                                italic: run_italic,
                                underline: run_underline,
                                strike: run_strike,
                                color: run_color.take(),
                                size_pt: run_size.take(),
                            });
                        }
                        in_run = false;
                        in_rpr = false;
                        in_text = false;
                    }
                    b"w:p" if in_para => {
                        if plain_text.len() < 40_000 && !para_runs.is_empty() {
                            plain_text.push('\n');
                        }
                        let has_content = !para_runs.is_empty() || para_image.is_some();
                        let p_obj = WordParagraph {
                            style: normalize_word_style(&para_style),
                            align: para_align.clone(),
                            is_list: para_is_list,
                            runs: std::mem::take(&mut para_runs),
                            image_entry: para_image.take(),
                        };
                        if in_cell {
                            if has_content {
                                cell_paras.push(p_obj);
                            }
                        } else if has_content {
                            blocks.push(WordBlock::Paragraph(p_obj));
                        }
                        in_para = false;
                        in_ppr = false;
                    }
                    b"w:tc" if in_cell => {
                        current_row.push(WordTableCell {
                            colspan: cell_colspan,
                            bg: cell_bg.take(),
                            paragraphs: std::mem::take(&mut cell_paras),
                        });
                        in_cell = false;
                        in_tcpr = false;
                    }
                    b"w:tr" if in_table => {
                        if !current_row.is_empty() && table_rows.len() < 80 {
                            table_rows.push(std::mem::take(&mut current_row));
                        } else {
                            current_row.clear();
                        }
                    }
                    b"w:tbl" if in_table => {
                        if !table_rows.is_empty() {
                            blocks.push(WordBlock::Table {
                                rows: std::mem::take(&mut table_rows),
                            });
                        }
                        in_table = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(WordVisualPreview {
        blocks,
        truncated,
        plain_text,
    })
}

fn normalize_word_style(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("title") && !lower.contains("sub") {
        "title".to_string()
    } else if lower.contains("subtitle") {
        "subtitle".to_string()
    } else if lower.contains("heading1") || lower.contains("titulo1") || lower == "1" {
        "h1".to_string()
    } else if lower.contains("heading2") || lower.contains("titulo2") || lower == "2" {
        "h2".to_string()
    } else if lower.contains("heading3") || lower.contains("titulo3") || lower == "3" {
        "h3".to_string()
    } else if lower.contains("heading") || lower.contains("titulo") {
        "h3".to_string()
    } else {
        "normal".to_string()
    }
}

fn is_previewable_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".svg")
}

// ============================================================================
// EXCEL VISUAL (.xlsx / .xlsm)
// ============================================================================

#[derive(Default, Clone)]
struct ExcelFontStyle {
    bold: bool,
    italic: bool,
    color: Option<String>,
}

#[derive(Default, Clone)]
struct ExcelXfStyle {
    font_id: usize,
    fill_id: usize,
    align: Option<String>,
}

/// Extrae hojas de cálculo Excel con estilos (negrita, colores, rellenos, alineación, anchos y celdas combinadas).
pub fn extract_excel_visual<P: AsRef<Path>>(
    path: P,
    max_sheets: usize,
    max_rows: usize,
    max_cols: usize,
) -> Result<ExcelVisualPreview> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    let shared_strings = if let Ok(mut f) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml = String::new();
        if f.read_to_string(&mut xml).is_ok() {
            parse_shared_strings(&xml)
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let (fonts, fills, xfs) = if let Ok(mut f) = archive.by_name("xl/styles.xml") {
        let mut xml = String::new();
        if f.read_to_string(&mut xml).is_ok() {
            parse_excel_styles(&xml)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        }
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let rels = parse_rels_map(&mut archive, "xl/_rels/workbook.xml.rels", "xl");
    let mut targets = resolve_workbook_sheets(&mut archive, &rels);
    if targets.is_empty() {
        let mut fallback = Vec::new();
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                let name = entry.name().to_string();
                if let Some(rest) = name
                    .strip_prefix("xl/worksheets/sheet")
                    .and_then(|s| s.strip_suffix(".xml"))
                {
                    fallback.push((rest.parse::<usize>().unwrap_or(999), name));
                }
            }
        }
        fallback.sort_by_key(|(o, _)| *o);
        targets = fallback
            .into_iter()
            .enumerate()
            .map(|(i, (_, p))| (format!("Hoja {}", i + 1), p))
            .collect();
    }
    targets.truncate(max_sheets);

    let mut sheets = Vec::with_capacity(targets.len());
    for (sheet_name, sheet_path) in targets {
        let Ok(mut entry) = archive.by_name(&sheet_path) else {
            continue;
        };
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_err() {
            continue;
        }
        sheets.push(parse_excel_sheet_visual(
            &sheet_name,
            &xml,
            &shared_strings,
            &fonts,
            &fills,
            &xfs,
            max_rows,
            max_cols,
        ));
    }

    Ok(ExcelVisualPreview { sheets })
}

fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut list = Vec::new();
    let mut cur = String::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"si" => {
                    list.push(std::mem::take(&mut cur));
                    in_si = false;
                    in_t = false;
                }
                b"t" => in_t = false,
                _ => {}
            },
            Ok(Event::Text(e)) if in_si && in_t => {
                if let Ok(t) = e.unescape() {
                    cur.push_str(&t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    list
}

fn resolve_workbook_sheets(
    archive: &mut zip::ZipArchive<File>,
    rels: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(mut wb) = archive.by_name("xl/workbook.xml") else {
        return out;
    };
    let mut xml = String::new();
    if wb.read_to_string(&mut xml).is_err() {
        return out;
    }
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) if e.name().as_ref() == b"sheet" => {
                let name = attr_val(e, &[b"name"]).unwrap_or_else(|| "Hoja".to_string());
                let rid = attr_val(e, &[b"r:id", b"id"]).unwrap_or_default();
                if let Some(target) = rels.get(&rid) {
                    if target.contains("worksheets/") {
                        out.push((name, target.clone()));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn parse_excel_styles(xml: &str) -> (Vec<ExcelFontStyle>, Vec<Option<String>>, Vec<ExcelXfStyle>) {
    let mut fonts = Vec::new();
    let mut fills = Vec::new();
    let mut xfs = Vec::new();

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut in_fonts = false;
    let mut in_font = false;
    let mut cur_font = ExcelFontStyle::default();

    let mut in_fills = false;
    let mut in_fill = false;
    let mut pattern_type = String::new();
    let mut cur_fill: Option<String> = None;

    let mut in_cell_xfs = false;
    let mut in_xf = false;
    let mut cur_xf = ExcelXfStyle::default();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"fonts" => in_fonts = true,
                b"font" if in_fonts => {
                    in_font = true;
                    cur_font = ExcelFontStyle::default();
                }
                b"b" if in_font => cur_font.bold = true,
                b"i" if in_font => cur_font.italic = true,
                b"color" if in_font => {
                    if let Some(rgb) = attr_val(e, &[b"rgb"]).and_then(|s| sanitize_hex_color(&s)) {
                        cur_font.color = Some(rgb);
                    }
                }
                b"fills" => in_fills = true,
                b"fill" if in_fills => {
                    in_fill = true;
                    pattern_type.clear();
                    cur_fill = None;
                }
                b"patternFill" if in_fill => {
                    pattern_type = attr_val(e, &[b"patternType"]).unwrap_or_default();
                }
                b"fgColor" if in_fill && pattern_type == "solid" => {
                    if let Some(rgb) = attr_val(e, &[b"rgb"]).and_then(|s| sanitize_hex_color(&s)) {
                        cur_fill = Some(rgb);
                    }
                }
                b"cellXfs" => in_cell_xfs = true,
                b"xf" if in_cell_xfs => {
                    in_xf = true;
                    cur_xf = ExcelXfStyle {
                        font_id: attr_val(e, &[b"fontId"]).and_then(|s| s.parse().ok()).unwrap_or(0),
                        fill_id: attr_val(e, &[b"fillId"]).and_then(|s| s.parse().ok()).unwrap_or(0),
                        align: None,
                    };
                }
                b"alignment" if in_xf => {
                    cur_xf.align = attr_val(e, &[b"horizontal"]);
                }
                _ => {}
            },
            Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                b"b" if in_font => cur_font.bold = true,
                b"i" if in_font => cur_font.italic = true,
                b"color" if in_font => {
                    if let Some(rgb) = attr_val(e, &[b"rgb"]).and_then(|s| sanitize_hex_color(&s)) {
                        cur_font.color = Some(rgb);
                    }
                }
                b"patternFill" if in_fill => {
                    pattern_type = attr_val(e, &[b"patternType"]).unwrap_or_default();
                }
                b"fgColor" if in_fill && pattern_type == "solid" => {
                    if let Some(rgb) = attr_val(e, &[b"rgb"]).and_then(|s| sanitize_hex_color(&s)) {
                        cur_fill = Some(rgb);
                    }
                }
                b"xf" if in_cell_xfs => {
                    xfs.push(ExcelXfStyle {
                        font_id: attr_val(e, &[b"fontId"]).and_then(|s| s.parse().ok()).unwrap_or(0),
                        fill_id: attr_val(e, &[b"fillId"]).and_then(|s| s.parse().ok()).unwrap_or(0),
                        align: None,
                    });
                }
                b"alignment" if in_xf => {
                    cur_xf.align = attr_val(e, &[b"horizontal"]);
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"fonts" => in_fonts = false,
                b"font" if in_font => {
                    fonts.push(std::mem::take(&mut cur_font));
                    in_font = false;
                }
                b"fills" => in_fills = false,
                b"fill" if in_fill => {
                    fills.push(cur_fill.take());
                    in_fill = false;
                }
                b"cellXfs" => in_cell_xfs = false,
                b"xf" if in_xf => {
                    xfs.push(std::mem::take(&mut cur_xf));
                    in_xf = false;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    (fonts, fills, xfs)
}

fn parse_cell_coord(cell_ref: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut split_idx = 0usize;
    for (i, b) in cell_ref.bytes().enumerate() {
        if b.is_ascii_alphabetic() {
            col = col * 26 + ((b.to_ascii_uppercase() - b'A') as usize + 1);
            split_idx = i + 1;
        } else {
            break;
        }
    }
    let row = cell_ref[split_idx..].parse::<usize>().ok()?;
    if col == 0 || row == 0 {
        None
    } else {
        Some((row - 1, col - 1))
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_excel_sheet_visual(
    sheet_name: &str,
    xml: &str,
    shared_strings: &[String],
    fonts: &[ExcelFontStyle],
    fills: &[Option<String>],
    xfs: &[ExcelXfStyle],
    max_rows: usize,
    max_cols: usize,
) -> ExcelSheetVisual {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut grid: Vec<Vec<ExcelCell>> = Vec::new();
    let mut col_widths_map: HashMap<usize, u32> = HashMap::new();
    let mut merges: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut total_rows = 0usize;
    let mut max_col_seen = 1usize;

    let mut in_row = false;
    let mut row_idx_0 = 0usize;
    let mut cell_coord: Option<(usize, usize)> = None;
    let mut cell_type = String::new();
    let mut cell_style_idx = 0usize;
    let mut in_v = false;
    let mut in_is = false;
    let mut in_t = false;
    let mut cell_buf = String::new();
    let mut buf = Vec::new();

    let empty_cell = || ExcelCell {
        value: String::new(),
        bold: false,
        italic: false,
        align: None,
        color: None,
        bg: None,
        is_number: false,
        colspan: 1,
        rowspan: 1,
        hidden_by_merge: false,
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let q = e.name();
                match q.as_ref() {
                    b"col" => {
                        let min_c = attr_val(e, &[b"min"]).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
                        let max_c = attr_val(e, &[b"max"]).and_then(|s| s.parse::<usize>().ok()).unwrap_or(min_c);
                        let w = attr_val(e, &[b"width"])
                            .and_then(|s| s.parse::<f32>().ok())
                            .map(|w| (w * 7.8).clamp(52.0, 320.0) as u32)
                            .unwrap_or(110);
                        for c in min_c..=max_c.min(max_cols) {
                            if c >= 1 {
                                col_widths_map.insert(c - 1, w);
                            }
                        }
                    }
                    b"mergeCell" => {
                        if let Some(r_str) = attr_val(e, &[b"ref"]) {
                            if let Some((a, b)) = r_str.split_once(':') {
                                if let (Some((r1, c1)), Some((r2, c2))) =
                                    (parse_cell_coord(a), parse_cell_coord(b))
                                {
                                    merges.push((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
                                }
                            }
                        }
                    }
                    b"row" => {
                        let r_num = attr_val(e, &[b"r"])
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(total_rows + 1);
                        total_rows = total_rows.max(r_num);
                        if r_num <= max_rows {
                            in_row = true;
                            row_idx_0 = r_num - 1;
                            if grid.len() <= row_idx_0 {
                                grid.resize_with(row_idx_0 + 1, Vec::new);
                            }
                        } else {
                            in_row = false;
                        }
                    }
                    b"c" if in_row => {
                        cell_coord = attr_val(e, &[b"r"]).and_then(|r| parse_cell_coord(&r));
                        cell_type = attr_val(e, &[b"t"]).unwrap_or_default();
                        cell_style_idx = attr_val(e, &[b"s"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                        cell_buf.clear();
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
            Ok(Event::Text(e)) if in_row && (in_v || (in_is && in_t)) => {
                if let Ok(t) = e.unescape() {
                    cell_buf.push_str(&t);
                }
            }
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"row" => in_row = false,
                b"v" => in_v = false,
                b"t" => in_t = false,
                b"is" => in_is = false,
                b"c" if in_row => {
                    let (r_i, c_i) = cell_coord.unwrap_or((row_idx_0, grid[row_idx_0].len()));
                    if r_i < max_rows && c_i < max_cols {
                        if grid.len() <= r_i {
                            grid.resize_with(r_i + 1, Vec::new);
                        }
                        if grid[r_i].len() <= c_i {
                            grid[r_i].resize_with(c_i + 1, empty_cell);
                        }
                        max_col_seen = max_col_seen.max(c_i + 1);

                        let is_num = cell_type.is_empty() && cell_buf.trim().parse::<f64>().is_ok();
                        let val = match cell_type.as_str() {
                            "s" => cell_buf
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|idx| shared_strings.get(idx))
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

                        let xf = xfs.get(cell_style_idx);
                        let font = xf.and_then(|x| fonts.get(x.font_id));
                        let bg = xf.and_then(|x| fills.get(x.fill_id)).and_then(|f| f.clone());
                        let align = xf
                            .and_then(|x| x.align.clone())
                            .or_else(|| if is_num { Some("right".to_string()) } else { None });

                        grid[r_i][c_i] = ExcelCell {
                            value: val,
                            bold: font.is_some_and(|f| f.bold),
                            italic: font.is_some_and(|f| f.italic),
                            align,
                            color: font.and_then(|f| f.color.clone()),
                            bg,
                            is_number: is_num,
                            colspan: 1,
                            rowspan: 1,
                            hidden_by_merge: false,
                        };
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // Recortar filas vacías al final y normalizar ancho de columnas
    while grid
        .last()
        .is_some_and(|r| r.iter().all(|c| c.value.is_empty() && c.bg.is_none()))
    {
        grid.pop();
    }
    let cols_count = max_col_seen.clamp(1, max_cols);
    for row in &mut grid {
        if row.len() < cols_count {
            row.resize_with(cols_count, empty_cell);
        }
    }

    // Aplicar merges dentro del rango visible
    for (r1, c1, r2, c2) in merges {
        if r1 < grid.len() && c1 < cols_count {
            let r_end = r2.min(grid.len() - 1);
            let c_end = c2.min(cols_count - 1);
            grid[r1][c1].rowspan = r_end - r1 + 1;
            grid[r1][c1].colspan = c_end - c1 + 1;
            for r in r1..=r_end {
                for c in c1..=c_end {
                    if r != r1 || c != c1 {
                        grid[r][c].hidden_by_merge = true;
                    }
                }
            }
        }
    }

    let col_widths = (0..cols_count)
        .map(|i| col_widths_map.get(&i).copied().unwrap_or(112))
        .collect();

    ExcelSheetVisual {
        name: sheet_name.to_string(),
        col_widths,
        rows: grid,
        total_rows: total_rows.max(1),
    }
}

// ============================================================================
// POWERPOINT VISUAL (.pptx / .pptm / .ppsx)
// ============================================================================

/// Extrae diapositivas PowerPoint con relación de aspecto, color de fondo, cajas de texto posicionadas, imágenes y notas.
pub fn extract_pptx_visual<P: AsRef<Path>>(path: P, max_slides: usize) -> Result<PptVisualPreview> {
    let file = File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    let (slide_cx, slide_cy) = read_pptx_slide_size(&mut archive);
    let aspect_ratio = (slide_cx as f32 / slide_cy.max(1) as f32).clamp(1.0, 2.5);

    let mut slide_indices: Vec<(usize, String)> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_string();
            if let Some(rest) = name
                .strip_prefix("ppt/slides/slide")
                .and_then(|s| s.strip_suffix(".xml"))
            {
                if let Ok(num) = rest.parse::<usize>() {
                    slide_indices.push((num, name));
                }
            }
        }
    }
    slide_indices.sort_by_key(|(n, _)| *n);
    slide_indices.truncate(max_slides);

    let mut slides = Vec::with_capacity(slide_indices.len());
    for (num, slide_path) in slide_indices {
        let rels_path = format!("ppt/slides/_rels/slide{}.xml.rels", num);
        let rels = parse_rels_map(&mut archive, &rels_path, "ppt/slides");

        let mut s_xml = String::new();
        {
            let Ok(mut s_file) = archive.by_name(&slide_path) else {
                continue;
            };
            if s_file.read_to_string(&mut s_xml).is_err() {
                continue;
            }
        }

        let notes_path = format!("ppt/notesSlides/notesSlide{}.xml", num);
        let notes = if let Ok(mut nf) = archive.by_name(&notes_path) {
            let mut nxml = String::new();
            if nf.read_to_string(&mut nxml).is_ok() {
                extract_simple_pptx_text(&nxml)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        slides.push(parse_pptx_slide_visual(
            num,
            &s_xml,
            &rels,
            slide_cx,
            slide_cy,
            notes,
        ));
    }

    Ok(PptVisualPreview {
        aspect_ratio,
        slides,
    })
}

fn read_pptx_slide_size(archive: &mut zip::ZipArchive<File>) -> (i64, i64) {
    let default = (12_192_000i64, 6_858_000i64); // 16:9 widescreen EMUs
    let Ok(mut pres) = archive.by_name("ppt/presentation.xml") else {
        return default;
    };
    let mut xml = String::new();
    if pres.read_to_string(&mut xml).is_err() {
        return default;
    }
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) if e.name().as_ref() == b"p:sldSz" => {
                let cx = attr_val(e, &[b"cx"]).and_then(|s| s.parse::<i64>().ok()).unwrap_or(default.0);
                let cy = attr_val(e, &[b"cy"]).and_then(|s| s.parse::<i64>().ok()).unwrap_or(default.1);
                if cx > 0 && cy > 0 {
                    return (cx, cy);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    default
}

fn extract_simple_pptx_text(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_t = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"a:t" => in_t = true,
            Ok(Event::End(ref e)) if e.name().as_ref() == b"a:t" => in_t = false,
            Ok(Event::End(ref e)) if e.name().as_ref() == b"a:p" => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            Ok(Event::Text(e)) if in_t => {
                if let Ok(t) = e.unescape() {
                    out.push_str(&t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out.trim().to_string()
}

fn parse_pptx_slide_visual(
    num: usize,
    xml: &str,
    rels: &HashMap<String, String>,
    slide_cx: i64,
    slide_cy: i64,
    notes: String,
) -> PptSlideVisual {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut slide_bg: Option<String> = None;
    let mut elements: Vec<PptElement> = Vec::new();
    let mut title_text = String::new();

    let mut in_bg = false;
    let mut in_sp = false;
    let mut in_pic = false;
    let mut in_sppr = false;
    let mut in_xfrm = false;
    let mut in_txbody = false;
    let mut in_p = false;
    let mut in_ppr = false;
    let mut in_r = false;
    let mut in_rpr = false;
    let mut in_t = false;

    let mut off_x = 0i64;
    let mut off_y = 0i64;
    let mut ext_cx = 0i64;
    let mut ext_cy = 0i64;
    let mut shape_bg: Option<String> = None;
    let mut shape_role = String::from("body");
    let mut pic_entry: Option<String> = None;

    let mut paras: Vec<PptParagraph> = Vec::new();
    let mut p_align = String::from("left");
    let mut p_bullet = false;
    let mut p_runs: Vec<WordRun> = Vec::new();

    let mut r_bold = false;
    let mut r_italic = false;
    let mut r_color: Option<String> = None;
    let mut r_size: Option<f32> = None;
    let mut r_text = String::new();

    let mut buf = Vec::new();

    let to_pct = |v: i64, total: i64| -> f32 {
        ((v as f32 / total.max(1) as f32) * 100.0).clamp(0.0, 100.0)
    };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"p:bg" => in_bg = true,
                b"p:sp" => {
                    in_sp = true;
                    off_x = 0;
                    off_y = 0;
                    ext_cx = 0;
                    ext_cy = 0;
                    shape_bg = None;
                    shape_role = String::from("body");
                    paras.clear();
                }
                b"p:pic" => {
                    in_pic = true;
                    off_x = 0;
                    off_y = 0;
                    ext_cx = 0;
                    ext_cy = 0;
                    pic_entry = None;
                }
                b"p:spPr" if in_sp || in_pic => in_sppr = true,
                b"a:xfrm" if in_sppr => in_xfrm = true,
                b"p:txBody" if in_sp => in_txbody = true,
                b"a:p" if in_txbody => {
                    in_p = true;
                    p_align = if shape_role == "title" { "center" } else { "left" }.to_string();
                    p_bullet = false;
                    p_runs.clear();
                }
                b"a:pPr" if in_p => {
                    in_ppr = true;
                    if let Some(a) = attr_val(e, &[b"algn"]) {
                        p_align = match a.as_str() {
                            "ctr" => "center",
                            "r" => "right",
                            "just" => "justify",
                            _ => "left",
                        }
                        .to_string();
                    }
                }
                b"a:r" if in_p => {
                    in_r = true;
                    r_bold = false;
                    r_italic = false;
                    r_color = None;
                    r_size = None;
                    r_text.clear();
                }
                b"a:rPr" if in_r => {
                    in_rpr = true;
                    if let Some(b) = attr_val(e, &[b"b"]) {
                        r_bold = b == "1" || b == "true";
                    }
                    if let Some(it) = attr_val(e, &[b"i"]) {
                        r_italic = it == "1" || it == "true";
                    }
                    if let Some(sz) = attr_val(e, &[b"sz"]).and_then(|s| s.parse::<f32>().ok()) {
                        r_size = Some((sz / 100.0).clamp(8.0, 56.0));
                    }
                }
                b"a:t" if in_r => in_t = true,
                b"a:srgbClr" => {
                    if let Some(hex) = attr_val(e, &[b"val"]).and_then(|s| sanitize_hex_color(&s)) {
                        if in_bg {
                            slide_bg = Some(hex);
                        } else if in_rpr {
                            r_color = Some(hex);
                        } else if in_sppr {
                            shape_bg = Some(hex);
                        }
                    }
                }
                b"p:ph" if in_sp => {
                    if let Some(t) = attr_val(e, &[b"type"]) {
                        if t == "title" || t == "ctrTitle" {
                            shape_role = String::from("title");
                        } else if t == "subTitle" {
                            shape_role = String::from("subtitle");
                        }
                    }
                }
                b"a:buChar" | b"a:buAutoNum" if in_ppr => p_bullet = true,
                b"a:blip" if in_pic => {
                    if let Some(rid) = attr_val(e, &[b"r:embed", b"embed"]) {
                        if let Some(target) = rels.get(&rid) {
                            if is_previewable_image(target) {
                                pic_entry = Some(target.clone());
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                b"p:ph" if in_sp => {
                    if let Some(t) = attr_val(e, &[b"type"]) {
                        if t == "title" || t == "ctrTitle" {
                            shape_role = String::from("title");
                        } else if t == "subTitle" {
                            shape_role = String::from("subtitle");
                        }
                    }
                }
                b"a:off" if in_xfrm => {
                    off_x = attr_val(e, &[b"x"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                    off_y = attr_val(e, &[b"y"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
                b"a:ext" if in_xfrm => {
                    ext_cx = attr_val(e, &[b"cx"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                    ext_cy = attr_val(e, &[b"cy"]).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
                b"a:srgbClr" => {
                    if let Some(hex) = attr_val(e, &[b"val"]).and_then(|s| sanitize_hex_color(&s)) {
                        if in_bg {
                            slide_bg = Some(hex);
                        } else if in_rpr {
                            r_color = Some(hex);
                        } else if in_sppr {
                            shape_bg = Some(hex);
                        }
                    }
                }
                b"a:buChar" | b"a:buAutoNum" if in_ppr => p_bullet = true,
                b"a:blip" if in_pic => {
                    if let Some(rid) = attr_val(e, &[b"r:embed", b"embed"]) {
                        if let Some(target) = rels.get(&rid) {
                            if is_previewable_image(target) {
                                pic_entry = Some(target.clone());
                            }
                        }
                    }
                }
                b"a:rPr" if in_r => {
                    if let Some(b) = attr_val(e, &[b"b"]) {
                        r_bold = b == "1" || b == "true";
                    }
                    if let Some(it) = attr_val(e, &[b"i"]) {
                        r_italic = it == "1" || it == "true";
                    }
                    if let Some(sz) = attr_val(e, &[b"sz"]).and_then(|s| s.parse::<f32>().ok()) {
                        r_size = Some((sz / 100.0).clamp(8.0, 56.0));
                    }
                }
                _ => {}
            },
            Ok(Event::Text(e)) if in_r && in_t => {
                if let Ok(t) = e.unescape() {
                    r_text.push_str(&t);
                }
            }
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"p:bg" => in_bg = false,
                b"a:t" => in_t = false,
                b"a:rPr" => in_rpr = false,
                b"a:pPr" => in_ppr = false,
                b"a:xfrm" => in_xfrm = false,
                b"p:spPr" => in_sppr = false,
                b"p:txBody" => in_txbody = false,
                b"a:r" if in_r => {
                    if !r_text.is_empty() {
                        p_runs.push(WordRun {
                            text: std::mem::take(&mut r_text),
                            bold: r_bold,
                            italic: r_italic,
                            underline: false,
                            strike: false,
                            color: r_color.take(),
                            size_pt: r_size.take(),
                        });
                    }
                    in_r = false;
                    in_rpr = false;
                    in_t = false;
                }
                b"a:p" if in_p => {
                    if !p_runs.is_empty() {
                        if title_text.is_empty() && (shape_role == "title" || paras.is_empty()) {
                            let joined: String = p_runs.iter().map(|r| r.text.as_str()).collect();
                            if !joined.trim().is_empty() {
                                title_text = joined.trim().to_string();
                            }
                        }
                        paras.push(PptParagraph {
                            align: p_align.clone(),
                            bullet: p_bullet,
                            runs: std::mem::take(&mut p_runs),
                        });
                    }
                    in_p = false;
                    in_ppr = false;
                }
                b"p:sp" if in_sp => {
                    if !paras.is_empty() {
                        elements.push(PptElement::TextBox {
                            left_pct: to_pct(off_x, slide_cx),
                            top_pct: to_pct(off_y, slide_cy),
                            width_pct: if ext_cx > 0 { to_pct(ext_cx, slide_cx) } else { 88.0 },
                            height_pct: if ext_cy > 0 { to_pct(ext_cy, slide_cy) } else { 20.0 },
                            bg: shape_bg.take(),
                            role: shape_role.clone(),
                            paragraphs: std::mem::take(&mut paras),
                        });
                    }
                    in_sp = false;
                }
                b"p:pic" if in_pic => {
                    if let Some(entry) = pic_entry.take() {
                        elements.push(PptElement::Image {
                            left_pct: to_pct(off_x, slide_cx),
                            top_pct: to_pct(off_y, slide_cy),
                            width_pct: if ext_cx > 0 { to_pct(ext_cx, slide_cx) } else { 40.0 },
                            height_pct: if ext_cy > 0 { to_pct(ext_cy, slide_cy) } else { 40.0 },
                            entry,
                        });
                    }
                    in_pic = false;
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if title_text.is_empty() {
        title_text = format!("Diapositiva {}", num);
    }

    PptSlideVisual {
        number: num,
        title: title_text,
        bg: slide_bg,
        elements,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_hex_color_handles_rgb_argb_and_auto() {
        assert_eq!(sanitize_hex_color("185ABD"), Some("#185ABD".to_string()));
        assert_eq!(sanitize_hex_color("FF217346"), Some("#217346".to_string()));
        assert_eq!(sanitize_hex_color("auto"), None);
    }

    #[test]
    fn resolve_ooxml_rel_resolves_parent_segments() {
        assert_eq!(
            resolve_ooxml_rel("ppt/slides", "../media/image1.png"),
            "ppt/media/image1.png"
        );
        assert_eq!(
            resolve_ooxml_rel("word", "media/logo.jpg"),
            "word/media/logo.jpg"
        );
    }
}
