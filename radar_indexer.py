from __future__ import annotations

import fnmatch
import logging
import os
import re
import shlex
import shutil
import socket
import sqlite3
import threading
import time
import unicodedata
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

LOG = logging.getLogger("dragon_radar.indexer")

MAX_CONTENT_CHARS = 48_000
MAX_CONTENT_FILE_BYTES = 35 * 1024 * 1024
MAX_PDF_PAGES_INDEX = 30
MAX_XLSX_SHEETS_INDEX = 8
MAX_XLSX_ROWS_INDEX = 250
BATCH_COMMIT_SIZE = 200

SYSTEM_EXCLUDED_DIRS = {
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "system volume information",
    "recovery",
    "boot",
    "perflogs",
    "msocache",
    "config.msi",
    "windowsapps",
    "windows.old",
    "onedrivetemp",
}

NOISY_EXCLUDED_DIRS = {
    "appdata",
    "application data",
    "cookies",
    "local settings",
    "nethood",
    "printhood",
    "recent",
    "sendto",
    "start menu",
    "templates",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    "target",
    ".dragon-data",
    ".dragon-index",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".idea",
    ".vscode",
    "dist-info",
    "site-packages",
    "temp",
    "tmp",
    "cache",
    "caches",
    "crashdumps",
}

EXCLUDED_USER_PROFILES = {
    "default",
    "default user",
    "public",
    "all users",
    "defaultuser0",
    "defaultuser1",
}

DOCUMENT_EXTS = {
    "pdf", "doc", "docx", "docm", "dotx", "dotm", "odt", "ott", "rtf",
    "xls", "xlsx", "xlsm", "xlsb", "ods", "csv", "tsv",
    "ppt", "pptx", "pptm", "ppsx", "odp",
    "txt", "md", "markdown", "mdx", "epub", "html", "htm", "xml",
    "json", "yaml", "yml", "ini", "cfg", "conf", "log", "sql",
}

MEDIA_EXTS = {
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ogv", "3gp", "mpeg", "mpg",
    "mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", "wma", "mid", "midi",
}

DESIGN_EXTS = {
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg", "avif", "tif", "tiff", "heic",
    "psd", "ai", "eps", "indd", "fig", "sketch", "xd", "cdr",
    "dwg", "dxf", "stl", "obj", "fbx", "blend", "3ds", "step", "stp", "iges",
}

CODE_EXTS = {
    "py", "js", "jsx", "ts", "tsx", "rs", "go", "java", "kt", "c", "h", "cpp", "hpp", "cs",
    "php", "rb", "sh", "bash", "zsh", "ps1", "psm1", "bat", "cmd", "css", "scss", "less",
    "vue", "svelte", "toml", "gradle", "cmake", "dockerfile", "makefile",
}

TEXT_EXTRACT_EXTS = (
    CODE_EXTS
    | {
        "txt", "md", "markdown", "mdx", "csv", "tsv", "json", "xml", "html", "htm",
        "yaml", "yml", "ini", "cfg", "conf", "log", "sql", "env", "properties", "rtf",
    }
)

OFFICE_EXTRACT_EXTS = {"pdf", "docx", "docm", "xlsx", "xlsm", "pptx", "pptm", "ppsx", "odt", "ods", "odp"}


def fold_text(value: str) -> str:
    if not value:
        return ""
    decomposed = unicodedata.normalize("NFD", str(value))
    stripped = "".join(ch for ch in decomposed if unicodedata.category(ch) != "Mn")
    return stripped.lower()


def format_file_size(size_bytes: int) -> str:
    size = max(0, int(size_bytes or 0))
    if size < 1024:
        return f"{size} B"
    if size < 1024 * 1024:
        return f"{size / 1024:.1f} KB"
    if size < 1024 * 1024 * 1024:
        return f"{size / (1024 * 1024):.1f} MB"
    return f"{size / (1024 * 1024 * 1024):.2f} GB"


def classify_extension(ext: str) -> tuple[str, str]:
    e = (ext or "").lower().lstrip(".")
    if e == "pdf":
        return "document", "PDF"
    if e in {"doc", "docx", "docm", "dotx", "dotm", "odt", "ott", "rtf"}:
        return "document", "WORD"
    if e in {"xls", "xlsx", "xlsm", "xlsb", "ods", "csv", "tsv"}:
        return "document", "EXCEL"
    if e in {"ppt", "pptx", "pptm", "ppsx", "odp"}:
        return "document", "PPTX"
    if e == "epub":
        return "document", "EBOOK"
    if e in {"txt", "md", "markdown", "mdx", "log"}:
        return "document", "DOC"
    if e == "rs":
        return "code", "RUST"
    if e in {"js", "jsx", "ts", "tsx"}:
        return "code", "JS"
    if e in CODE_EXTS or e in {"json", "xml", "html", "htm", "yaml", "yml", "sql", "ini", "cfg"}:
        return "code", "CODE"
    if e in {"svg", "ai", "eps", "cdr"}:
        return "design", "VECTOR"
    if e in {"psd", "indd", "fig", "sketch", "xd"}:
        return "design", "DESIGN"
    if e in {"dwg", "dxf", "stl", "obj", "fbx", "blend", "3ds", "step", "stp"}:
        return "design", "CAD/3D"
    if e in DESIGN_EXTS:
        return "design", "IMG"
    if e in {"mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ogv", "3gp", "mpeg", "mpg"}:
        return "media", "VIDEO"
    if e in MEDIA_EXTS:
        return "media", "AUDIO"
    if e in {"zip", "rar", "7z", "tar", "gz", "jar", "apk", "nupkg"}:
        return "archive", "ZIP"
    return "other", (e.upper()[:6] if e else "FILE")


def extract_drive_label(path_str: str) -> str:
    p = str(path_str or "")
    if len(p) >= 2 and p[1] == ":":
        return f"{p[0].upper()}:\\"
    if p.startswith("\\\\"):
        parts = [seg for seg in p.split("\\") if seg]
        if len(parts) >= 2:
            return f"\\\\{parts[0]}\\{parts[1]}"
    return "LOCAL"


def extract_searchable_text(file_path: Path, ext: str, size_bytes: int) -> str:
    if size_bytes <= 0 or size_bytes > MAX_CONTENT_FILE_BYTES:
        return ""
    e = ext.lower()
    try:
        if e in TEXT_EXTRACT_EXTS:
            raw = file_path.read_bytes()[: MAX_CONTENT_CHARS * 2]
            if b"\x00" in raw[:4096]:
                return ""
            for enc in ("utf-8", "utf-8-sig", "cp1252", "latin-1"):
                try:
                    text = raw.decode(enc)
                    break
                except UnicodeDecodeError:
                    continue
            else:
                text = raw.decode("utf-8", errors="ignore")
            if e == "rtf":
                text = re.sub(r"\\[a-zA-Z]+\d* ?", " ", text)
                text = re.sub(r"[{}]", " ", text)
            return _clean_extracted_text(text)

        if e == "pdf":
            return _extract_pdf_text(file_path)

        if e in {"docx", "docm"}:
            return _extract_docx_text(file_path)

        if e in {"xlsx", "xlsm"}:
            return _extract_xlsx_text(file_path)

        if e in {"pptx", "pptm", "ppsx"}:
            return _extract_pptx_text(file_path)

        if e in {"odt", "ods", "odp"}:
            return _extract_opendoc_text(file_path)
    except Exception:
        return ""
    return ""


def _clean_extracted_text(text: str) -> str:
    if not text:
        return ""
    cleaned = re.sub(r"\s+", " ", text).strip()
    return cleaned[:MAX_CONTENT_CHARS]


def _extract_pdf_text(file_path: Path) -> str:
    try:
        import pymupdf  # type: ignore
    except ImportError:
        return ""
    chunks: list[str] = []
    total_len = 0
    with pymupdf.open(str(file_path)) as doc:
        page_limit = min(len(doc), MAX_PDF_PAGES_INDEX)
        for idx in range(page_limit):
            page_txt = doc[idx].get_text("text") or ""
            if page_txt:
                chunks.append(page_txt)
                total_len += len(page_txt)
                if total_len >= MAX_CONTENT_CHARS:
                    break
    return _clean_extracted_text(" ".join(chunks))


def _extract_docx_text(file_path: Path) -> str:
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            xml_bytes = zf.read("word/document.xml")
        root = ET.fromstring(xml_bytes)
        parts: list[str] = []
        total_len = 0
        for elem in root.iter():
            if elem.tag.endswith("}t") and elem.text:
                parts.append(elem.text)
                total_len += len(elem.text)
                if total_len >= MAX_CONTENT_CHARS:
                    break
        return _clean_extracted_text(" ".join(parts))
    except Exception:
        return ""


def _extract_xlsx_text(file_path: Path) -> str:
    try:
        import openpyxl  # type: ignore

        wb = openpyxl.load_workbook(str(file_path), read_only=True, data_only=True)
        try:
            parts: list[str] = []
            total_len = 0
            for sheet_name in wb.sheetnames[:MAX_XLSX_SHEETS_INDEX]:
                ws = wb[sheet_name]
                parts.append(str(sheet_name))
                row_count = 0
                for row in ws.iter_rows(values_only=True):
                    row_count += 1
                    if row_count > MAX_XLSX_ROWS_INDEX:
                        break
                    for val in row:
                        if val is not None:
                            s = str(val).strip()
                            if s:
                                parts.append(s)
                                total_len += len(s) + 1
                    if total_len >= MAX_CONTENT_CHARS:
                        break
                if total_len >= MAX_CONTENT_CHARS:
                    break
            return _clean_extracted_text(" ".join(parts))
        finally:
            wb.close()
    except Exception:
        # Fast zipfile XML fallback if openpyxl fails
        try:
            with zipfile.ZipFile(file_path, "r") as zf:
                if "xl/sharedStrings.xml" in zf.namelist():
                    root = ET.fromstring(zf.read("xl/sharedStrings.xml"))
                    parts = [el.text for el in root.iter() if el.tag.endswith("}t") and el.text]
                    return _clean_extracted_text(" ".join(parts))
        except Exception:
            return ""
    return ""


def _extract_pptx_text(file_path: Path) -> str:
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            slide_names = sorted(
                n for n in zf.namelist()
                if n.startswith("ppt/slides/slide") and n.endswith(".xml")
            )
            parts: list[str] = []
            total_len = 0
            for sname in slide_names[:80]:
                root = ET.fromstring(zf.read(sname))
                for elem in root.iter():
                    if elem.tag.endswith("}t") and elem.text:
                        parts.append(elem.text)
                        total_len += len(elem.text) + 1
                        if total_len >= MAX_CONTENT_CHARS:
                            break
                if total_len >= MAX_CONTENT_CHARS:
                    break
            return _clean_extracted_text(" ".join(parts))
    except Exception:
        return ""


def _extract_opendoc_text(file_path: Path) -> str:
    try:
        with zipfile.ZipFile(file_path, "r") as zf:
            if "content.xml" not in zf.namelist():
                return ""
            root = ET.fromstring(zf.read("content.xml"))
            parts = [t.strip() for t in root.itertext() if t and t.strip()]
            return _clean_extracted_text(" ".join(parts))
    except Exception:
        return ""


def discover_local_targets() -> tuple[list[str], list[Path]]:
    """Descubre unidades fijas locales y carpetas de datos de usuario (sin SO)."""
    drives: list[str] = []
    if hasattr(os, "listdrives"):
        raw_drives = os.listdrives()
    else:
        raw_drives = [f"{ch}:\\" for ch in "CDEFGHIJKLMNOPQRSTUVWXYZ" if os.path.isdir(f"{ch}:\\")]

    for d in raw_drives:
        norm = d if d.endswith("\\") else f"{d}\\"
        try:
            # Omite unidades opticas vacias o inaccesibles sin usar Win32 FFI
            usage = shutil.disk_usage(norm)
            if usage.total > 0 and os.path.isdir(norm):
                drives.append(norm.upper())
        except OSError:
            continue

    targets: list[Path] = []
    seen: set[str] = set()

    def add_target(path: Path) -> None:
        key = str(path).lower()
        if key not in seen and path.is_dir():
            seen.add(key)
            targets.append(path)

    for drv in drives:
        drv_path = Path(drv)
        if drv.startswith("C:"):
            users_dir = drv_path / "Users"
            if users_dir.is_dir():
                try:
                    for entry in os.scandir(users_dir):
                        if not entry.is_dir(follow_symlinks=False):
                            continue
                        if entry.name.lower() in EXCLUDED_USER_PROFILES or entry.name.startswith("."):
                            continue
                        profile = Path(entry.path)
                        base_folders = ["Documents", "Desktop", "Downloads", "Pictures", "Videos", "Music"]
                        for sub in base_folders:
                            sub_path = profile / sub
                            if sub_path.is_dir():
                                add_target(sub_path)
                        try:
                            for sub_entry in os.scandir(profile):
                                if not sub_entry.is_dir(follow_symlinks=False):
                                    continue
                                low = sub_entry.name.lower()
                                if low.startswith(".") or low in NOISY_EXCLUDED_DIRS:
                                    continue
                                if sub_entry.name in base_folders:
                                    continue
                                add_target(Path(sub_entry.path))
                        except OSError:
                            pass
                except OSError:
                    pass
            try:
                for entry in os.scandir(drv_path):
                    if not entry.is_dir(follow_symlinks=False):
                        continue
                    low = entry.name.lower()
                    if low == "users" or low in SYSTEM_EXCLUDED_DIRS or low.startswith("."):
                        continue
                    add_target(Path(entry.path))
            except OSError:
                pass
        else:
            try:
                for entry in os.scandir(drv_path):
                    if not entry.is_dir(follow_symlinks=False):
                        continue
                    low = entry.name.lower()
                    if low in SYSTEM_EXCLUDED_DIRS or low.startswith("$") or low.startswith("."):
                        continue
                    add_target(Path(entry.path))
            except OSError:
                pass

    return drives, targets


@dataclass
class IndexTelemetry:
    phase: str = "READY"
    percent: float = 100.0
    target_index: int = 0
    total_targets: int = 0
    current_target: str = ""
    current_file: str = ""
    target_processed: int = 0
    target_total: int = 0
    new_files: int = 0
    updated_files: int = 0
    moved_files: int = 0
    deleted_files: int = 0
    skipped_files: int = 0
    last_checkpoint: str = ""
    last_sync_ms: int = 0
    initial_scan_completed: bool = False
    drives: list[dict[str, Any]] = field(default_factory=list)


class RadarIndexer:
    """Motor puro Python de indexación y búsqueda híbrida sobre SQLite WAL + FTS5."""

    def __init__(
        self,
        db_path: Path,
        machine_name: str | None = None,
        shared_roots: list[Path] | None = None,
        storage_mode: str = "LOCAL_SERVICE",
    ) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self.machine_name = machine_name or os.environ.get("COMPUTERNAME") or socket.gethostname() or "LOCAL"
        self.shared_roots = [Path(p) for p in (shared_roots or [])]
        self.storage_mode = storage_mode

        self._write_lock = threading.Lock()
        self._telemetry_lock = threading.Lock()
        self._stop_event = threading.Event()
        self._sync_running = threading.Event()
        self._sync_thread: threading.Thread | None = None

        self.telemetry = IndexTelemetry()
        self._init_db()
        self._load_persisted_state()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(str(self.db_path), timeout=20.0, check_same_thread=False)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL;")
        conn.execute("PRAGMA synchronous=NORMAL;")
        conn.execute("PRAGMA temp_store=MEMORY;")
        conn.execute("PRAGMA busy_timeout=15000;")
        return conn

    def _init_db(self) -> None:
        with self._write_lock:
            conn = self._connect()
            try:
                conn.executescript(
                    """
                    CREATE TABLE IF NOT EXISTS files (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        path TEXT UNIQUE NOT NULL,
                        name TEXT NOT NULL,
                        name_fold TEXT NOT NULL,
                        path_fold TEXT NOT NULL,
                        ext TEXT NOT NULL,
                        category TEXT NOT NULL,
                        badge TEXT NOT NULL,
                        drive TEXT NOT NULL,
                        size INTEGER NOT NULL DEFAULT 0,
                        mtime_ms INTEGER NOT NULL DEFAULT 0,
                        has_content INTEGER NOT NULL DEFAULT 0,
                        snippet TEXT NOT NULL DEFAULT '',
                        scan_gen INTEGER NOT NULL DEFAULT 0
                    );
                    CREATE INDEX IF NOT EXISTS idx_files_ext ON files(ext);
                    CREATE INDEX IF NOT EXISTS idx_files_category ON files(category);
                    CREATE INDEX IF NOT EXISTS idx_files_drive ON files(drive);
                    CREATE INDEX IF NOT EXISTS idx_files_name_fold ON files(name_fold);
                    CREATE INDEX IF NOT EXISTS idx_files_scan_gen ON files(scan_gen);

                    CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
                        name,
                        path,
                        content,
                        tokenize='unicode61 remove_diacritics 2'
                    );

                    CREATE TABLE IF NOT EXISTS settings (
                        key TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                    );
                    """
                )
                conn.commit()
            finally:
                conn.close()

    def _load_persisted_state(self) -> None:
        initial_done = self.get_setting("initial_scan_completed") == "1"
        last_cp = self.get_setting("last_checkpoint") or ""
        last_sync_raw = self.get_setting("last_sync_ms") or "0"
        try:
            last_sync_ms = int(last_sync_raw)
        except ValueError:
            last_sync_ms = 0
        with self._telemetry_lock:
            self.telemetry.initial_scan_completed = initial_done
            self.telemetry.last_checkpoint = last_cp
            self.telemetry.last_sync_ms = last_sync_ms
            self.telemetry.phase = "READY" if initial_done else "INITIAL_INDEXING"
            self.telemetry.percent = 100.0 if initial_done else 0.0

    def get_setting(self, key: str) -> str | None:
        conn = self._connect()
        try:
            row = conn.execute("SELECT value FROM settings WHERE key = ?", (key,)).fetchone()
            return str(row["value"]) if row else None
        finally:
            conn.close()

    def set_setting(self, key: str, value: str) -> None:
        with self._write_lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT INTO settings(key, value) VALUES (?, ?) "
                    "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    (key, str(value)),
                )
                conn.commit()
            finally:
                conn.close()

    def get_counts(self) -> tuple[int, int]:
        conn = self._connect()
        try:
            row = conn.execute(
                "SELECT COUNT(*) AS total, COALESCE(SUM(has_content), 0) AS with_content FROM files"
            ).fetchone()
            return int(row["total"] or 0), int(row["with_content"] or 0)
        finally:
            conn.close()

    def is_sync_running(self) -> bool:
        return self._sync_running.is_set()

    def stop(self) -> None:
        self._stop_event.set()
        t = self._sync_thread
        if t and t.is_alive():
            t.join(timeout=3.0)

    def get_telemetry_snapshot(self) -> dict[str, Any]:
        with self._telemetry_lock:
            t = self.telemetry
            return {
                "phase": t.phase,
                "percent": round(t.percent, 2),
                "target_index": t.target_index,
                "total_targets": t.total_targets,
                "current_target": t.current_target,
                "current_file": t.current_file,
                "target_processed": t.target_processed,
                "target_total": t.target_total,
                "new_files": t.new_files,
                "updated_files": t.updated_files,
                "moved_files": t.moved_files,
                "deleted_files": t.deleted_files,
                "skipped_files": t.skipped_files,
                "last_checkpoint": t.last_checkpoint,
                "last_sync_ms": t.last_sync_ms,
                "initial_scan_completed": t.initial_scan_completed,
                "drives": list(t.drives),
            }

    def start_background_sync(self, custom_targets: list[Path] | None = None) -> bool:
        if self._sync_running.is_set():
            return False
        self._sync_running.set()
        self._sync_thread = threading.Thread(
            target=self._run_sync_worker,
            args=(custom_targets,),
            name="dragon-radar-sync",
            daemon=True,
        )
        self._sync_thread.start()
        return True

    def sync_blocking(self, custom_targets: list[Path] | None = None) -> dict[str, Any]:
        self._sync_running.set()
        self._run_sync_worker(custom_targets)
        return self.get_telemetry_snapshot()

    def _resolve_targets(self, custom_targets: list[Path] | None) -> tuple[list[str], list[Path]]:
        if custom_targets is not None:
            valid = [Path(p) for p in custom_targets if Path(p).is_dir()]
            drives = sorted({extract_drive_label(str(p)) for p in valid})
            return drives, valid
        if self.shared_roots:
            valid = [p for p in self.shared_roots if p.is_dir()]
            drives = sorted({extract_drive_label(str(p)) for p in valid})
            return drives, valid
        return discover_local_targets()

    def _run_sync_worker(self, custom_targets: list[Path] | None = None) -> None:
        try:
            drives, targets = self._resolve_targets(custom_targets)
            scan_gen = int(time.time() * 1000)
            was_completed = self.get_setting("initial_scan_completed") == "1"
            phase_name = "VALIDATING_INDEX" if was_completed else "INITIAL_INDEXING"

            drive_stats: dict[str, dict[str, Any]] = {}
            for d in drives:
                drive_stats[d] = {
                    "drive": d,
                    "status": "PENDING",
                    "indexed_files": 0,
                    "targets_completed": 0,
                    "targets_total": 0,
                }
            for tgt in targets:
                d = extract_drive_label(str(tgt))
                if d not in drive_stats:
                    drive_stats[d] = {
                        "drive": d,
                        "status": "PENDING",
                        "indexed_files": 0,
                        "targets_completed": 0,
                        "targets_total": 0,
                    }
                drive_stats[d]["targets_total"] += 1

            with self._telemetry_lock:
                self.telemetry.phase = phase_name
                self.telemetry.percent = 0.0
                self.telemetry.target_index = 0
                self.telemetry.total_targets = len(targets)
                self.telemetry.current_target = ""
                self.telemetry.current_file = ""
                self.telemetry.target_processed = 0
                self.telemetry.target_total = 0
                self.telemetry.new_files = 0
                self.telemetry.updated_files = 0
                self.telemetry.deleted_files = 0
                self.telemetry.skipped_files = 0
                self.telemetry.drives = list(drive_stats.values())

            existing_map: dict[str, tuple[int, int, int]] = {}
            conn_read = self._connect()
            try:
                for row in conn_read.execute("SELECT id, path, size, mtime_ms FROM files"):
                    existing_map[str(row["path"])] = (int(row["id"]), int(row["size"]), int(row["mtime_ms"]))
            finally:
                conn_read.close()

            pending_ops: list[tuple[str, Any]] = []
            seen_ids: set[int] = set()

            def flush_batch() -> None:
                if not pending_ops:
                    return
                with self._write_lock:
                    conn = self._connect()
                    try:
                        cur = conn.cursor()
                        for op_kind, payload in pending_ops:
                            if op_kind == "touch":
                                cur.execute("UPDATE files SET scan_gen = ? WHERE id = ?", (scan_gen, payload))
                            elif op_kind == "insert":
                                (
                                    path_s, name, name_fold, path_fold, ext, cat, badge, drv,
                                    size, mtime_ms, has_content, snippet, content_text,
                                ) = payload
                                cur.execute(
                                    """
                                    INSERT INTO files(
                                        path, name, name_fold, path_fold, ext, category, badge,
                                        drive, size, mtime_ms, has_content, snippet, scan_gen
                                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                                    ON CONFLICT(path) DO UPDATE SET
                                        name = excluded.name,
                                        name_fold = excluded.name_fold,
                                        path_fold = excluded.path_fold,
                                        ext = excluded.ext,
                                        category = excluded.category,
                                        badge = excluded.badge,
                                        drive = excluded.drive,
                                        size = excluded.size,
                                        mtime_ms = excluded.mtime_ms,
                                        has_content = excluded.has_content,
                                        snippet = excluded.snippet,
                                        scan_gen = excluded.scan_gen
                                    """,
                                    (
                                        path_s, name, name_fold, path_fold, ext, cat, badge, drv,
                                        size, mtime_ms, has_content, snippet, scan_gen,
                                    ),
                                )
                                row_id = cur.execute(
                                    "SELECT id FROM files WHERE path = ?", (path_s,)
                                ).fetchone()["id"]
                                cur.execute("DELETE FROM files_fts WHERE rowid = ?", (row_id,))
                                cur.execute(
                                    "INSERT INTO files_fts(rowid, name, path, content) VALUES (?, ?, ?, ?)",
                                    (row_id, name, path_s, content_text),
                                )
                            elif op_kind == "update":
                                (
                                    row_id, path_s, name, name_fold, path_fold, ext, cat, badge, drv,
                                    size, mtime_ms, has_content, snippet, content_text,
                                ) = payload
                                cur.execute(
                                    """
                                    UPDATE files SET
                                        name = ?, name_fold = ?, path_fold = ?, ext = ?, category = ?,
                                        badge = ?, drive = ?, size = ?, mtime_ms = ?, has_content = ?,
                                        snippet = ?, scan_gen = ?
                                    WHERE id = ?
                                    """,
                                    (
                                        name, name_fold, path_fold, ext, cat, badge, drv,
                                        size, mtime_ms, has_content, snippet, scan_gen, row_id,
                                    ),
                                )
                                cur.execute("DELETE FROM files_fts WHERE rowid = ?", (row_id,))
                                cur.execute(
                                    "INSERT INTO files_fts(rowid, name, path, content) VALUES (?, ?, ?, ?)",
                                    (row_id, name, path_s, content_text),
                                )
                        conn.commit()
                    finally:
                        conn.close()
                pending_ops.clear()

            total_targets = max(1, len(targets))
            for idx, target_dir in enumerate(targets, start=1):
                if self._stop_event.is_set():
                    break
                drv = extract_drive_label(str(target_dir))
                if drv in drive_stats:
                    drive_stats[drv]["status"] = "INDEXING"

                with self._telemetry_lock:
                    self.telemetry.target_index = idx
                    self.telemetry.current_target = str(target_dir)
                    self.telemetry.target_processed = 0
                    self.telemetry.percent = ((idx - 1) / total_targets) * 100.0
                    self.telemetry.drives = [dict(v) for v in drive_stats.values()]

                processed_in_target = 0
                stack = [target_dir]
                while stack and not self._stop_event.is_set():
                    current_dir = stack.pop()
                    try:
                        with os.scandir(current_dir) as it:
                            entries = list(it)
                    except OSError:
                        continue

                    for entry in entries:
                        if self._stop_event.is_set():
                            break
                        try:
                            if entry.is_dir(follow_symlinks=False):
                                low_name = entry.name.lower()
                                if (
                                    low_name in SYSTEM_EXCLUDED_DIRS
                                    or low_name in NOISY_EXCLUDED_DIRS
                                    or low_name.startswith("$")
                                ):
                                    continue
                                stack.append(Path(entry.path))
                                continue
                            if not entry.is_file(follow_symlinks=False):
                                continue
                            st = entry.stat(follow_symlinks=False)
                        except OSError:
                            continue

                        path_s = os.path.normpath(entry.path)
                        size = int(st.st_size)
                        mtime_ms = int(st.st_mtime * 1000)
                        processed_in_target += 1

                        if drv in drive_stats:
                            drive_stats[drv]["indexed_files"] += 1

                        prev = existing_map.get(path_s)
                        if prev is not None and prev[1] == size and prev[2] == mtime_ms:
                            seen_ids.add(prev[0])
                            pending_ops.append(("touch", prev[0]))
                            with self._telemetry_lock:
                                self.telemetry.skipped_files += 1
                                self.telemetry.target_processed = processed_in_target
                                if processed_in_target % 50 == 0:
                                    self.telemetry.current_file = entry.name
                        else:
                            name = entry.name
                            ext = Path(name).suffix.lower().lstrip(".")
                            cat, badge = classify_extension(ext)
                            content_text = ""
                            if ext in TEXT_EXTRACT_EXTS or ext in OFFICE_EXTRACT_EXTS:
                                content_text = extract_searchable_text(Path(path_s), ext, size)
                            has_content = 1 if content_text else 0
                            snippet = content_text[:220] if content_text else ""
                            name_fold = fold_text(name)
                            path_fold = fold_text(path_s)

                            if prev is None:
                                pending_ops.append(
                                    (
                                        "insert",
                                        (
                                            path_s, name, name_fold, path_fold, ext, cat, badge, drv,
                                            size, mtime_ms, has_content, snippet, content_text,
                                        ),
                                    )
                                )
                                with self._telemetry_lock:
                                    self.telemetry.new_files += 1
                                    self.telemetry.target_processed = processed_in_target
                                    self.telemetry.current_file = name
                            else:
                                seen_ids.add(prev[0])
                                pending_ops.append(
                                    (
                                        "update",
                                        (
                                            prev[0], path_s, name, name_fold, path_fold, ext, cat, badge, drv,
                                            size, mtime_ms, has_content, snippet, content_text,
                                        ),
                                    )
                                )
                                with self._telemetry_lock:
                                    self.telemetry.updated_files += 1
                                    self.telemetry.target_processed = processed_in_target
                                    self.telemetry.current_file = name

                        if len(pending_ops) >= BATCH_COMMIT_SIZE:
                            flush_batch()

                flush_batch()
                if drv in drive_stats:
                    drive_stats[drv]["targets_completed"] += 1
                    if drive_stats[drv]["targets_completed"] >= drive_stats[drv]["targets_total"]:
                        drive_stats[drv]["status"] = "OK"

            # Prune deleted files only if scan completed uninterrupted
            deleted_count = 0
            if not self._stop_event.is_set():
                with self._write_lock:
                    conn = self._connect()
                    try:
                        stale_rows = conn.execute(
                            "SELECT id FROM files WHERE scan_gen < ?", (scan_gen,)
                        ).fetchall()
                        if stale_rows:
                            stale_ids = [(int(r["id"]),) for r in stale_rows]
                            conn.executemany("DELETE FROM files_fts WHERE rowid = ?", stale_ids)
                            conn.executemany("DELETE FROM files WHERE id = ?", stale_ids)
                            deleted_count = len(stale_ids)
                        conn.commit()
                    finally:
                        conn.close()

                now_ms = int(time.time() * 1000)
                cp_label = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
                self.set_setting("initial_scan_completed", "1")
                self.set_setting("last_checkpoint", cp_label)
                self.set_setting("last_sync_ms", str(now_ms))

                for d_info in drive_stats.values():
                    if d_info["status"] != "OK":
                        d_info["status"] = "OK"

                with self._telemetry_lock:
                    self.telemetry.phase = "READY"
                    self.telemetry.percent = 100.0
                    self.telemetry.current_target = ""
                    self.telemetry.current_file = ""
                    self.telemetry.deleted_files = deleted_count
                    self.telemetry.last_checkpoint = cp_label
                    self.telemetry.last_sync_ms = now_ms
                    self.telemetry.initial_scan_completed = True
                    self.telemetry.drives = [dict(v) for v in drive_stats.values()]
        except Exception as exc:
            LOG.exception("Error en ciclo de indexación: %s", exc)
            with self._telemetry_lock:
                self.telemetry.phase = "READY"
                self.telemetry.current_file = ""
        finally:
            self._sync_running.clear()

    def get_extensions(self, prefix: str = "", limit: int = 18) -> list[dict[str, Any]]:
        clean_prefix = fold_text(prefix).lstrip("*.")
        safe_limit = max(1, min(100, int(limit or 18)))
        conn = self._connect()
        try:
            if clean_prefix:
                rows = conn.execute(
                    """
                    SELECT ext AS extension, COUNT(*) AS count
                    FROM files
                    WHERE ext != '' AND ext LIKE ?
                    GROUP BY ext
                    ORDER BY count DESC, ext ASC
                    LIMIT ?
                    """,
                    (f"{clean_prefix}%", safe_limit),
                ).fetchall()
            else:
                rows = conn.execute(
                    """
                    SELECT ext AS extension, COUNT(*) AS count
                    FROM files
                    WHERE ext != ''
                    GROUP BY ext
                    ORDER BY count DESC, ext ASC
                    LIMIT ?
                    """,
                    (safe_limit,),
                ).fetchall()
            return [{"extension": str(r["extension"]), "count": int(r["count"])} for r in rows]
        finally:
            conn.close()

    def search(
        self,
        query: str,
        mode: str = "all",
        limit: int = 50,
        expand_all: bool = False,
    ) -> list[dict[str, Any]]:
        q_raw = str(query or "").strip()
        if not q_raw:
            return []

        norm_mode = (mode or "all").strip().lower()
        max_rows = 10_000 if expand_all else max(1, min(2000, int(limit or 50)))

        # Support OR operator (|) by splitting into branches and merging best scores
        branches = [b.strip() for b in q_raw.split("|") if b.strip()]
        if not branches:
            return []

        merged: dict[int, tuple[float, dict[str, Any]]] = {}
        conn = self._connect()
        try:
            for branch in branches:
                branch_hits = self._search_branch(conn, branch, norm_mode, max_rows)
                for row_id, score, item in branch_hits:
                    existing = merged.get(row_id)
                    if existing is None or score > existing[0]:
                        merged[row_id] = (score, item)
        finally:
            conn.close()

        ranked = sorted(merged.values(), key=lambda pair: (-pair[0], pair[1]["name"].lower()))
        out: list[dict[str, Any]] = []
        for score, item in ranked[:max_rows]:
            item["score"] = f"{score:.1f}"
            out.append(item)
        return out

    def _search_branch(
        self,
        conn: sqlite3.Connection,
        branch: str,
        mode: str,
        max_rows: int,
    ) -> list[tuple[int, float, dict[str, Any]]]:
        parsed = _parse_query_branch(branch, mode == "extension")
        extensions = parsed["extensions"]
        positive_terms = parsed["positive"]
        negated_terms = parsed["negated"]
        path_filters = parsed["path_filters"]
        wildcard_terms = parsed["wildcards"]

        where_clauses: list[str] = []
        params: list[Any] = []

        if mode == "documents":
            where_clauses.append(
                f"f.ext IN ({','.join('?' for _ in DOCUMENT_EXTS)})"
            )
            params.extend(sorted(DOCUMENT_EXTS))
        elif mode == "media":
            where_clauses.append("f.category = 'media'")
        elif mode == "design":
            where_clauses.append("f.category = 'design'")
        elif mode == "content":
            where_clauses.append("f.has_content = 1")

        if extensions:
            ext_conds = []
            for ext in extensions:
                if len(ext) <= 2 and mode == "extension":
                    ext_conds.append("f.ext LIKE ?")
                    params.append(f"{ext}%")
                else:
                    ext_conds.append("f.ext = ?")
                    params.append(ext)
            where_clauses.append("(" + " OR ".join(ext_conds) + ")")

        for pf in path_filters:
            where_clauses.append("f.path_fold LIKE ?")
            params.append(f"%{pf}%")

        for neg in negated_terms:
            where_clauses.append("f.name_fold NOT LIKE ? AND f.path_fold NOT LIKE ?")
            params.extend([f"%{neg}%", f"%{neg}%"])

        for wc in wildcard_terms:
            like_pat = wc.replace("*", "%").replace("?", "_")
            where_clauses.append("(f.name_fold LIKE ? OR f.path_fold LIKE ?)")
            params.extend([f"%{like_pat}%", f"%{like_pat}%"])

        candidates: dict[int, tuple[float, dict[str, Any]]] = {}
        fetch_cap = min(5000, max(max_rows * 4, 250))

        # 1) Fast filename / path match (always executed unless strictly content-only)
        if mode != "content":
            fn_where = list(where_clauses)
            fn_params = list(params)
            for term in positive_terms:
                fn_where.append("(f.name_fold LIKE ? OR f.path_fold LIKE ?)")
                fn_params.extend([f"%{term}%", f"%{term}%"])

            sql_fn = "SELECT f.* FROM files f"
            if fn_where:
                sql_fn += " WHERE " + " AND ".join(fn_where)
            sql_fn += " ORDER BY LENGTH(f.name) ASC, f.mtime_ms DESC LIMIT ?"
            fn_params.append(fetch_cap)

            for row in conn.execute(sql_fn, fn_params):
                row_id = int(row["id"])
                score = _score_file_row(row, positive_terms, extensions, from_fts=False)
                candidates[row_id] = (score, _row_to_result(row))

        # 2) Full-text FTS5 match (name + path + extracted document content)
        if positive_terms and mode != "filename":
            fts_expr = _build_fts_query(positive_terms)
            if fts_expr:
                fts_where = list(where_clauses)
                fts_params: list[Any] = [fts_expr] + list(params)
                sql_fts = (
                    "SELECT f.*, bm25(files_fts, 10.0, 3.0, 1.0) AS rank_val, "
                    "snippet(files_fts, 2, '', '', ' … ', 18) AS fts_snip "
                    "FROM files_fts "
                    "JOIN files f ON f.id = files_fts.rowid "
                    "WHERE files_fts MATCH ?"
                )
                if fts_where:
                    sql_fts += " AND " + " AND ".join(fts_where)
                sql_fts += " ORDER BY rank_val ASC LIMIT ?"
                fts_params.append(fetch_cap)
                try:
                    for row in conn.execute(sql_fts, fts_params):
                        row_id = int(row["id"])
                        bm25_raw = float(row["rank_val"] or 0.0)
                        score = _score_file_row(
                            row, positive_terms, extensions, from_fts=True, bm25_val=bm25_raw
                        )
                        snip = str(row["fts_snip"] or row["snippet"] or "").strip()
                        item = _row_to_result(row, override_snippet=snip)
                        prev = candidates.get(row_id)
                        if prev is None or score > prev[0]:
                            candidates[row_id] = (score, item)
                except sqlite3.OperationalError:
                    pass

        # Post-filter negated terms against snippet & wildcards strictly
        results: list[tuple[int, float, dict[str, Any]]] = []
        for row_id, (score, item) in candidates.items():
            hay = fold_text(f"{item['name']} {item['path']} {item['snippet']}")
            if any(neg in hay for neg in negated_terms):
                continue
            if wildcard_terms:
                nf = fold_text(item["name"])
                pf = fold_text(item["path"])
                if not all(
                    fnmatch.fnmatchcase(nf, f"*{wc}*") or fnmatch.fnmatchcase(pf, f"*{wc}*")
                    for wc in wildcard_terms
                ):
                    continue
            results.append((row_id, score, item))

        return results


def _parse_query_branch(branch: str, extension_mode: bool) -> dict[str, list[str]]:
    extensions: list[str] = []
    positive: list[str] = []
    negated: list[str] = []
    path_filters: list[str] = []
    wildcards: list[str] = []

    try:
        tokens = shlex.split(branch, posix=False)
    except ValueError:
        tokens = branch.split()

    for idx, raw_tok in enumerate(tokens):
        tok = raw_tok.strip().strip(",")
        if not tok:
            continue

        is_quoted = (tok.startswith('"') and tok.endswith('"') and len(tok) >= 2)
        if is_quoted:
            inner = fold_text(tok[1:-1]).strip()
            if inner:
                positive.append(inner)
            continue

        low = fold_text(tok)

        # Negation (!term or -term)
        if (low.startswith("!") or low.startswith("-")) and len(low) > 1:
            neg = low[1:].strip()
            if neg:
                negated.append(neg)
            continue

        # Explicit extension syntax
        ext_match = re.match(r"^(?:ext:|extension:|tipo:|\*\.|\.)([a-z0-9]{1,12})$", low)
        if ext_match:
            ext = ext_match.group(1)
            if ext not in extensions:
                extensions.append(ext)
            continue

        # Extension mode: first bare alphanumeric token is treated as extension
        if extension_mode and idx == 0 and re.match(r"^[a-z0-9]{1,12}$", low):
            if low not in extensions:
                extensions.append(low)
            continue

        # Path filter (contains backslash, drive colon, or slash)
        if "\\" in low or (len(low) >= 2 and low[1] == ":"):
            path_filters.append(low.replace("/", "\\"))
            continue

        # Wildcard token
        if "*" in low or "?" in low:
            wildcards.append(low)
            continue

        positive.append(low)

    return {
        "extensions": extensions,
        "positive": positive,
        "negated": negated,
        "path_filters": path_filters,
        "wildcards": wildcards,
    }


def _build_fts_query(terms: list[str]) -> str:
    parts: list[str] = []
    for t in terms:
        cleaned = re.sub(r"[^\w\s]", " ", t).strip()
        if not cleaned:
            continue
        words = [w for w in cleaned.split() if w]
        if len(words) > 1:
            parts.append('"' + " ".join(words) + '"')
        else:
            w = words[0]
            if len(w) >= 2:
                parts.append(f'"{w}"*')
            else:
                parts.append(f'"{w}"')
    return " AND ".join(parts)


def _score_file_row(
    row: sqlite3.Row,
    positive_terms: list[str],
    extensions: list[str],
    from_fts: bool = False,
    bm25_val: float = 0.0,
) -> float:
    score = 350.0
    name_fold = str(row["name_fold"] or "")
    path_fold = str(row["path_fold"] or "")
    stem_fold = Path(name_fold).stem

    if extensions and str(row["ext"] or "") in extensions:
        score += 120.0

    for term in positive_terms:
        if name_fold == term or stem_fold == term:
            score += 550.0
        elif name_fold.startswith(term):
            score += 360.0
        elif term in name_fold:
            score += 240.0
        elif term in path_fold:
            score += 130.0
        elif from_fts:
            score += 95.0

    if from_fts:
        # SQLite bm25 returns negative values (more negative = better)
        score += min(180.0, max(0.0, abs(bm25_val) * 18.0))

    if int(row["has_content"] or 0):
        score += 20.0
    return score


def _row_to_result(row: sqlite3.Row, override_snippet: str | None = None) -> dict[str, Any]:
    snippet = override_snippet if override_snippet is not None else str(row["snippet"] or "")
    if not snippet:
        snippet = f"Indexado en {row['drive']} ({row['badge']})"
    return {
        "name": str(row["name"]),
        "path": str(row["path"]),
        "badge": str(row["badge"]),
        "size": format_file_size(int(row["size"] or 0)),
        "score": "0.0",
        "snippet": snippet[:280],
    }
