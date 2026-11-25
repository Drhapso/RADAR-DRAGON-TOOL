# 🐉 Radar del Dragón

**Radar del Dragón** es una herramienta pensada para ayudarte a encontrar cualquier archivo o documento dentro de tu computadora en cuestión de segundos, con una pantalla interactiva inspirada en un radar clásico.

---

## ✨ ¿Qué puede hacer por ti?

### 1. Encuentra archivos por su nombre o por lo que dicen por dentro
- **Documentos** *(PDF, Word, Excel, PowerPoint, libros digitales, notas y archivos de texto)*: los encuentra tanto si escribes el **nombre del archivo** como si escribes **palabras o frases que aparecen dentro del texto**. Además, te muestra una vista previa con el fragmento exacto donde apareció lo que buscabas.
- **Multimedia** *(videos, películas, audios, canciones y subtítulos)*: los ubica rápidamente por el **nombre del archivo**.
- **Imágenes y Diseño** *(fotografías, ilustraciones, vectores, diseños gráficos y planos)*: los ubica por el **nombre del archivo**.
- **Otros archivos del equipo**: también puedes localizarlos por su **nombre**.

### 2. Búsqueda flexible y sin complicaciones
- No importa si escribes **con o sin tildes** (por ejemplo, buscar `informacion` también encuentra `información`).
- No hace falta escribir la palabra completa: si escribes el comienzo de una palabra (como `contrat`), también encontrará variaciones como `contrato` o `contratos`.
- Ordena primero tus carpetas y documentos personales para que los resultados importantes aparezcan arriba.

### 3. Actualización automática e inteligente
- **Primera vez**: al abrir el programa por primera vez, revisa automáticamente tus discos y carpetas personales (Documentos, Descargas, Escritorio, imágenes, videos, etc.) omitiendo carpetas internas del sistema para no llenarte de basura.
- **Puntos de guardado**: si apagas el equipo o cierras el programa antes de que termine la primera revisión, la próxima vez continúa exactamente desde donde se quedó, sin empezar de cero.
- **Siempre al día**: cada vez que lo vuelves a abrir, verifica automáticamente si creaste archivos nuevos, editaste alguno, los moviste de carpeta o los borraste, y actualiza la lista al instante.

### 4. Privacidad y funcionamiento en red local
- **Cada equipo busca en sus propios archivos**: si la herramienta se comparte en una red local de oficina o entre varias computadoras, cada equipo crea y guarda su propio registro privado dentro de esa misma computadora.
- Al buscar desde tu equipo, **siempre** se revisarán únicamente tus archivos locales, nunca los del servidor ni los de otras personas.

---

## 🧭 Cómo usar la pantalla del Radar

1. **Abrir el programa**
   - Haz doble clic en **`iniciar_radar.bat`** (o en **`dragon.exe`**).
   - Se abrirá automáticamente una ventana en tu navegador con la pantalla del Radar. *(Mantén abierta la ventana negra de inicio mientras lo estés usando).*

2. **Escribir lo que buscas**
   - En la barra superior derecha escribe el nombre del archivo o alguna palabra que recuerdes del contenido del documento.
   - Los resultados aparecerán de inmediato en la lista y como **esferas luminosas** dentro del radar verde.

3. **Usar los filtros de búsqueda**
   - **HÍBRIDO**: busca en todo tipo de archivos a la vez (nombres y contenido).
   - **DOCS**: se enfoca únicamente en documentos (por nombre y por texto interno).
   - **MULTIMEDIA**: muestra solo videos y audios.
   - **DISEÑO/IMG**: muestra solo imágenes, vectores y archivos de diseño.
   - **CONTENIDO**: busca exclusivamente **dentro del texto** de tus documentos y muestra el párrafo donde coincide.
   - **NOMBRE/SYS**: busca por el nombre en cualquier archivo del equipo.

4. **Interactuar con los resultados**
   - **Clic en una fila**: abre la **Vista Rápida** para leer el contenido del documento al instante sin tener que abrir otros programas.
   - **Clic en una esfera del radar**: selecciona ese archivo en la lista.
   - **Clic en el botón superior del radar**: cambia el nivel de acercamiento (zoom) de la cuadrícula del radar.

5. **Atajos de teclado**
   - **`↑` / `↓`**: sube y baja entre los archivos encontrados.
   - **`Espacio`**: abre la Vista Rápida del archivo seleccionado.
   - **`Enter`**: abre directamente la carpeta donde está guardado el archivo.
   - **`Esc`**: cierra la Vista Rápida.

---

## 🌐 Uso en red local (varios equipos en oficina)

- **Desde una carpeta compartida en red:** si el programa está guardado en una carpeta compartida de la oficina, cualquier usuario puede abrir **`iniciar_radar.bat`** desde su computadora. Automáticamente se creará un índice privado en esa PC para buscar solo en sus propios discos.
- **Desde el navegador en red local:** si el administrador abre la pantalla del radar para la red de la oficina y entras desde otro equipo, verás un botón llamado **"ACTIVAR EN ESTE EQUIPO"** la primera vez. Al descargarlo y abrirlo una sola vez, tu computadora preparará su propio buscador local y todas tus búsquedas leerán únicamente tus archivos.

---

## 🔄 Si quieres preparar el equipo antes de abrir el radar

- Puedes hacer doble clic en **`indexar_equipo.bat`** para que revise todas las carpetas personales de tu computadora y deje todo listo antes de comenzar a buscar.
