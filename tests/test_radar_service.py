from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from radar_indexer import RadarIndexer
from server import create_app, normalize_explorer_path, parse_http_range


def _create_sample_corpus(base: Path) -> dict[str, Path]:
    docs = base / "Documentos_2026"
    sub = docs / "Contratos"
    sub.mkdir(parents=True, exist_ok=True)

    txt_file = sub / "acta_entrega_2026.txt"
    txt_file.write_text(
        "Acta de entrega oficial para la dirección de telemática.\n"
        "Incluye código único RADAR-ALPHA-901 y presupuesto aprobado.",
        encoding="utf-8",
    )

    borrador = sub / "acta_entrega_borrador.txt"
    borrador.write_text("Borrador preliminar de prueba.", encoding="utf-8")

    csv_file = docs / "inventario.csv"
    csv_file.write_text("Equipo,Serial,Estado\nServidor,SN-001,Operativo\n", encoding="utf-8")

    json_file = docs / "config_demo.json"
    json_file.write_text('{"servicio": "dragon-radar", "puerto": 8092}', encoding="utf-8")

    pdf_file = docs / "resolucion_huerfanas.pdf"
    import pymupdf  # type: ignore

    pdf_doc = pymupdf.open()
    page = pdf_doc.new_page()
    page.insert_text((72, 72), "Resolución Ministerial 2026 — Diagnóstico Especial CIE10-Z99")
    pdf_doc.save(str(pdf_file))
    pdf_doc.close()

    docx_file = sub / "informe_tecnico.docx"
    import docx  # type: ignore

    d = docx.Document()
    d.add_heading("Informe Técnico Anual", level=1)
    d.add_paragraph("Migración del motor Dragon Radar a servicio Python sin ejecutables.")
    d.save(str(docx_file))

    xlsx_file = docs / "presupuesto_2026.xlsx"
    import openpyxl  # type: ignore

    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Resumen"
    ws.append(["Rubro", "Valor", "Observacion"])
    ws.append(["Infraestructura", 1500000, "Aprobado por comité"])
    wb.save(str(xlsx_file))
    wb.close()

    return {
        "txt": txt_file,
        "borrador": borrador,
        "csv": csv_file,
        "json": json_file,
        "pdf": pdf_file,
        "docx": docx_file,
        "xlsx": xlsx_file,
    }


class TestRadarService(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="dragon_radar_test_")
        self.tmp_path = Path(self._tmp.name)
        self.corpus_dir = self.tmp_path / "corpus"
        self.corpus_dir.mkdir(parents=True, exist_ok=True)
        self.files = _create_sample_corpus(self.corpus_dir)

        self.db_path = self.tmp_path / "data" / "radar_test.sqlite3"
        self.cache_dir = self.tmp_path / "cache"
        self.indexer = RadarIndexer(
            db_path=self.db_path,
            machine_name="TEST-HOST",
            shared_roots=[self.corpus_dir],
            storage_mode="LOCAL_SERVICE",
        )
        self.indexer.sync_blocking([self.corpus_dir])

        self.config = {
            "port": 8092,
            "lan_mode": "local_agent",
            "authorized_subnet": "172.23.174.0/24",
            "shared_paths": [str(self.corpus_dir)],
            "libreoffice_path": "",
            "sync_interval_minutes": 15,
        }
        self.app = create_app(
            config=self.config,
            indexer=self.indexer,
            listen_port=8092,
            bind_lan=True,
            cache_dir=self.cache_dir,
        )
        self.client = self.app.test_client()

    def tearDown(self) -> None:
        self.indexer.stop()
        self._tmp.cleanup()

    def test_indexer_counts_and_incremental_prune(self) -> None:
        total, with_content = self.indexer.get_counts()
        self.assertEqual(total, 7)
        self.assertEqual(with_content, 7)

        # Second sync should skip all unchanged files
        snap2 = self.indexer.sync_blocking([self.corpus_dir])
        self.assertEqual(snap2["new_files"], 0)
        self.assertEqual(snap2["updated_files"], 0)
        self.assertEqual(snap2["skipped_files"], 7)

        # Deleting a file should prune it on next sync
        self.files["borrador"].unlink()
        snap3 = self.indexer.sync_blocking([self.corpus_dir])
        self.assertEqual(snap3["deleted_files"], 1)
        total_after, _ = self.indexer.get_counts()
        self.assertEqual(total_after, 6)

    def test_search_queries_and_modes(self) -> None:
        # Accent-insensitive content search inside PDF
        res_pdf = self.indexer.search("resolucion CIE10", mode="all")
        self.assertTrue(any(r["name"] == "resolucion_huerfanas.pdf" for r in res_pdf))

        # Content inside DOCX
        res_docx = self.indexer.search("servicio Python ejecutables", mode="content")
        self.assertTrue(any(r["name"] == "informe_tecnico.docx" for r in res_docx))

        # Content inside XLSX
        res_xlsx = self.indexer.search("Infraestructura comite", mode="all")
        self.assertTrue(any(r["name"] == "presupuesto_2026.xlsx" for r in res_xlsx))

        # Exact extension token
        res_ext = self.indexer.search(".pdf", mode="all")
        self.assertEqual([r["name"] for r in res_ext], ["resolucion_huerfanas.pdf"])

        # Extension mode
        res_ext_mode = self.indexer.search("xlsx", mode="extension")
        self.assertEqual([r["name"] for r in res_ext_mode], ["presupuesto_2026.xlsx"])

        # Negation excludes borrador
        res_neg = self.indexer.search("acta !borrador", mode="all")
        names = [r["name"] for r in res_neg]
        self.assertIn("acta_entrega_2026.txt", names)
        self.assertNotIn("acta_entrega_borrador.txt", names)

        # OR query
        res_or = self.indexer.search("inventario | informe_tecnico", mode="all")
        names_or = {r["name"] for r in res_or}
        self.assertTrue({"inventario.csv", "informe_tecnico.docx"}.issubset(names_or))

        # Wildcard query
        res_wc = self.indexer.search("acta*2026", mode="all")
        self.assertEqual([r["name"] for r in res_wc], ["acta_entrega_2026.txt"])

    def test_http_endpoints_and_falcon_center_contract(self) -> None:
        # Static UI served directly
        r_idx = self.client.get("/")
        self.assertEqual(r_idx.status_code, 200)
        self.assertIn(b"DRAGON RADAR", r_idx.data)

        r_js = self.client.get("/radar.js")
        self.assertEqual(r_js.status_code, 200)

        # Health & server info
        r_health = self.client.get("/health")
        self.assertEqual(r_health.status_code, 200)
        h = r_health.get_json()
        self.assertEqual(h["app"], "dragon-radar")
        self.assertEqual(h["status"], "ok")
        self.assertEqual(h["engine"], "python-service")

        r_info = self.client.get("/api/server-info")
        self.assertEqual(r_info.status_code, 200)
        info = r_info.get_json()
        self.assertTrue(info["client_is_local"])
        self.assertEqual(info["port"], 8092)

        # Status contract expected by Falcon Center & UI HUD
        r_status = self.client.get("/api/status")
        self.assertEqual(r_status.status_code, 200)
        st = r_status.get_json()
        self.assertEqual(st["redb_records"], 7)
        self.assertEqual(st["vector_count"], 7)
        self.assertEqual(st["phase"], "READY")
        self.assertEqual(st["machine_name"], "TEST-HOST")

        # Validate endpoint
        r_val = self.client.get("/api/index/validate")
        self.assertEqual(r_val.status_code, 200)
        v = r_val.get_json()
        self.assertEqual(v["app"], "dragon-radar")
        self.assertTrue(v["complete"])

        # Extensions endpoint
        r_ext = self.client.get("/api/extensions?limit=10")
        self.assertEqual(r_ext.status_code, 200)
        exts = {item["extension"] for item in r_ext.get_json()["extensions"]}
        self.assertTrue({"pdf", "docx", "xlsx", "txt"}.issubset(exts))

        # Search endpoint
        r_search = self.client.get("/api/search?q=RADAR-ALPHA-901&mode=all&limit=50")
        self.assertEqual(r_search.status_code, 200)
        hits = r_search.get_json()
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]["name"], "acta_entrega_2026.txt")

    def test_previews_raw_range_and_download(self) -> None:
        for key, expected_kind in [
            ("pdf", "pdf"),
            ("docx", "word"),
            ("xlsx", "excel"),
            ("csv", "table"),
            ("json", "code"),
        ]:
            r = self.client.get("/api/preview", query_string={"file": str(self.files[key])})
            self.assertEqual(r.status_code, 200)
            data = r.get_json()
            self.assertTrue(data["success"])
            self.assertEqual(data["kind"], expected_kind)

        # Raw with HTTP Range 206
        r_raw = self.client.get(
            "/api/raw",
            query_string={"file": str(self.files["txt"])},
            headers={"Range": "bytes=0-15"},
        )
        self.assertEqual(r_raw.status_code, 206)
        self.assertEqual(len(r_raw.data), 16)

        # Download attachment
        r_dl = self.client.get("/api/download", query_string={"file": str(self.files["csv"])})
        self.assertEqual(r_dl.status_code, 200)
        self.assertIn("attachment", r_dl.headers.get("Content-Disposition", ""))

    def test_cidr_and_open_security(self) -> None:
        # Unauthorized external IP rejected with 403
        r_ext_ip = self.client.get("/health", environ_base={"REMOTE_ADDR": "8.8.8.8"})
        self.assertEqual(r_ext_ip.status_code, 403)

        # Authorized LAN IP allowed on /health but blocked on /api/open
        r_lan_health = self.client.get("/health", environ_base={"REMOTE_ADDR": "172.23.174.55"})
        self.assertEqual(r_lan_health.status_code, 200)

        r_lan_open = self.client.get(
            "/api/open",
            query_string={"file": str(self.files["txt"])},
            environ_base={"REMOTE_ADDR": "172.23.174.55"},
        )
        self.assertEqual(r_lan_open.status_code, 403)

        # Session 0 detection returns safe JSON without launching explorer
        with mock.patch("server.is_running_in_session_zero", return_value=True):
            r_s0 = self.client.get("/api/open", query_string={"file": str(self.files["txt"])})
            self.assertEqual(r_s0.status_code, 200)
            body = r_s0.get_json()
            self.assertFalse(body["opened"])
            self.assertTrue(body["session0"])

    def test_shared_server_path_containment(self) -> None:
        outside_file = self.tmp_path / "secreto_fuera.txt"
        outside_file.write_text("no autorizado", encoding="utf-8")

        shared_cfg = dict(self.config, lan_mode="shared_server", shared_paths=[str(self.corpus_dir)])
        shared_app = create_app(
            config=shared_cfg,
            indexer=self.indexer,
            listen_port=8092,
            bind_lan=True,
            cache_dir=self.cache_dir,
        )
        shared_client = shared_app.test_client()

        r_bad = shared_client.get(
            "/api/preview",
            query_string={"file": str(outside_file)},
            environ_base={"REMOTE_ADDR": "172.23.174.55"},
        )
        self.assertEqual(r_bad.status_code, 403)

        r_ok = shared_client.get(
            "/api/preview",
            query_string={"file": str(self.files["txt"])},
            environ_base={"REMOTE_ADDR": "172.23.174.55"},
        )
        self.assertEqual(r_ok.status_code, 200)

    def test_helpers(self) -> None:
        self.assertEqual(normalize_explorer_path(r"\\?\D:\Docs\Prueba.pdf"), r"D:\Docs\Prueba.pdf")
        self.assertEqual(normalize_explorer_path(r"\\?\UNC\srv\share\a.docx"), r"\\srv\share\a.docx")
        self.assertEqual(normalize_explorer_path("D:/A//B/c.txt"), r"D:\A\B\c.txt")
        self.assertEqual(parse_http_range("bytes=0-99", 1000), (0, 99))
        self.assertEqual(parse_http_range("bytes=500-", 1000), (500, 999))
        self.assertEqual(parse_http_range("bytes=-100", 1000), (900, 999))
        self.assertIsNone(parse_http_range("bytes=1200-1500", 1000))


if __name__ == "__main__":
    unittest.main()
