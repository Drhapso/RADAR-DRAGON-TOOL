from __future__ import annotations

import argparse
import ipaddress
import json
import logging
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import webbrowser
from pathlib import Path
from typing import Any

from flask import Flask, Response, jsonify, request, send_file, stream_with_context
from waitress.server import create_server

from radar_indexer import RadarIndexer, extract_searchable_text
from radar_preview import (
    build_file_preview,
    convert_office_to_pdf,
    find_soffice_executable,
    mime_for_path,
    read_zip_asset,
)

APP_VERSION = "2.0.0-PY-SERVICE"
ROOT_DIR = Path(__file__).resolve().parent
UI_DIR = ROOT_DIR / "ui"
DEFAULT_CONFIG_PATH = ROOT_DIR / "config" / "site_config.json"

LOG = logging.getLogger("dragon_radar.server")


def setup_logging() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )


def load_site_config(config_path: Path) -> dict[str, Any]:
    defaults: dict[str, Any] = {
        "port": 8092,
        "lan_mode": "local_agent",
        "authorized_subnet": "172.23.174.0/24",
        "primary_host": "172.23.174.103",
        "proxy_host": "172.23.174.11",
        "shared_paths": [],
        "libreoffice_path": "",
        "sync_interval_minutes": 15,
    }
    if config_path.is_file():
        try:
            loaded = json.loads(config_path.read_text(encoding="utf-8-sig"))
            if isinstance(loaded, dict):
                defaults.update(loaded)
        except Exception as exc:
            LOG.warning("No se pudo leer %s: %s", config_path, exc)
    return defaults


def get_local_ipv4_addresses() -> set[str]:
    ips = {"127.0.0.1", "::1"}
    try:
        hostname = socket.gethostname()
        for info in socket.getaddrinfo(hostname, None):
            addr = info[4][0]
            if addr:
                ips.add(addr.split("%")[0])
    except OSError:
        pass
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("172.23.174.1", 80))
            ips.add(s.getsockname()[0])
    except OSError:
        pass
    return ips


def normalize_peer_ip(raw_ip: str | None) -> str:
    ip = (raw_ip or "").strip()
    if ip.startswith("::ffff:"):
        ip = ip[7:]
    if "%" in ip:
        ip = ip.split("%", 1)[0]
    return ip or "127.0.0.1"


def normalize_explorer_path(raw_path: str) -> str:
    s = str(raw_path or "").strip().strip('"')
    if s.lower().startswith("\\\\?\\unc\\"):
        s = "\\\\" + s[8:]
    elif s.startswith("\\\\?\\"):
        s = s[4:]
    s = s.replace("/", "\\")
    is_unc = s.startswith("\\\\")
    prefix = "\\\\" if is_unc else ""
    body = s[2:] if is_unc else s
    body = re.sub(r"\\{2,}", r"\\", body)
    result = prefix + body
    if len(result) == 2 and result[1] == ":":
        result += "\\"
    return result


def is_running_in_session_zero() -> bool:
    """Detecta de forma 100% segura (solo variables de entorno, sin APIs Win32) si corre como servicio SYSTEM."""
    if os.name != "nt":
        return False
    session_name = (os.environ.get("SESSIONNAME") or "").strip().lower()
    user_name = (os.environ.get("USERNAME") or "").strip().upper()
    if session_name == "services":
        return True
    if user_name == "SYSTEM" or user_name.endswith("$"):
        return True
    if not session_name and (os.environ.get("USERDOMAIN") or "").strip().upper() == "NT AUTHORITY":
        return True
    return False


def resolve_explorer_exe() -> str:
    win_dir = os.environ.get("WINDIR") or os.environ.get("SystemRoot") or r"C:\Windows"
    cand = Path(win_dir) / "explorer.exe"
    if cand.is_file():
        return str(cand)
    return "explorer.exe"


def open_path_in_explorer(raw_file: str) -> dict[str, Any]:
    normalized = normalize_explorer_path(raw_file)
    p = Path(normalized)
    if p.is_file():
        target = normalized
        directory = str(p.parent)
        select_file = True
        mode = "select_file"
    elif p.is_dir():
        target = normalized
        directory = normalized
        select_file = False
        mode = "open_dir"
    elif p.parent.is_dir():
        target = normalize_explorer_path(str(p.parent))
        directory = target
        select_file = False
        mode = "open_parent"
    else:
        return {
            "opened": False,
            "error": "Ruta no encontrada en el equipo",
            "target": normalized,
        }

    if is_running_in_session_zero():
        return {
            "opened": False,
            "session0": True,
            "mode": mode,
            "target": target,
            "directory": directory,
            "error": "Servicio activo en segundo plano (SYSTEM / Sesión 0). Ruta lista para pegar en el Explorador.",
        }

    if os.name != "nt":
        return {"opened": False, "error": "Disponible solo en Windows", "target": target}

    explorer_exe = resolve_explorer_exe()
    clean_target = target.replace('"', "")
    try:
        if select_file:
            # explorer.exe requiere /select,"ruta" sin espacio intermedio
            cmd = f'"{explorer_exe}" /select,"{clean_target}"'
            subprocess.Popen(cmd, shell=False)
        else:
            subprocess.Popen([explorer_exe, clean_target], shell=False)
        return {"opened": True, "mode": mode, "target": target, "directory": directory}
    except OSError as exc:
        return {"opened": False, "error": str(exc), "target": target, "directory": directory}


def parse_http_range(range_header: str | None, total_size: int) -> tuple[int, int] | None:
    if not range_header or total_size <= 0:
        return None
    header = range_header.strip()
    if not header.lower().startswith("bytes="):
        return None
    spec = header[6:].split(",", 1)[0].strip()
    if "-" not in spec:
        return None
    start_s, end_s = spec.split("-", 1)
    try:
        if not start_s:
            suffix = int(end_s)
            if suffix <= 0:
                return None
            start = max(0, total_size - suffix)
            return start, total_size - 1
        start = int(start_s)
        end = int(end_s) if end_s else total_size - 1
        if start < 0 or start >= total_size or end < start:
            return None
        return start, min(end, total_size - 1)
    except ValueError:
        return None


def create_app(
    config: dict[str, Any],
    indexer: RadarIndexer,
    *,
    listen_port: int,
    bind_lan: bool,
    cache_dir: Path,
    shutdown_callback: Any | None = None,
) -> Flask:
    app = Flask(__name__)

    machine_name = indexer.machine_name
    lan_mode = str(config.get("lan_mode") or "local_agent").strip().lower()
    if lan_mode not in {"local_agent", "shared_server"}:
        lan_mode = "local_agent"
    shared_server_active = lan_mode == "shared_server"
    authorized_subnet_str = str(config.get("authorized_subnet") or "172.23.174.0/24").strip()
    try:
        authorized_net = ipaddress.ip_network(authorized_subnet_str, strict=False)
    except ValueError:
        authorized_net = None

    canonical_shared_roots: list[Path] = []
    for raw_root in config.get("shared_paths") or []:
        try:
            rp = Path(str(raw_root)).resolve()
            if rp.is_dir():
                canonical_shared_roots.append(rp)
        except OSError:
            continue

    configured_lo = str(config.get("libreoffice_path") or "").strip()
    sync_interval_minutes = max(5, int(config.get("sync_interval_minutes") or 15))
    local_ips = get_local_ipv4_addresses()

    def get_peer_ip() -> str:
        return normalize_peer_ip(request.remote_addr)

    def is_client_local() -> bool:
        peer = get_peer_ip()
        try:
            ip_obj = ipaddress.ip_address(peer)
            if ip_obj.is_loopback:
                return True
        except ValueError:
            pass
        return peer in local_ips

    def is_peer_authorized() -> bool:
        if is_client_local():
            return True
        if authorized_net is None:
            return True
        peer = get_peer_ip()
        try:
            return ipaddress.ip_address(peer) in authorized_net
        except ValueError:
            return False

    def resolve_allowed_file(raw_file: str | None) -> tuple[Path | None, str | None, int]:
        if not raw_file:
            return None, "Parámetro 'file' requerido", 400
        if not shared_server_active and not is_client_local():
            return None, "Acceso a archivos locales permitido únicamente desde este equipo", 403
        normalized = normalize_explorer_path(raw_file)
        try:
            resolved = Path(normalized).resolve()
        except OSError:
            return None, "Ruta inválida", 400
        if not resolved.is_file():
            return None, "Archivo no encontrado", 404
        if shared_server_active:
            if not canonical_shared_roots:
                return None, "No hay carpetas compartidas configuradas", 403
            allowed = False
            for root in canonical_shared_roots:
                try:
                    resolved.relative_to(root)
                    allowed = True
                    break
                except ValueError:
                    continue
            if not allowed:
                return None, "Archivo fuera de las carpetas compartidas autorizadas", 403
        return resolved, None, 200

    def build_status_payload() -> dict[str, Any]:
        total_files, with_content = indexer.get_counts()
        telem = indexer.get_telemetry_snapshot()
        soffice = find_soffice_executable(configured_lo)
        if indexer.get_setting("libreoffice_first_run_checked") is None:
            indexer.set_setting("libreoffice_first_run_checked", "1" if soffice else "0")

        return {
            "status": "OPERATIONAL",
            "mft_armed": True,
            "engine": "python-service",
            "machine_name": machine_name,
            "storage_mode": indexer.storage_mode,
            "data_dir": str(indexer.db_path.parent),
            "redb_records": total_files,
            "tracked_files": total_files,
            "vector_count": with_content,
            "running_in_background": indexer.is_sync_running(),
            "phase": telem["phase"],
            "percent": telem["percent"],
            "target_index": telem["target_index"],
            "total_targets": telem["total_targets"],
            "current_target": telem["current_target"],
            "current_file": telem["current_file"],
            "target_processed": telem["target_processed"],
            "target_total": telem["target_total"],
            "new_files": telem["new_files"],
            "updated_files": telem["updated_files"],
            "moved_files": telem["moved_files"],
            "deleted_files": telem["deleted_files"],
            "skipped_files": telem["skipped_files"],
            "last_checkpoint": telem["last_checkpoint"],
            "last_sync_ms": telem["last_sync_ms"],
            "initial_scan_completed": telem["initial_scan_completed"],
            "drives": telem["drives"],
            "sync_interval_minutes": sync_interval_minutes,
            "office_pdf_available": soffice is not None,
            "libreoffice_installed": soffice is not None,
            "libreoffice_first_run_checked": True,
            "libreoffice_path": str(soffice) if soffice else None,
            "lan_enabled": True,
            "bind_lan": bind_lan,
            "lan_mode": lan_mode,
            "authorized_subnet": authorized_subnet_str,
            "shared_server_active": shared_server_active,
            "shared_paths_count": len(canonical_shared_roots),
            "client_is_local": is_client_local(),
            "port": listen_port,
            "portable_edition": False,
            "version": APP_VERSION,
        }

    @app.before_request
    def _enforce_cidr() -> Any:
        if request.method == "OPTIONS":
            resp = Response("", status=204)
            return resp
        if not is_peer_authorized():
            return jsonify({"error": "Acceso denegado: dirección IP fuera de la subred autorizada"}), 403
        return None

    @app.after_request
    def _apply_headers(resp: Response) -> Response:
        resp.headers["Cache-Control"] = "no-store, private, max-age=0"
        resp.headers["Pragma"] = "no-cache"
        resp.headers["X-Content-Type-Options"] = "nosniff"
        resp.headers["Referrer-Policy"] = "no-referrer"
        resp.headers["X-Frame-Options"] = "SAMEORIGIN"
        resp.headers["Access-Control-Allow-Origin"] = "*"
        resp.headers["Access-Control-Allow-Methods"] = "GET, POST, OPTIONS"
        resp.headers["Access-Control-Allow-Headers"] = "*"
        resp.headers["Access-Control-Allow-Private-Network"] = "true"
        if resp.mimetype == "text/html":
            resp.headers["Content-Security-Policy"] = (
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
                "img-src 'self' data: blob: http://127.0.0.1:*; "
                "media-src 'self' blob: http://127.0.0.1:*; "
                "frame-src 'self' blob: http://127.0.0.1:*; "
                "connect-src 'self' http://127.0.0.1:*; object-src 'none'; base-uri 'none'; frame-ancestors 'self'"
            )
        return resp

    @app.route("/")
    @app.route("/index.html")
    def serve_index() -> Any:
        return send_file(UI_DIR / "index.html", mimetype="text/html; charset=utf-8")

    @app.route("/styles.css")
    def serve_styles() -> Any:
        return send_file(UI_DIR / "styles.css", mimetype="text/css; charset=utf-8")

    @app.route("/radar.js")
    def serve_js() -> Any:
        return send_file(UI_DIR / "radar.js", mimetype="application/javascript; charset=utf-8")

    @app.route("/health")
    def health() -> Any:
        return jsonify(
            {
                "app": "dragon-radar",
                "status": "ok",
                "db_ready": True,
                "engine": "python-service",
                "version": APP_VERSION,
                "port": listen_port,
                "lan_mode": lan_mode,
                "shared_server_active": shared_server_active,
                "machine_name": machine_name,
            }
        )

    @app.route("/api/server-info")
    def server_info() -> Any:
        return jsonify(
            {
                "app": "dragon-radar",
                "status": "ok",
                "db_ready": True,
                "engine": "python-service",
                "version": APP_VERSION,
                "port": listen_port,
                "bind_lan": bind_lan,
                "lan_enabled": True,
                "portable_edition": False,
                "lan_mode": lan_mode,
                "authorized_subnet": authorized_subnet_str,
                "shared_server_active": shared_server_active,
                "shared_paths_count": len(canonical_shared_roots),
                "client_is_local": is_client_local(),
                "machine_name": machine_name,
            }
        )

    @app.route("/api/status")
    def api_status() -> Any:
        return jsonify(build_status_payload())

    @app.route("/api/index/stream")
    def api_index_stream() -> Any:
        def generate():
            last_sent = 0.0
            for _ in range(180):
                payload = build_status_payload()
                yield f"data: {json.dumps(payload, ensure_ascii=False)}\n\n"
                delay = 0.7 if payload.get("phase") != "READY" else 2.5
                last_sent = time.monotonic()
                while time.monotonic() - last_sent < delay:
                    time.sleep(0.25)

        return Response(
            stream_with_context(generate()),
            mimetype="text/event-stream",
            headers={"X-Accel-Buffering": "no"},
        )

    @app.route("/api/index/validate", methods=["GET", "POST"])
    def api_index_validate() -> Any:
        telem = indexer.get_telemetry_snapshot()
        total_files, with_content = indexer.get_counts()
        force_sync = request.args.get("force") in {"1", "true"}
        incomplete = (not telem["initial_scan_completed"]) or (total_files == 0)
        resumed = False
        if (incomplete or force_sync) and not indexer.is_sync_running():
            resumed = indexer.start_background_sync()
            telem = indexer.get_telemetry_snapshot()

        pending_targets = max(0, int(telem["total_targets"]) - int(telem["target_index"]))
        complete = bool(telem["initial_scan_completed"] and not indexer.is_sync_running() and telem["phase"] == "READY")
        return jsonify(
            {
                "app": "dragon-radar",
                "machine_name": machine_name,
                "complete": complete,
                "phase": telem["phase"],
                "pending_targets": pending_targets,
                "resumed_just_now": resumed,
                "redb_records": total_files,
                "vector_count": with_content,
                "last_checkpoint": telem["last_checkpoint"],
            }
        )

    @app.route("/api/search")
    def api_search() -> Any:
        if not shared_server_active and not is_client_local():
            return jsonify([])
        q = request.args.get("q", "")
        mode = request.args.get("mode", "all")
        expand_all = request.args.get("all", "").lower() in {"1", "true", "yes"}
        try:
            limit = int(request.args.get("limit", "50"))
        except ValueError:
            limit = 50
        results = indexer.search(q, mode=mode, limit=limit, expand_all=expand_all)
        return jsonify(results)

    @app.route("/api/extensions")
    def api_extensions() -> Any:
        if not shared_server_active and not is_client_local():
            return jsonify({"extensions": []})
        q = request.args.get("q", "")
        try:
            limit = int(request.args.get("limit", "18"))
        except ValueError:
            limit = 18
        return jsonify({"extensions": indexer.get_extensions(prefix=q, limit=limit)})

    @app.route("/api/preview")
    def api_preview() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"success": False, "error": err}), status
        data = build_file_preview(resolved, cache_dir=cache_dir, configured_soffice=configured_lo)
        return jsonify(data)

    @app.route("/api/extract")
    def api_extract() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"success": False, "error": err}), status
        ext = resolved.suffix.lower().lstrip(".")
        size = resolved.stat().st_size
        text = extract_searchable_text(resolved, ext, size)
        return jsonify({"success": True, "file": str(resolved), "content": text})

    @app.route("/api/raw")
    def api_raw() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"error": err}), status
        mime = mime_for_path(resolved)
        file_size = resolved.stat().st_size
        range_hdr = request.headers.get("Range")
        if range_hdr:
            parsed = parse_http_range(range_hdr, file_size)
            if parsed is None:
                return Response("", status=416, headers={"Content-Range": f"bytes */{file_size}"})
            start, end = parsed
            length = end - start + 1
            with resolved.open("rb") as fh:
                fh.seek(start)
                chunk = fh.read(length)
            return Response(
                chunk,
                status=206,
                mimetype=mime,
                headers={
                    "Accept-Ranges": "bytes",
                    "Content-Range": f"bytes {start}-{end}/{file_size}",
                    "Content-Length": str(length),
                },
            )
        resp = send_file(resolved, mimetype=mime, conditional=False)
        resp.headers["Accept-Ranges"] = "bytes"
        return resp

    @app.route("/api/download")
    def api_download() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"error": err}), status
        return send_file(resolved, as_attachment=True, download_name=resolved.name)

    @app.route("/api/office-pdf")
    def api_office_pdf() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"error": err}), status
        pdf_path = convert_office_to_pdf(resolved, cache_dir=cache_dir, configured_soffice=configured_lo)
        if not pdf_path:
            return jsonify({"error": "No se pudo convertir el documento Office a PDF"}), 503
        return send_file(pdf_path, mimetype="application/pdf")

    @app.route("/api/zip-asset")
    def api_zip_asset() -> Any:
        resolved, err, status = resolve_allowed_file(request.args.get("file"))
        if err or not resolved:
            return jsonify({"error": err}), status
        entry = request.args.get("entry", "")
        asset = read_zip_asset(resolved, entry)
        if not asset:
            return jsonify({"error": "Recurso no encontrado dentro del documento"}), 404
        data, mime = asset
        return Response(data, mimetype=mime)

    @app.route("/api/open")
    def api_open() -> Any:
        if not is_client_local():
            return (
                jsonify(
                    {
                        "opened": False,
                        "error": "Abrir ubicación local solo está permitido en el equipo donde corre el radar.",
                    }
                ),
                403,
            )
        raw_file = request.args.get("file", "")
        if not raw_file:
            return jsonify({"opened": False, "error": "Parámetro 'file' requerido"}), 400
        result = open_path_in_explorer(raw_file)
        return jsonify(result), (200 if result.get("opened") or result.get("session0") else 400)

    @app.route("/api/agent/launcher")
    def api_agent_launcher() -> Any:
        bat_content = (
            "@echo off\r\n"
            "setlocal\r\n"
            "title Radar del Dragon - Servicio Local Python\r\n"
            "echo Para ejecutar Radar del Dragon localmente sin ejecutables .exe,\r\n"
            f"echo ejecute 'servidor local.bat' en la carpeta del proyecto (puerto {listen_port}).\r\n"
            "pause\r\n"
        )
        return Response(
            bat_content.encode("utf-8"),
            mimetype="application/octet-stream",
            headers={"Content-Disposition": 'attachment; filename="Activar_Radar_Local.bat"'},
        )

    @app.route("/api/shutdown", methods=["POST"])
    def api_shutdown() -> Any:
        if not is_client_local():
            return jsonify({"error": "Cierre permitido únicamente desde loopback local"}), 403
        if shutdown_callback is not None:
            threading.Thread(target=shutdown_callback, daemon=True).start()
        return jsonify({"status": "shutting_down", "app": "dragon-radar"})

    return app


def start_periodic_sync_loop(
    indexer: RadarIndexer,
    interval_minutes: int,
    stop_event: threading.Event,
) -> threading.Thread:
    def _loop() -> None:
        # Inicia escaneo inicial / validacion al arrancar
        indexer.start_background_sync()
        interval_secs = max(300, int(interval_minutes) * 60)
        while not stop_event.wait(interval_secs):
            if not indexer.is_sync_running():
                indexer.start_background_sync()

    t = threading.Thread(target=_loop, name="dragon-radar-scheduler", daemon=True)
    t.start()
    return t


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Dragon Radar — Servicio Python/Waitress (sin ejecutables .exe)")
    parser.add_argument("--host", default="", help="IP de escucha (por defecto 0.0.0.0 o 127.0.0.1 con --local)")
    parser.add_argument("--port", type=int, default=0, help="Puerto TCP HTTP (por defecto config/site_config.json o 8092)")
    parser.add_argument("--config", default=str(DEFAULT_CONFIG_PATH), help="Ruta a site_config.json")
    parser.add_argument("--lan-mode", choices=["", "local_agent", "shared_server"], default="", help="Sobrescribir modo LAN")
    parser.add_argument("--local", action="store_true", help="Escuchar unicamente en 127.0.0.1")
    parser.add_argument("--no-browser", action="store_true", help="No abrir navegador al iniciar")
    parser.add_argument("--open-browser", action="store_true", help="Abrir navegador al iniciar")
    parser.add_argument("--strict-port", action="store_true", help="Fallar si el puerto ya esta ocupado")
    parser.add_argument("--sync-only", action="store_true", help="Ejecutar indexacion incremental y salir")
    parser.add_argument("--db-path", default="", help="Ruta opcional para base SQLite FTS5")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    setup_logging()
    args = parse_args(argv)

    config_path = Path(args.config)
    config = load_site_config(config_path)
    if args.lan_mode:
        config["lan_mode"] = args.lan_mode

    port = args.port if args.port > 0 else int(config.get("port") or 8092)
    bind_lan = not args.local
    host = "127.0.0.1" if args.local else (args.host.strip() or "0.0.0.0")

    machine_name = os.environ.get("COMPUTERNAME") or socket.gethostname() or "LOCAL"
    lan_mode = str(config.get("lan_mode") or "local_agent").strip().lower()
    shared_roots: list[Path] = []
    if lan_mode == "shared_server":
        for p_str in config.get("shared_paths") or []:
            p = Path(str(p_str))
            if p.is_dir():
                shared_roots.append(p)

    if args.db_path:
        db_path = Path(args.db_path)
    else:
        sub_name = "SHARED_SERVER" if lan_mode == "shared_server" else machine_name
        db_path = ROOT_DIR / ".dragon-data" / sub_name / "radar_service.sqlite3"

    cache_dir = ROOT_DIR / ".dragon-data" / "office-pdf-cache"
    storage_mode = "SHARED_SERVER" if lan_mode == "shared_server" else "LOCAL_SERVICE"

    indexer = RadarIndexer(
        db_path=db_path,
        machine_name=machine_name,
        shared_roots=shared_roots if lan_mode == "shared_server" else None,
        storage_mode=storage_mode,
    )

    if args.sync_only:
        LOG.info("Ejecutando indexación incremental única en %s...", machine_name)
        snap = indexer.sync_blocking()
        total, with_content = indexer.get_counts()
        LOG.info(
            "Indexación finalizada: archivos=%d, contenido=%d, nuevos=%d, actualizados=%d, eliminados=%d",
            total,
            with_content,
            snap["new_files"],
            snap["updated_files"],
            snap["deleted_files"],
        )
        return 0

    stop_event = threading.Event()
    server_holder: dict[str, Any] = {}

    def graceful_shutdown() -> None:
        LOG.info("Recibida solicitud de apagado limpio de Dragon Radar...")
        stop_event.set()
        indexer.stop()
        srv = server_holder.get("server")
        if srv is not None:
            try:
                srv.close()
            except Exception:
                pass
        time.sleep(0.3)
        os._exit(0)

    def _handle_sig(_signum: int, _frame: Any) -> None:
        graceful_shutdown()

    try:
        signal.signal(signal.SIGTERM, _handle_sig)
        signal.signal(signal.SIGINT, _handle_sig)
    except Exception:
        pass

    app = create_app(
        config=config,
        indexer=indexer,
        listen_port=port,
        bind_lan=bind_lan,
        cache_dir=cache_dir,
        shutdown_callback=graceful_shutdown,
    )

    sync_interval = int(config.get("sync_interval_minutes") or 15)
    start_periodic_sync_loop(indexer, sync_interval, stop_event)

    LOG.info(
        "Dragon Radar Service (%s) escuchando en http://%s:%d (modo=%s, DB=%s)",
        APP_VERSION,
        host,
        port,
        lan_mode,
        db_path,
    )

    server = create_server(app, host=host, port=port, threads=16, channel_timeout=120)
    server_holder["server"] = server

    if args.open_browser and not args.no_browser:
        threading.Timer(0.8, lambda: webbrowser.open(f"http://127.0.0.1:{port}")).start()

    try:
        server.run()
    finally:
        stop_event.set()
        indexer.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
