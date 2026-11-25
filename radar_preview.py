from __future__ import annotations

import csv
import hashlib
import io
import mimetypes
import os
import shutil
import subprocess
import tempfile
import threading
import xml.etree.ElementTree as ET
import zipfile
from datetime import datetime
from pathlib import Path
from typing import Any

from radar_indexer import TEXT_EXTRACT_EXTS, format_file_size

MAX_PREVIEW_TEXT_CHARS = 80_000
MAX_CSV_ROWS = 200
MAX_CSV_COLS = 24
MAX_EXCEL_SHEETS = 8
MAX_EXCEL_ROWS = 180
MAX_EXCEL_COLS = 26
MAX_PPT_SLIDES = 60
MAX_ZIP_ENTRIES = 200

_OFFICE_PDF_LOCK = threading.Lock()

EXTRA_MIMES = {
    ".pdf": "application/pdf",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".webp": "image/webp",
    ".gif": "image/gif",
    ".bmp": "image/bmp",
    ".svg": "image/svg+xml",
    ".ico": "image/x-icon",
    ".avif": "image/avif",
    ".mp4": "video/mp4",
    ".webm": "video/webm",
    ".mov": "video/quicktime",
    ".m4v": "video/x-m4v",
    ".ogv": "video/ogg",
    ".mp3": "audio/mpeg",
    ".wav": "audio/wav",
    ".ogg": "audio/ogg",
    ".oga": "audio/ogg",
    ".opus": "audio/opus",
    ".flac": "audio/flac",
    ".m4a": "audio/mp4",
    ".aac": "audio/aac",
    ".json": "application/json; charset=utf-8",
    ".txt": "text/plain; charset=utf-8",
    ".md": "text/markdown; charset=utf-8",
    ".csv": "text/csv; charset=utf-8",
    ".html": "text/html; charset=utf-8",
    ".htm": "text/html; charset=utf-8",
}


def mime_for_path(file_path: Path) -> str:
    suffix = file_path.suffix.lower()
    if suffix in EXTRA_MIMES:
        return EXTRA_MIMES[suffix]
    guessed, _ = mimetypes.guess_type(str(file_path))
    return guessed or "application/octet-stream"


def find_soffice_executable(configured_path: str = "") -> Path | None:
    candidates: list[Path] = []
    if configured_path:
        candidates.append(Path(configured_path))
    for env_key in ("ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"):
        base = os.environ.get(env_key)
        if not base:
            continue
        for rel in (
            r"LibreOffice\program\soffice.com",
            r"LibreOffice\program\soffice.exe",
            r"Programs\LibreOffice\program\soffice.com",
            r"Programs\LibreOffice\program\soffice.exe",
        ):
            candidates.append(Path(base) / rel)

    which_lo = shutil.which("soffice")
    if which_lo:
        candidates.append(Path(which_lo))

    for cand in candidates:
        try:
            if cand.is_file():
                return cand
        except OSError:
            continue
    return None


def get_office_pdf_cache_path(file_path: Path, cache_dir: Path) -> Path:
    st = file_path.stat()
    key_raw = f"{str(file_path).lower()}|{st.st_size}|{int(st.st_mtime * 1000)}"
    digest = hashlib.sha256(key_raw.encode("utf-8")).hexdigest()[:24]
    return cache_dir / f"{digest}.pdf"


def convert_office_to_pdf(
    file_path: Path,
    cache_dir: Path,
    configured_soffice: str = "",
) -> Path | None:
    soffice = find_soffice_executable(configured_soffice)
    if not soffice:
        return None
    cache_dir.mkdir(parents=True, exist_ok=True)
    cached_pdf = get_office_pdf_cache_path(file_path, cache_dir)
    if cached_pdf.is_file() and cached_pdf.stat().st_size > 0:
        return cached_pdf

    with _OFFICE_PDF_LOCK:
        if cached_pdf.is_file() and cached_pdf.stat().st_size > 0:
            return cached_pdf

        with tempfile.TemporaryDirectory(prefix="dragon_lo_") as tmp_dir:
            tmp_out = Path(tmp_dir)
            profile_dir = tmp_out / "lo_profile"
            profile_dir.mkdir(parents=True, exist_ok=True)
            profile_uri = profile_dir.as_uri()
            cmd = [
                str(soffice),
                "--headless",
                "--nologo",
                "--nodefault",
                "--norestore",
                "--nolockcheck",
                f"-env:UserInstallation={profile_uri}",
                "--convert-to",
                "pdf",
                "--outdir",
                str(tmp_out),
                str(file_path),
            ]
            creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
            res = subprocess.run(
                cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=60,
                creationflags=creationflags,
                check=False,
            )
            if res.returncode != 0:
                return None

            pdfs = list(tmp_out.glob("*.pdf"))
            if not pdfs:
                return None
            tmp_target = cached_pdf.with_suffix(".tmp")
            shutil.copyfile(pdfs[0], tmp_target)
            tmp_target.replace(cached_pdf)
            return cached_pdf


def read_text_preview(file_path: Path, max_chars: int = MAX_PREVIEW_TEXT_CHARS) -> str:
    raw = file_path.read_bytes()[: max_chars * 2]
    if b"\x00" in raw[:4096]:
        return ""
    for enc in ("utf-8-sig", "utf-8", "cp1252", "latin-1"):
        try:
            return raw.decode(enc)[:max_chars]
        except UnicodeDecodeError:
            continue
    return raw.decode("utf-8", errors="ignore")[:max_chars]


def read_zip_asset(file_path: Path, entry_name: str) -> tuple[bytes, str] | None:
    safe_entry = entry_name.lstrip("/").replace("\\", "/")
    if ".." in safe_entry.split("/"):
        return None
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            if safe_entry not in zf.namelist():
                return None
            data = zf.read(safe_entry)
            return data, mime_for_path(Path(safe_entry))
    except Exception:
        return None


def build_file_preview(
    file_path: Path,
    cache_dir: Path,
    configured_soffice: str = "",
) -> dict[str, Any]:
    st = file_path.stat()
    size_bytes = int(st.st_size)
    modified = datetime.fromtimestamp(st.st_mtime).strftime("%Y-%m-%d %H:%M")
    name = file_path.name
    ext = file_path.suffix.lower().lstrip(".")
    mime = mime_for_path(file_path)

    soffice = find_soffice_executable(configured_soffice)
    office_pdf_available = soffice is not None
    office_pdf_cached = False
    if office_pdf_available:
        try:
            cp = get_office_pdf_cache_path(file_path, cache_dir)
            office_pdf_cached = cp.is_file() and cp.stat().st_size > 0
        except OSError:
            office_pdf_cached = False

    payload: dict[str, Any]
    if ext == "pdf":
        page_count, content = _preview_pdf(file_path)
        payload = {
            "success": True,
            "kind": "pdf",
            "page_count": page_count,
            "content": content,
        }
    elif ext in {"png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg", "avif"}:
        payload = {"success": True, "kind": "image"}
    elif ext in {"mp4", "webm", "mov", "m4v", "ogv"}:
        payload = {"success": True, "kind": "video"}
    elif ext in {"mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac"}:
        payload = {"success": True, "kind": "audio"}
    elif ext in {"md", "markdown", "mdx"}:
        payload = {
            "success": True,
            "kind": "markdown",
            "content": read_text_preview(file_path),
        }
    elif ext in {"html", "htm"}:
        payload = {
            "success": True,
            "kind": "html",
            "content": read_text_preview(file_path),
        }
    elif ext in {"csv", "tsv"}:
        delim = "\t" if ext == "tsv" else ","
        raw_text = read_text_preview(file_path, 120_000)
        rows, total_rows = _parse_csv_rows(raw_text, delim)
        payload = {
            "success": True,
            "kind": "table",
            "sheets": [{"name": name, "rows": rows, "total_rows": total_rows}],
            "content": raw_text,
        }
    elif ext in {"xlsx", "xlsm"}:
        sheets, text_dump = _preview_xlsx(file_path)
        payload = {
            "success": True,
            "kind": "excel",
            "sheets": sheets,
            "content": text_dump,
        }
    elif ext in {"xls", "xlsb", "ods"}:
        payload = {
            "success": True,
            "kind": "excel",
            "legacy": True,
            "content": _extract_odf_text(file_path) if ext == "ods" else "",
        }
    elif ext in {"docx", "docm", "dotx", "dotm"}:
        paragraphs, word_visual, text_dump = _preview_docx(file_path)
        payload = {
            "success": True,
            "kind": "word",
            "paragraphs": paragraphs,
            "word_visual": word_visual,
            "content": text_dump,
        }
    elif ext in {"doc", "odt", "ott", "rtf"}:
        content = _extract_odf_text(file_path) if ext in {"odt", "ott"} else (
            read_text_preview(file_path, 40_000) if ext == "rtf" else ""
        )
        payload = {
            "success": True,
            "kind": "word",
            "legacy": True,
            "content": content,
        }
    elif ext in {"pptx", "pptm", "ppsx"}:
        slides, text_dump = _preview_pptx(file_path)
        payload = {
            "success": True,
            "kind": "powerpoint",
            "slides": slides,
            "ppt_visual": {
                "aspect_ratio": 1.777,
                "slides": [
                    {
                        "number": s["number"],
                        "title": s["title"],
                        "bg": "#FFFFFF",
                        "notes": "",
                        "elements": [
                            {
                                "kind": "textbox",
                                "role": "title",
                                "left_pct": 6.0,
                                "top_pct": 8.0,
                                "width_pct": 88.0,
                                "height_pct": 18.0,
                                "paragraphs": [
                                    {"align": "left", "bullet": False, "runs": [{"text": s["title"], "bold": True}]}
                                ],
                            },
                            {
                                "kind": "textbox",
                                "role": "body",
                                "left_pct": 6.0,
                                "top_pct": 28.0,
                                "width_pct": 88.0,
                                "height_pct": 64.0,
                                "paragraphs": [
                                    {"align": "left", "bullet": True, "runs": [{"text": ln}]}
                                    for ln in s["lines"][:14]
                                ],
                            },
                        ],
                    }
                    for s in slides
                ],
            },
            "content": text_dump,
        }
    elif ext in {"ppt", "odp"}:
        payload = {
            "success": True,
            "kind": "powerpoint",
            "legacy": True,
            "content": _extract_odf_text(file_path) if ext == "odp" else "",
        }
    elif ext in {"zip", "jar", "apk", "nupkg"}:
        total, entries = _preview_zip(file_path)
        payload = {
            "success": True,
            "kind": "archive",
            "archive_total": total,
            "archive_entries": entries,
        }
    elif ext in TEXT_EXTRACT_EXTS:
        payload = {
            "success": True,
            "kind": "code",
            "content": read_text_preview(file_path, 60_000),
        }
    else:
        payload = {
            "success": True,
            "kind": "unsupported",
            "note": "Este formato binario/especializado no tiene renderizado web integrado.",
        }

    payload.update(
        {
            "file": str(file_path),
            "name": name,
            "extension": ext,
            "mime": mime,
            "size_bytes": size_bytes,
            "size_label": format_file_size(size_bytes),
            "modified": modified,
            "office_pdf_available": office_pdf_available,
            "office_pdf_cached": office_pdf_cached,
        }
    )
    return payload


def _preview_pdf(file_path: Path) -> tuple[int | None, str]:
    try:
        import pymupdf  # type: ignore

        with pymupdf.open(str(file_path)) as doc:
            page_count = len(doc)
            parts: list[str] = []
            total_len = 0
            for idx in range(min(page_count, 50)):
                txt = doc[idx].get_text("text") or ""
                if txt:
                    parts.append(f"--- Página {idx + 1} ---\n{txt.strip()}")
                    total_len += len(txt)
                    if total_len >= 30_000:
                        break
            return page_count, "\n\n".join(parts)[:30_000]
    except Exception:
        return None, ""


def _parse_csv_rows(raw_text: str, delimiter: str) -> tuple[list[list[str]], int]:
    rows: list[list[str]] = []
    total = 0
    reader = csv.reader(io.StringIO(raw_text), delimiter=delimiter)
    for r in reader:
        if not any(cell.strip() for cell in r):
            continue
        total += 1
        if len(rows) < MAX_CSV_ROWS:
            rows.append([cell.strip() for cell in r[:MAX_CSV_COLS]])
    return rows, total


def _preview_xlsx(file_path: Path) -> tuple[list[dict[str, Any]], str]:
    sheets: list[dict[str, Any]] = []
    text_lines: list[str] = []
    try:
        import openpyxl  # type: ignore

        wb = openpyxl.load_workbook(str(file_path), read_only=True, data_only=True)
        try:
            for sname in wb.sheetnames[:MAX_EXCEL_SHEETS]:
                ws = wb[sname]
                rows: list[list[str]] = []
                total_rows = 0
                for row in ws.iter_rows(values_only=True):
                    vals = ["" if v is None else str(v) for v in row[:MAX_EXCEL_COLS]]
                    if not any(v.strip() for v in vals):
                        continue
                    total_rows += 1
                    if len(rows) < MAX_EXCEL_ROWS:
                        rows.append(vals)
                    if len(text_lines) < 350:
                        text_lines.append(" | ".join(v for v in vals if v.strip()))
                sheets.append({"name": str(sname), "rows": rows, "total_rows": total_rows})
        finally:
            wb.close()
    except Exception:
        pass
    return sheets, "\n".join(text_lines)[:30_000]


def _preview_docx(file_path: Path) -> tuple[list[str], dict[str, Any], str]:
    paragraphs: list[str] = []
    blocks: list[dict[str, Any]] = []
    try:
        import docx  # type: ignore

        document = docx.Document(str(file_path))
        for p in document.paragraphs[:450]:
            txt = (p.text or "").strip()
            if not txt:
                continue
            paragraphs.append(txt)
            style_name = (p.style.name or "").lower() if p.style else ""
            w_style = "normal"
            if "heading 1" in style_name or "título 1" in style_name or "title" in style_name:
                w_style = "h1"
            elif "heading 2" in style_name or "título 2" in style_name:
                w_style = "h2"
            elif "heading 3" in style_name or "título 3" in style_name:
                w_style = "h3"
            runs = []
            for r in p.runs:
                if not r.text:
                    continue
                runs.append(
                    {
                        "text": r.text,
                        "bold": bool(r.bold),
                        "italic": bool(r.italic),
                        "underline": bool(r.underline),
                    }
                )
            if not runs:
                runs = [{"text": txt}]
            blocks.append(
                {
                    "kind": "paragraph",
                    "style": w_style,
                    "align": "left",
                    "is_list": "list" in style_name,
                    "runs": runs,
                }
            )
        for tbl in document.tables[:12]:
            tbl_rows = []
            for row in tbl.rows[:40]:
                cells = []
                for cell in row.cells[:12]:
                    ctext = (cell.text or "").strip()
                    if ctext:
                        paragraphs.append(ctext)
                    cells.append(
                        {
                            "colspan": 1,
                            "bg": None,
                            "paragraphs": [
                                {"style": "normal", "align": "left", "runs": [{"text": ctext}]}
                            ],
                        }
                    )
                tbl_rows.append(cells)
            if tbl_rows:
                blocks.append({"kind": "table", "rows": tbl_rows})
    except Exception:
        try:
            with zipfile.ZipFile(file_path, "r") as zf:
                root = ET.fromstring(zf.read("word/document.xml"))
            for p_el in root.iter():
                if p_el.tag.endswith("}p"):
                    txt = "".join(t.text for t in p_el.iter() if t.tag.endswith("}t") and t.text).strip()
                    if txt:
                        paragraphs.append(txt)
                        blocks.append(
                            {
                                "kind": "paragraph",
                                "style": "normal",
                                "align": "left",
                                "runs": [{"text": txt}],
                            }
                        )
        except Exception:
            pass

    plain = "\n\n".join(paragraphs)[:40_000]
    return paragraphs, {"blocks": blocks, "truncated": len(blocks) >= 450, "plain_text": plain}, plain


def _preview_pptx(file_path: Path) -> tuple[list[dict[str, Any]], str]:
    slides: list[dict[str, Any]] = []
    text_parts: list[str] = []
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            slide_names = sorted(
                n for n in zf.namelist()
                if n.startswith("ppt/slides/slide") and n.endswith(".xml")
            )
            for idx, sname in enumerate(slide_names[:MAX_PPT_SLIDES], start=1):
                root = ET.fromstring(zf.read(sname))
                lines = [
                    el.text.strip()
                    for el in root.iter()
                    if el.tag.endswith("}t") and el.text and el.text.strip()
                ]
                title = lines[0] if lines else f"Diapositiva {idx}"
                body_lines = lines[1:] if len(lines) > 1 else []
                slides.append({"number": idx, "title": title, "lines": body_lines})
                text_parts.append(f"=== Diapositiva {idx}: {title} ===\n" + "\n".join(body_lines))
    except Exception:
        pass
    return slides, "\n\n".join(text_parts)[:40_000]


def _preview_zip(file_path: Path) -> tuple[int, list[dict[str, Any]]]:
    entries: list[dict[str, Any]] = []
    total = 0
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            infos = zf.infolist()
            total = len(infos)
            for info in infos[:MAX_ZIP_ENTRIES]:
                entries.append(
                    {
                        "path": info.filename,
                        "is_dir": info.is_dir(),
                        "size": int(info.file_size),
                        "compressed_size": int(info.compress_size),
                    }
                )
    except Exception:
        pass
    return total, entries


def _extract_odf_text(file_path: Path) -> str:
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            if "content.xml" not in zf.namelist():
                return ""
            root = ET.fromstring(zf.read("content.xml"))
            return "\n".join(t.strip() for t in root.itertext() if t and t.strip())[:40_000]
    except Exception:
        return ""
