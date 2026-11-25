# 🐉 Radar del Dragón — Edición PORTABLE (Sin Red LAN)

Esta carpeta contiene una versión **100 % autónoma y ejecutable en cualquier equipo Windows**, compilada sin funcionalidades de servidor/cliente en red LAN y con el runtime C/C++ enlazado estáticamente (`+crt-static`).

## Contenido

- `dragon.exe`: ejecutable único autocontenido (`x86_64`, sin dependencias de VC++ Redistributable ni archivos web externos).
- `iniciar_portable.bat`: lanzador rápido para abrir el Radar del Dragón en el equipo actual.

## Características de la edición PORTABLE

- **Solo ejecución local (`127.0.0.1`)**: deshabilita el parámetro `--lan`, rechaza enlaces de red externos (`0.0.0.0`) y excluye las rutas `/api/agent/launcher` y `/api/agent/binary`.
- **Aislamiento automático por computadora**:
  - Al ejecutarse en un equipo, guarda su índice privado (`.dragon-data/` y `.dragon-index/`).
  - Si copias la carpeta o la llevas en una memoria USB a otra computadora, detecta automáticamente que el equipo cambió (`owner_machine`) y aísla el índice de la nueva PC en `%LOCALAPPDATA%\DragonRadar\<NOMBRE_EQUIPO>`, evitando mezclar archivos entre computadoras.
- **Todas las funciones integradas incluidas**:
  - Indexación incremental con checkpoints y validación de archivos nuevos/modificados/movidos/eliminados.
  - Búsqueda híbrida, por contenido (BM25 + semántica), por nombre y **por extensión** (`.pdf`, `*.xlsx`, `ext:docx`, modo `EXTENSIÓN`).
  - Vista previa integrada con **1 clic** y apertura de ubicación en el Explorador con **2 clics**, renderizado visual para Word, Excel y PowerPoint (más LibreOffice si está instalado en ese equipo, verificado al primer arranque).

## Cómo usar

1. Copia la carpeta `PORTABLE` completa (o solo `dragon.exe` + `iniciar_portable.bat`) a cualquier equipo Windows.
2. Haz doble clic en **`iniciar_portable.bat`** (o directamente en **`dragon.exe`**).
3. Se abrirá automáticamente tu navegador en `http://127.0.0.1:8088`.
