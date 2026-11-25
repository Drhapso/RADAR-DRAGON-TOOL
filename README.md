# 🐉 Dragon Radar Tool

[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-orange?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20x64-0078D6?logo=windows)](https://microsoft.com)
[![Protocol](https://img.shields.io/badge/MCP-2024--11--05-brightgreen)](https://modelcontextprotocol.io/)

> **Motor de búsqueda híbrido de ultra-alta velocidad para disco y contenidos, equipado con indexación MFT a nivel de hardware, búsqueda de texto completo BM25, inferencia semántica vectorial en CPU y servidor nativo MCP para agentes de inteligencia artificial.**

---

## ⚡ Visión General

**Dragon Radar Tool** es una solución de búsqueda y análisis de archivos de alto rendimiento desarrollada en Rust. Combina la velocidad instantánea de lectura directa de tablas de sistema de archivos con un motor profundo de indexación de contenidos y comprensión semántica vectorial, presentado a través de una interfaz táctica HUD interactiva y expuesto mediante el protocolo estándar MCP para asistentes inteligentes.

```
                              ┌───────────────────────────────────┐
                              │         DRAGON RADAR TOOL         │
                              └─────────────────┬─────────────────┘
                                                │
         ┌────────────────────────┬─────────────┴────────────┬────────────────────────┐
         ▼                        ▼                          ▼                        ▼
 ┌───────────────┐        ┌───────────────┐          ┌───────────────┐        ┌───────────────┐
 │   MFT / USN   │        │     REDB      │          │    TANTIVY    │        │   MODEL2VEC   │
 │ Hardware Disc │        │ Compact ACID  │          │ Full-Text     │        │ CPU Semantic  │
 │ 100k+ file/s  │        │ B-Tree MVCC   │          │ BM25 + LZ4    │        │ 256-d Vectors │
 └───────┬───────┘        └───────┬───────┘          └───────┬───────┘        └───────┬───────┘
         │                        │                          │                        │
         └────────────────────────┼──────────────────────────┴────────────────────────┘
                                  ▼
                ┌───────────────────────────────────┐
                │   FUSIÓN HÍBRIDA RRF (BM25 + AI)  │
                └─────────────────┬─────────────────┘
                                  │
                  ┌───────────────┴───────────────┐
                  ▼                               ▼
       ┌─────────────────────┐         ┌─────────────────────┐
       │   HUD TÁCTICO WEB   │         │    SERVIDOR MCP     │
       │ Canvas CRT Fósforo  │         │   JSON-RPC stdio    │
       │ Sweep 120 RPM Blips │         │ Agentes Inteligentes│
       └─────────────────────┘         └─────────────────────┘
```

---

## 🌟 Pilares Tecnológicos

### 1. Escaneo NTFS MFT a Nivel de Hardware (`dragon-mft`)
- Acceso directo a la **Master File Table (MFT)** y al **USN Journal** de NTFS utilizando el código de control `FSCTL_ENUM_USN_DATA`.
- Capaz de procesar más de **100,000 entradas de disco por segundo**, leyendo la tabla secuencial cruda sin necesidad de recorrer recursivamente las carpetas del sistema.
- **Servicio Daemon en Segundo Plano (`dragon-service`)**: Implementa un canal Named Pipe seguro (`\\.\pipe\dragon_radar_mft`) que permite a clientes no elevados consultar la MFT sin solicitar confirmaciones UAC constantes.
- **Fallback Multi-Hilo Inteligente**: Conmuta automáticamente a un escáner concurrente con `jwalk` cuando se indexan unidades FAT32, exFAT o rutas de red.

### 2. Almacenamiento Embebido Compacto ACID (`dragon-storage`)
- Motor de persistencia nativo basado en **`redb`** (árboles B-Tree con control de concurrencia MVCC).
- Almacenamiento hermético en un **único archivo de base de datos** (`.dragon-data/radar.redb`), con costo de almacenamiento ultra-bajo (más de 5,000 archivos indexados en menos de 9 MB).
- **Cero desbordamiento de espacio**: Previene el crecimiento descontrolado de registros transaccionales (WAL), garantizando transacciones atómicas y auto-compactación periódica.

### 3. Extractor de Documentos Multi-Formato (`dragon-extract`)
- Motor nativo y seguro sin dependencias externas ni intermediarios en Python o Java:
  - **PDF**: Decodificación de flujos y cadenas de texto mediante `lopdf` (procesamiento en <30 ms).
  - **Documentos Office**: Descompresión `Deflate` y análisis directo de XML (`quick-xml`) para Word (`.docx`), Excel (`.xlsx`) y PowerPoint (`.pptx`).
  - **Código y Texto Plano**: Detección automática de BOM y decodificación multi-codificación (UTF-8, UTF-16 LE/BE, Windows-1252) con `encoding_rs`.

### 4. Búsqueda de Texto Completo BM25 (`dragon-index`)
- Motor de indexación invertido impulsado por **Tantivy**.
- Compresión de bloques de postings mediante **LZ4**.
- Calificación y desempate mediante el algoritmo probabilístico **BM25**, con respuestas de búsqueda en rangos de **1 a 3 milisegundos**.

### 5. Inferencia Semántica Vectorial en CPU (`dragon-ai`)
- Modelo estático optimizado **Model2Vec** con embeddings de **256 dimensiones**.
- Inferencia ultrarrápida sin sobrecarga: **menos de 1 milisegundo por documento** directamente en CPU x86_64, sin dependencias pesadas de frameworks de Deep Learning.
- **Almacén Vectorial SIMD**: Escaneo paralelo con Rayon calculando similitud coseno sobre memoria contigua.
- **Fusión Híbrida RRF (Reciprocal Rank Fusion)**: Combina los rankings exactos de BM25 con los rankings conceptuales vectoriales bajo la fórmula:
  $$RRF(d) = \frac{1}{60 + rank_{BM25}(d)} + \frac{1}{60 + rank_{Vector}(d)}$$

### 6. Servidor Nativo MCP (Model Context Protocol) (`dragon-mcp`)
- Implementación del protocolo MCP sobre flujos estándar `stdio` (JSON-RPC 2.0).
- Permite a asistentes inteligentes como Claude Desktop, Antigravity, Cursor o Gemini interactuar de forma autónoma con los motores de búsqueda locales:
  - `dragon_find_files`: Búsqueda instantánea de archivos por nombre o patrón en disco.
  - `dragon_search_content`: Búsqueda BM25 dentro de contenidos indexados.
  - `dragon_semantic_search`: Búsqueda conceptual por vectores semánticos.
  - `dragon_hybrid_search`: Fusión de rankings léxicos y semánticos.
  - `dragon_extract_document`: Extracción estructurada de texto de documentos.
  - `dragon_status`: Métricas del sistema en tiempo real.

### 7. HUD Táctico "Radar del Dragón" (`ui`)
- Interfaz gráfica retro-futurista inspirada en radares tácticos CRT con estética en **verde fósforo (`#00ff66`)** y **ámbar (`#ffaa00`)**.
- Pantalla de radar en Canvas HTML5 con barrido rotatorio a **120 RPM** y representación posicional de esferas (blips) según la relevancia y distancia del archivo.
- Servidor web asíncrono autocontenido que se ejecuta en segundo plano y lanza el navegador predeterminado automáticamente.
- **Vista Rápida (*Quick Look*)**: Previsualización instantánea del texto de los documentos presionando la tecla `Espacio`.
- **Apertura Directa**: Navegación con flechas y apertura en el Explorador de Windows con `Enter`.

---

## 🚀 Inicio Rápido

### Requisitos
- Windows 10 / 11 (x64)
- Rust 1.80+ (para compilación desde código fuente)

### Compilación

```powershell
# Clonar o ubicarse en el directorio del proyecto
cd "C:\Users\Drhapso\Documents\PROYECTOS IA INDEPENDIENTES\RADAR DEL DRAGON\RADAR DRAGON TOOL"

# Compilar todo el workspace en modo Release optimizado
cargo build --release

# Copiar el ejecutable a la raíz (opcional)
copy target\release\dragon.exe .\dragon.exe
```

Binarios resultantes en `target/release/`:
* **`dragon.exe`** (7.3 MB): Herramienta principal CLI, servidor HTTP del HUD y servidor MCP.
* **`dragon-mcp.exe`** (5.5 MB): Servidor MCP independiente para agentes.
* **`dragon-service.exe`** (1.7 MB): Daemon de Windows para el Named Pipe MFT.

---

## 💻 Uso de la Herramienta

### 1. Iniciar el HUD Táctico (Interfaz Visual)

Simplemente ejecuta:
```powershell
.\dragon.exe
```
*(O haz doble clic en `iniciar_radar.bat` o `dragon.exe` desde el explorador de Windows).*

El sistema levantará el servidor web local (`http://127.0.0.1:8088`), abrirá tu navegador automáticamente y desplegará la pantalla de radar táctico interactiva.

---

### 2. Indexar Todas las Unidades Locales del Equipo (Omitiendo SO)

Para indexar el contenido de tus discos locales sin saturar la base de datos con archivos del sistema operativo Windows:

```powershell
# Ejecutar el script automatizado
.\indexar_equipo.bat
```
*O mediante PowerShell:*
```powershell
.\indexar_equipo.ps1
```

**Comportamiento del indexador:**
- Detecta todas las unidades fijas locales (`C:`, `D:`, `E:`, etc.).
- Omite automáticamente directorios del sistema: `Windows`, `Program Files`, `ProgramData`, `$Recycle.Bin`, `System Volume Information`, temporales de `AppData`, etc.
- Indexa perfiles de usuario (`Documentos`, `Escritorio`, `Descargas`, proyectos de código) y carpetas de datos en unidades secundarias.
- Registra nombres en `redb`, contenidos en `Tantivy` y genera representaciones vectoriales en `vectors.bin`.
- **Indexación Automática y Checkpoints (`index_state`)**: Al iniciar la aplicación por primera vez indexa automáticamente las unidades locales omitiendo el SO, guardando checkpoints continuos para reanudar tras cualquier interrupción. En cada arranque posterior valida el índice completo: incorpora archivos nuevos, actualiza modificados por peso/fecha, actualiza rutas de archivos movidos y purga los eliminados.

---

### 3. Comandos de Consola (CLI)

#### Búsqueda Híbrida (BM25 + Semántica AI)
```powershell
.\dragon.exe hybrid "contrato de arrendamiento y pagos"
```

#### Búsqueda Léxica de Texto Completo
```powershell
.\dragon.exe search "DeviceIoControl"
```

#### Búsqueda Semántica Conceptual
```powershell
.\dragon.exe semantic "extracto bancario y finanzas"
```

#### Sincronización Automática, Checkpoints y Validación
```powershell
# Ejecutar sincronización integral con checkpoints y validación de movidos/eliminados
.\dragon.exe sync

# Indexar o actualizar incrementalmente una carpeta específica
.\dragon.exe index "C:\Users\Drhapso\Documents"

# Forzar reindexación completa ignorando huellas y checkpoints previos
.\dragon.exe sync --force
```

#### Escaneo Rápido de Disco MFT o Carpetas
```powershell
# Escaneo MFT de unidad C:
.\dragon.exe scan C

# Escaneo de una carpeta específica
.\dragon.exe scan "C:\Users\Drhapso\Documents"
```

#### Extracción Directa de Texto
```powershell
.\dragon.exe extract "ruta\al\documento.pdf"
```

#### Información y Telemetría del Sistema
```powershell
.\dragon.exe info
```

---

## 🤖 Configuración como Servidor MCP

Para conectar Dragon Radar Tool con asistentes de inteligencia artificial como **Claude Desktop**, agrega lo siguiente a tu archivo `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "dragon-radar": {
      "command": "C:\\Users\\Drhapso\\Documents\\PROYECTOS IA INDEPENDIENTES\\RADAR DEL DRAGON\\RADAR DRAGON TOOL\\dragon.exe",
      "args": ["mcp"]
    }
  }
}
```

Una vez configurado, tu asistente podrá:
- Encontrar archivos al instante en cualquier unidad local.
- Leer y resumir documentos PDF, Word, Excel y código fuente sin límites arbitrarios de tamaño.
- Realizar búsquedas conceptuales cruzadas a través de tus proyectos personales.

---

## 📁 Estructura del Proyecto

```
RADAR DRAGON TOOL/
├── crates/
│   ├── dragon-core/       # Modelos comunes, FileRecord y métricas de escaneo
│   ├── dragon-mft/        # Escáner NTFS MFT crudo (FSCTL_ENUM_USN_DATA) y fallback concurrente
│   ├── dragon-storage/    # Persistencia compacta en base de datos redb (B-Tree MVCC)
│   ├── dragon-extract/    # Extractores puros para PDF, DOCX, XLSX, PPTX y código
│   ├── dragon-index/      # Motor Tantivy BM25 con compresión LZ4
│   ├── dragon-ai/         # Inferencia Model2Vec (256-d), almacén SIMD y RRF
│   ├── dragon-mcp/        # Servidor stdio JSON-RPC 2.0 para el Model Context Protocol
│   ├── dragon-service/    # Daemon Windows con Named Pipe (\\.\pipe\dragon_radar_mft)
│   └── dragon-cli/        # CLI principal y servidor asíncrono para el HUD táctico
├── ui/                    # Frontend visual (Canvas 2D, estilos CRT y radar.js)
├── indexar_equipo.ps1     # Script de indexación para todas las unidades locales (sin SO)
├── indexar_equipo.bat     # Lanzador por doble clic del indexador de equipo
├── iniciar_radar.bat      # Lanzador por doble clic del Radar del Dragón (HUD)
├── Cargo.toml             # Configuración del workspace multi-crate de Rust
└── README.md              # Documentación técnica original
```

---

## 📄 Licencia

Este proyecto se distribuye bajo los términos de la licencia **MIT** o **Apache-2.0** a elección del desarrollador. Consulta el archivo `LICENSE` para más información.
