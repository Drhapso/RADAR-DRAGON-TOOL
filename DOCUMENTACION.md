# 📘 Documentación Técnica — Radar del Dragón (Dragon Radar Tool)

Documento de referencia técnica sobre arquitectura, motores de almacenamiento e indexación, vista previa documental, perfiles de compilación (`estándar` vs `PORTABLE`) y flujo de publicación en GitHub Desktop.

---

## 1. Arquitectura del Workspace Rust

| Crate | Responsabilidad | Archivos Clave |
| --- | --- | --- |
| `dragon-core` | Estructuras (`FileRecord`, `SearchMode`, `FileCategory`), normalización de extensiones (`extract_extension_lower`) y detección de rutas ruidosas (`is_noisy_build_or_cache_path`). | `crates/dragon-core/src/lib.rs` |
| `dragon-mft` | Escaneo rápido NTFS MFT en Windows con fallback paralelo `jwalk` y descubrimiento de ubicaciones de usuario (`discover_local_user_targets`). | `crates/dragon-mft/src/*` |
| `dragon-storage` | Persistencia ACID en `redb`: metadatos, rutas, checkpoints incrementales, ajustes persistentes e **índice invertido de extensiones** (`extension_index`). | `crates/dragon-storage/src/lib.rs` |
| `dragon-index` | Índice invertido BM25 en Tantivy para texto completo de documentos (`FileCategory::Document`) con expansión por prefijos y plegado de acentos. | `crates/dragon-index/src/lib.rs` |
| `dragon-extract` | Extractores de texto y renderizadores visuales OOXML para Word, Excel, PowerPoint, PDF y archivos planos/código. | `crates/dragon-extract/src/lib.rs`, `office.rs`, `office_visual.rs`, `pdf.rs`, `text.rs` |
| `dragon-ai` | Embeddings locales (`Model2Vec` o `HashEmbedder`), almacén vectorial y ranking híbrido RRF. | `crates/dragon-ai/src/*` |
| `dragon-mcp` | Servidor stdio JSON-RPC (Model Context Protocol). | `crates/dragon-mcp/src/*` |
| `dragon-service` | Integración de servicio Windows / Pipe MFT. | `crates/dragon-service/src/*` |
| `dragon-cli` | Binario `dragon`: CLI, indexador incremental (`indexer.rs`) y servidor HTTP del HUD (`ui_server.rs`) con `index.html`, `styles.css` y `radar.js` embebidos vía `include_str!`. | `crates/dragon-cli/src/main.rs`, `indexer.rs`, `ui_server.rs` |

---

## 2. Almacenamiento en `redb` (`radar.redb`) y Búsqueda por Extensión

### Tablas en `DragonStorage`
- `files_metadata` (`u64 -> &[u8]`): `FileRecord` serializado en JSON por ID MFT/hash.
- `path_to_id` (`&str -> u64`): índice ruta absoluta -> ID.
- `extension_index` (`(&str, u64) -> ()`): índice B-Tree ordenado por `(extensión_minúscula_sin_punto, doc_id)`.
- `index_state` (`&str -> &[u8]`): huellas `IndexedFileState` (tamaño, `modified_ms`, `doc_id`) para sincronización incremental y conciliación de archivos movidos/borrados.
- `settings` (`&str -> &str`): configuración persistente del equipo.

### Reglas y mantenimiento de `extension_index`
1. **Backfill automático (`rebuild_extension_index`)**: al ejecutar `DragonStorage::open()`, si falta `extension_index_version = "1"` en `settings`, se reconstruye `extension_index` automáticamente desde todos los registros no-directorio de `files_metadata`.
2. **Actualización atómica**: `insert_batch` elimina la entrada previa si un archivo existente cambió de extensión e inserta `(ext, doc_id)`; `remove_by_paths` purga las entradas eliminadas.
3. **Consultas disponibles**:
   - `extension_exists(ext)`: consulta en $O(\log N)$ si hay al menos un archivo con esa extensión.
   - `list_top_extensions(prefix_filter, limit)`: agrupa y ordena extensiones por cantidad de archivos sin deserializar `files_metadata`.
   - `search_by_extension(extensions, name_tokens, category_filter, allow_prefix_fallback, limit)`: escanea por rango en `extension_index`, soporta múltiples extensiones y fallback por prefijo, aplica filtros opcionales por tokens en nombre/ruta y penaliza rutas de caché/build (`target\debug`, `node_modules`, `AppData`, `.git`, etc.).

### Sintaxis de búsqueda soportada en `/api/search`
- **Modo `extension` (botón `EXTENSIÓN`)**:
  - `pdf`, `.pdf`, `*.pdf`, `ext:pdf`, `tipo:xlsx`
  - Listas múltiples: `.pdf, .docx`, `xlsx csv`
  - Extensión + filtro textual de nombre/ruta: `pdf contrato`, `ext:xlsx presupuesto`
  - Coincidencia por prefijo mientras se escribe: `xl` -> `.xls`, `.xlsx`, `.xlsm`
- **Modos generales (`all` / `HÍBRIDO`, `documents`, `media`, `design`, `content`, `filename`)**:
  - Filtros explícitos (`.pdf`, `*.xlsx`, `ext:docx`, `contrato .pdf`): acotan directamente al índice de extensiones (y filtran también resultados BM25/semánticos cuando hay texto adicional).
  - Token único coincidente con una extensión indexada (ej. `xlsx`, `dwg`, `mp4`): combina coincidencias de nombre/contenido con archivos recuperados desde `extension_index`.

---

## 3. Vista Previa Documental y Regla de LibreOffice

### Flujo de interacción en la UI (`ui/index.html`, `ui/radar.js`, `ui/styles.css`)
- **1 clic** en fila o esfera del radar: selecciona el elemento y solicita `GET /api/preview?file=...`.
- **2 clics** en fila (o `Enter`): solicita `GET /api/open?file=...` para abrir el Explorador de Windows seleccionando el archivo (`explorer.exe /select,"..."`).
- Se eliminó por completo la integración y el botón nativo de QuickLook (`/api/quicklook` y `#previewQuickLookBtn` / `#modalQuickLookBtn`).

### Renderizado de Office (`crates/dragon-extract/src/office_visual.rs`)
- **Word (`WordVisualModel`)**: parsea `word/document.xml`, estilos, encabezados, negritas/itálicas/subrayado, colores, alineación, listas, tablas e imágenes embebidas referenciadas mediante `word/_rels/document.xml.rels` servidas vía `/api/zip-asset`.
- **Excel (`ExcelVisualModel`)**: parsea `xl/workbook.xml`, `sharedStrings.xml`, hojas de cálculo y celdas combinadas (`mergeCell`), renderizando cuadrícula con cabeceras alfabéticas y selector de hojas.
- **PowerPoint (`PptxVisualModel`)**: parsea `ppt/presentation.xml`, diapositivas (`slides/slide*.xml`), relaciones de imágenes, formas posicionadas y notas del orador en formato panorámico 16:9.
- **Conversión Office -> PDF con caché (`/api/office-pdf`)**: si LibreOffice está instalado, convierte bajo demanda con perfil aislado y caché FNV-1a en `%TEMP%\dragon-office-preview\<hash>.pdf` con soporte HTTP `Range`.

### Comprobación de LibreOffice en primera ejecución
- En `run_ui_server`, se invoca `ensure_libreoffice_first_run_check(&state.storage)` (clave `libreoffice_first_run_checked_v1` y `libreoffice_detected_path_v1` en `settings`).
- `find_soffice_exe()` inspecciona `ProgramFiles`, `ProgramFiles(x86)`, `LOCALAPPDATA\Programs\LibreOffice\program` y `PATH` buscando `soffice.com` / `soffice.exe`.
- `/api/status` expone `libreoffice_installed`, `libreoffice_first_run_checked` y `libreoffice_path`. Si `libreoffice_installed == false`, la UI muestra el banner `#libreofficeNoticeBanner` (ocultable por sesión) informando que el visor OOXML sigue activo pero sin renderizado PDF exacto.

---

## 4. Perfiles de Compilación: Estándar vs `PORTABLE` (Sin LAN)

El crate `crates/dragon-cli/Cargo.toml` define la feature:

```toml
[features]
default = ["lan"]
lan = []
```

| Aspecto | Edición Estándar (`.\dragon.exe`) | Edición PORTABLE (`.\PORTABLE\dragon.exe`) |
| --- | --- | --- |
| Comando Cargo | `cargo build --release --bin dragon` | `RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --bin dragon --no-default-features --target-dir target\portable` |
| Feature `lan` | Habilitada | Deshabilitada |
| Enlace de red HTTP | Local (`127.0.0.1`) o LAN (`--lan` / `0.0.0.0`) | Exclusivamente `127.0.0.1` (loopback) |
| Endpoints `/api/agent/*` | Activos (`/api/agent/launcher`, `/api/agent/binary`) | Eliminados en compilación (`404 Not Found`) |
| Dependencia CRT MSVC | Dinámica estándar | Enlace estático (`+crt-static`, sin requerir VC++ Redistributable) |
| Estado en `/api/status` | `"lan_enabled": true, "portable_edition": false` | `"lan_enabled": false, "portable_edition": true` |

### Uso del script `compilador.ps1`
```powershell
.\compilador.ps1                # Valida JS/tests, compila dragon.exe y PORTABLE\dragon.exe, y valida pesos GitHub
.\compilador.ps1 -KillRunning   # Cierra instancias previas de dragon.exe antes de compilar
.\compilador.ps1 -SoloEstandar  # Solo compila .\dragon.exe
.\compilador.ps1 -SoloPortable  # Solo compila .\PORTABLE\dragon.exe
.\compilador.ps1 -SkipTests     # Compila sin re-ejecutar cargo test
```

---

## 5. Paquete Instalador Offline y Modo LAN Híbrido (`config/site_config.json`)

- **Puerto oficial**: `8092` (configurable en `config/site_config.json`; el lanzador `Iniciar-Radar.ps1` busca libre entre `8092` y `8101` verificando `GET /health` -> `{"app": "dragon-radar", "status": "ok"}`).
- **Configuración híbrida (`config/site_config.example.json` -> `config/site_config.json`)**:
  - `lan_mode = "local_agent"` *(predeterminado)*: las peticiones remotas a `/api/status`, `/api/search`, `/api/preview`, `/api/raw`, `/api/download`, `/api/open` se bloquean (`403`) en el anfitrión; los clientes LAN descargan el activador local y buscan exclusivamente en sus propios discos.
  - `lan_mode = "shared_server"`: indexa exclusivamente `shared_paths` en un índice aislado (`.dragon-shared-data` / `.dragon-shared-index`), valida contención canónica de rutas en cada petición remota, deshabilita `/api/open` remoto y habilita `GET /api/download` para descarga directa desde el navegador.
  - `libreoffice_path`: ruta opcional explícita a `soffice.com` / `soffice.exe`.
- **Interfaz 100 % offline**: sin referencias a Google Fonts ni CDNs externos.
- **Constructor del instalador offline**:
  ```powershell
  .\deploy\build-offline-package.ps1
  ```
  Genera `dist\Radar-del-Dragon-Offline-1.3.0-win-x64.zip` + `.sha256.txt` con binario `+crt-static`, `INSTALAR.bat`/`INSTALAR.ps1`, `Iniciar-Radar.ps1`, `Desinstalar-Radar.ps1` y `herramientas\configure-firewall.ps1`.

---

## 6. Política de Git / GitHub Desktop y Límites de Peso

- `.gitignore` ignora `/target/` (incluyendo `target/portable/`), bases de runtime (`.dragon-data/`, `.dragon-index/`, `.dragon-models/`, `PORTABLE/.dragon-data/`, `PORTABLE/.dragon-index/`, `PORTABLE/.dragon-models/`) y ejecutables temporales `*.exe`, **excepto** `!PORTABLE/dragon.exe`.
- GitHub establece una advertencia en **50 MB** y un límite estricto de rechazo en **100 MB** por archivo individual.
- `compilador.ps1` audita todos los archivos trackeables y no ignorados (`git ls-files` + `git ls-files --others --exclude-standard`) para garantizar que `PORTABLE/dragon.exe` y el resto del repositorio estén muy por debajo de ambos límites antes de hacer commit y push en **GitHub Desktop**.
