# 🐉 Radar del Dragón (Dragon Radar Tool)

**Radar del Dragón** es una herramienta ultra-rápida escrita en Rust para indexar, buscar y previsualizar cualquier archivo o documento dentro de tu computadora en milisegundos, con una interfaz táctica inspirada en un radar clásico.

---

## ✨ Funcionalidades Principales

### 1. Búsqueda híbrida, por contenido y por extensión
- **HÍBRIDO**: combina coincidencia por nombre, **índice directo por extensión**, búsqueda de texto completo **BM25** (Tantivy) y similitud **semántica vectorial**.
- **DOCS**: busca únicamente dentro de documentos (PDF, Word, Excel, PowerPoint, texto y código).
- **MULTIMEDIA**: localiza videos, audios y subtítulos por nombre o extensión.
- **DISEÑO/IMG**: localiza fotografías, vectores, CAD/3D y archivos de diseño gráfico.
- **CONTENIDO**: busca exclusivamente dentro del texto extraído de documentos y muestra el párrafo exacto de coincidencia.
- **NOMBRE/SYS**: busca por nombre o extensión en todo tipo de archivos del equipo.
- **EXTENSIÓN**: busca directamente usando el índice B-Tree invertido de extensiones (`extension_index`), con soporte para:
  - Extensiones simples o con comodín: `pdf`, `.pdf`, `*.xlsx`, `ext:docx`, `tipo:pptx`.
  - Listas múltiples: `.pdf, .docx` o `xlsx csv`.
  - Combinación extensión + palabra clave: `pdf contrato` o `ext:xlsx presupuesto 2026`.
  - Chips rápidos interactivos con las extensiones más frecuentes indexadas en tu equipo.
  - En el **cuadro de búsqueda general (`HÍBRIDO` y demás modos)** también puedes filtrar directamente con `.pdf`, `*.xlsx` o `ext:docx`, o escribir una extensión como `xlsx` o `dwg`.

### 2. Vista previa integrada con 1 clic y apertura con 2 clics
- **1 clic en cualquier resultado** *(o con `↑` / `↓`)*: abre la **Vista Previa integrada** en el panel derecho (sin depender de herramientas externas ni botones de QuickLook nativo).
- **2 clics en un resultado** *(o `Enter`)*: abre directamente la carpeta contenedora en el **Explorador de Windows** seleccionando el archivo.
- **Formatos soportados en vista previa**:
  - **Word (`.docx`, `.docm`, `.dotx`, `.doc`, `.odt`, `.rtf`)**: renderizado visual en hojas tipo **A4** con estilos, títulos, negritas, colores, listas, imágenes embebidas y tablas, además de conmutación automática a vista **PDF por páginas** cuando LibreOffice está disponible.
  - **Excel (`.xlsx`, `.xlsm`, `.ods`, `.csv`, `.tsv`)**: selector interactivo de pestañas por **hoja de cálculo**, columnas `A, B, C...`, filas numeradas, encabezados congelados y celdas combinadas (`colspan`/`rowspan`).
  - **PowerPoint (`.pptx`, `.pptm`, `.ppsx`, `.odp`, `.ppt`)**: visor panorámico **16:9 por diapositivas** con fondos, formas, textos posicionados, imágenes embebidas y notas del orador.
  - **PDF, texto/código, Markdown, imágenes, audio/video y comprimidos `.zip`**: visores dedicados con reproducción por streaming HTTP (`Range`).
- **Validación automática de LibreOffice en la primera ejecución**: al arrancar por primera vez, Radar del Dragón verifica si **LibreOffice** (`soffice.com` / `soffice.exe`) está instalado y guarda el estado en `radar.redb`. Si no está instalado, muestra un aviso descartable en la parte superior y continúa funcionando normalmente con el visor OOXML integrado.

### 3. Actualización automática e inteligente
- **Primera ejecución**: escanea automáticamente unidades locales y carpetas personales omitiendo rutas basura o del sistema operativo.
- **Checkpoints persistentes**: si se interrumpe el primer escaneo, continúa desde el último punto de control.
- **Validación en cada arranque**: detecta archivos nuevos, modificados por peso/fecha, movidos de carpeta y eliminados.

---

## 🧭 Controles y Atajos de Teclado

- **1 clic en fila / esfera del radar**: selecciona el archivo y carga su Vista Previa integrada.
- **2 clics en fila**: abre la ubicación del archivo en el Explorador de Windows.
- **Botón superior del radar**: cambia la escala de zoom (`1X`, `2X`, `3X`) y dispara una onda de pulso.
- **`↑` / `↓`**: navega entre los resultados y actualiza la vista previa.
- **`Espacio`**: amplía la vista previa en el modal expandido *(cuando el cursor no está escribiendo en el buscador)*.
- **`Enter`**: abre la carpeta del archivo seleccionado en el Explorador.
- **`Esc`**: cierra la vista previa ampliada.

---

## 🛠️ Compilación y Edición PORTABLE

### Script compilador en PowerShell (`compilador.ps1`)
Para compilar los cambios del proyecto y actualizar ambos ejecutables automáticamente:

```powershell
.\compilador.ps1
```

Este script:
1. Valida sintaxis JS (`node --check ui\radar.js`) y ejecuta las pruebas (`cargo test --workspace` y perfil portable sin LAN).
2. Compila la edición estándar en release y actualiza **`.\dragon.exe`**.
3. Compila la edición autónoma sin LAN (`--no-default-features` + `-C target-feature=+crt-static`) en **`.\PORTABLE\dragon.exe`**.
4. Verifica que ningún archivo trackeable supere los límites de GitHub (**50 MB** alerta / **100 MB** máximo) para hacer commit y push sin problemas desde **GitHub Desktop**.

Opciones útiles:
- `.\compilador.ps1 -KillRunning`: cierra procesos activos de `dragon.exe` antes de compilar.
- `.\compilador.ps1 -SoloEstandar` / `-SoloPortable`: compila solo una de las dos ediciones.
- `.\compilador.ps1 -SkipTests`: omite las pruebas previas para compilar más rápido.

### Carpeta `PORTABLE\` (Sin Red LAN)
La carpeta [`PORTABLE`](file:///C:/Users/Drhapso/Documents/PROYECTOS%20IA%20INDEPENDIENTES/RADAR%20DEL%20DRAGON/RADAR%20DRAGON%20TOOL/PORTABLE) contiene una versión lista para copiar y ejecutar en cualquier equipo Windows:
- **`PORTABLE\dragon.exe`**: compilado **sin la funcionalidad de red LAN** (escucha exclusivamente en `127.0.0.1`, desactiva `--lan` y las rutas `/api/agent/*`) y con CRT estático para funcionar sin dependencias externas.
- **`PORTABLE\iniciar_portable.bat`**: acceso directo para iniciar el radar portátil.
- **`PORTABLE\README_PORTABLE.md`**: instrucciones de uso de la edición portátil.

---

## 🌐 Uso estándar y en Red LAN

- **Uso normal en este PC**: ejecuta **`iniciar_radar.bat`** o **`dragon.exe`**.
- **Modo servidor LAN** *(solo edición estándar)*: ejecuta `dragon.exe ui --lan` para exponer el HUD en la red local (`0.0.0.0`). Cada computadora cliente descarga o activa su motor local aislado para buscar exclusivamente en sus propios discos.
- **Indexación manual anticipada**: ejecuta **`indexar_equipo.bat`** o **`indexar_equipo.ps1`**.

Para detalles técnicos de arquitectura, endpoints REST, esquema de tablas `redb` y compilación, consulta [`DOCUMENTACION.md`](file:///C:/Users/Drhapso/Documents/PROYECTOS%20IA%20INDEPENDIENTES/RADAR%20DEL%20DRAGON/RADAR%20DRAGON%20TOOL/DOCUMENTACION.md).
