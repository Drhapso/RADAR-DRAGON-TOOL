//! Extractor de texto para documentos PDF utilizando lopdf con respaldo por streams y Form XObjects.

use anyhow::{Context, Result};
use lopdf::content::Content;
use lopdf::{Document, Object, ObjectId};
use serde::Serialize;
use std::collections::HashSet;
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
/// Usa `lopdf::Document::extract_text` como vía principal y activa un extractor resiliente sobre
/// el stream de página y sus Form XObjects (`/Subtype /Form`) cuando una página falla o devuelve vacío.
pub fn extract_pdf_preview<P: AsRef<Path>>(
    path: P,
    max_pages: usize,
    max_chars: usize,
) -> Result<PdfPreview> {
    let doc = Document::load(path.as_ref())
        .with_context(|| format!("Error al cargar estructura del PDF: {:?}", path.as_ref()))?;

    let pages = doc.get_pages();
    let total_pages = pages.len();
    let mut full_text = String::with_capacity(4096);
    let mut page_count = 0;

    for (&page_num, &page_id) in &pages {
        if page_count >= max_pages || full_text.len() >= max_chars {
            break;
        }

        let mut page_text = doc
            .extract_text(&[page_num])
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        // Respaldo: si lopdf::extract_text falló por CMap/fuente o el texto está dentro de un Form XObject
        if page_text.is_empty() {
            let rem = max_chars.saturating_sub(full_text.len());
            page_text = extract_page_fallback_text(&doc, page_id, rem);
        }

        if !page_text.is_empty() {
            let rem = max_chars.saturating_sub(full_text.len());
            if page_text.len() > rem {
                let mut end = rem;
                while end > 0 && !page_text.is_char_boundary(end) {
                    end -= 1;
                }
                full_text.push_str(&page_text[..end]);
            } else {
                full_text.push_str(&page_text);
            }
            full_text.push('\n');
        }
        page_count += 1;
    }

    Ok(PdfPreview {
        page_count: total_pages,
        text: full_text,
    })
}

/// Extrae operadores textuales (`Tj`, `TJ`, `'`, `"`) del contenido de una página y de sus Form XObjects.
fn extract_page_fallback_text(doc: &Document, page_id: ObjectId, max_chars: usize) -> String {
    let mut out = String::new();
    if let Ok(content_bytes) = doc.get_page_content(page_id) {
        append_text_from_content_bytes(&content_bytes, &mut out, max_chars);
    }

    // Inspeccionar también Form XObjects (sellos, encabezados, tablas incrustadas, PDFs de ORFEO/Acrobat)
    if out.len() < max_chars {
        let mut visited: HashSet<ObjectId> = HashSet::new();
        if let Ok(page_dict) = doc.get_dictionary(page_id) {
            if let Ok(res_obj) = page_dict.get(b"Resources") {
                extract_form_xobjects_from_resources(doc, res_obj, &mut out, max_chars, 0, &mut visited);
            }
        }
    }

    out.trim().to_string()
}

fn extract_form_xobjects_from_resources(
    doc: &Document,
    res_obj: &Object,
    out: &mut String,
    max_chars: usize,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth >= 4 || out.len() >= max_chars {
        return;
    }
    let res_dict = match res_obj {
        Object::Dictionary(d) => d,
        Object::Reference(id) => match doc.get_dictionary(*id) {
            Ok(d) => d,
            Err(_) => return,
        },
        _ => return,
    };
    let Ok(xobj_entry) = res_dict.get(b"XObject") else {
        return;
    };
    let xobj_dict = match xobj_entry {
        Object::Dictionary(d) => d,
        Object::Reference(id) => match doc.get_dictionary(*id) {
            Ok(d) => d,
            Err(_) => return,
        },
        _ => return,
    };

    for (_, val) in xobj_dict.iter() {
        if out.len() >= max_chars {
            break;
        }
        let Object::Reference(obj_id_ref) = val else {
            continue;
        };
        let obj_id = *obj_id_ref;
        if !visited.insert(obj_id) {
            continue;
        }
        let Ok(Object::Stream(stream)) = doc.get_object(obj_id) else {
            continue;
        };
        let is_form = matches!(
            stream.dict.get(b"Subtype"),
            Ok(Object::Name(subtype)) if subtype.as_slice() == b"Form"
        );
        if !is_form {
            continue;
        }

        let content_bytes = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        append_text_from_content_bytes(&content_bytes, out, max_chars);

        if let Ok(nested_res) = stream.dict.get(b"Resources") {
            extract_form_xobjects_from_resources(doc, nested_res, out, max_chars, depth + 1, visited);
        }
    }
}

fn append_text_from_content_bytes(bytes: &[u8], out: &mut String, max_chars: usize) {
    if bytes.is_empty() || out.len() >= max_chars {
        return;
    }
    let Ok(content) = Content::decode(bytes) else {
        return;
    };

    for op in &content.operations {
        if out.len() >= max_chars {
            break;
        }
        match op.operator.as_str() {
            "Tj" | "'" | "\"" => {
                for operand in &op.operands {
                    if let Object::String(raw, _) = operand {
                        let decoded = decode_pdf_string_bytes(raw);
                        if !decoded.is_empty() {
                            out.push_str(&decoded);
                        }
                    }
                }
                if op.operator == "'" || op.operator == "\"" {
                    out.push('\n');
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = op.operands.first() {
                    append_tj_array(items, out, max_chars);
                }
            }
            "T*" | "TD" | "Td" | "ET" => {
                if !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
}

fn append_tj_array(items: &[Object], out: &mut String, max_chars: usize) {
    for item in items {
        if out.len() >= max_chars {
            break;
        }
        match item {
            Object::String(raw, _) => {
                let decoded = decode_pdf_string_bytes(raw);
                if !decoded.is_empty() {
                    out.push_str(&decoded);
                }
            }
            Object::Integer(n) if *n <= -180 => {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
            }
            Object::Real(f) if *f <= -180.0 => {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
}

fn decode_pdf_string_bytes(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    // UTF-16BE con BOM (FE FF)
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let mut units = Vec::with_capacity((raw.len() - 2) / 2);
        let mut i = 2usize;
        while i + 1 < raw.len() {
            units.push(u16::from_be_bytes([raw[i], raw[i + 1]]));
            i += 2;
        }
        return filter_printable_pdf_text(&String::from_utf16_lossy(&units));
    }
    // UTF-8 válido o Windows-1252 (muy común en PDFDocEncoding/WinAnsiEncoding en español)
    let decoded = match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(raw).0.into_owned(),
    };
    filter_printable_pdf_text(&decoded)
}

fn filter_printable_pdf_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut printable = 0usize;
    let mut total = 0usize;
    for ch in s.chars() {
        if ch == '\0' {
            continue;
        }
        total += 1;
        if ch.is_alphanumeric()
            || ch.is_whitespace()
            || matches!(
                ch,
                '.' | ',' | ':' | ';' | '-' | '_' | '/' | '(' | ')' | '[' | ']' | '°' | 'º' | 'ª'
                    | '¿' | '?' | '¡' | '!' | '%' | '$' | '+' | '=' | '@' | '#' | '"' | '\''
            )
        {
            printable += 1;
            out.push(ch);
        }
    }
    if total == 0 || printable * 10 < total * 6 {
        return String::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_fallback_decodes_winansi_utf16be_and_tj_kerning() {
        // WinAnsi / Windows-1252: "Resolución" (0xF3 = ó)
        let win1252 = b"Resoluci\xf3n 2026";
        assert_eq!(decode_pdf_string_bytes(win1252), "Resolución 2026");

        // UTF-16BE con BOM: "Acta"
        let utf16 = [0xFE, 0xFF, 0x00, b'A', 0x00, b'c', 0x00, b't', 0x00, b'a'];
        assert_eq!(decode_pdf_string_bytes(&utf16), "Acta");

        // TJ con kerning pequeño (-40) NO debe partir palabra; espacio grande (-250) SÍ agrega espacio
        let items = vec![
            Object::String(b"Bogo".to_vec(), lopdf::StringFormat::Literal),
            Object::Integer(-40),
            Object::String(b"t\xe1".to_vec(), lopdf::StringFormat::Literal),
            Object::Integer(-250),
            Object::String(b"D.C.".to_vec(), lopdf::StringFormat::Literal),
        ];
        let mut out = String::new();
        append_tj_array(&items, &mut out, 1000);
        assert_eq!(out, "Bogotá D.C.");
    }
}
