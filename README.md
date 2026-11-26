# Radar del Dragón

Radar del Dragón ayuda a localizar archivos del equipo por nombre, tipo y contenido. Presenta resultados en una interfaz web local, permite revisar vistas previas compatibles y abrir la ubicación del archivo.

## Funciones

- Catálogo local para acelerar búsquedas posteriores.
- Búsqueda de documentos por nombre, extensión y texto extraído cuando el formato lo permite.
- Filtros por tipo y vistas previas para formatos compatibles.
- Apertura de la carpeta del resultado desde el equipo que lo indexó.
- Modo cliente LAN: la interfaz se obtiene de la organización y el agente consulta el catálogo local del usuario.
- Instalación offline de cliente Windows x64 con runtime Python y dependencias incluidas.

## Instalación offline en Windows x64

Usa el paquete completo `dist/Radar-del-Dragon-Cliente-Offline-Python314-win-x64.zip`.

1. Extrae el ZIP completo en una carpeta local donde tu usuario pueda escribir.
2. Ejecuta `INSTALL_OFFLINE.bat`.
3. Cuando termine, abre `http://127.0.0.1:8092` si el navegador no se abre automáticamente.

El instalador verifica los archivos del paquete, instala Python 3.14.10 para el usuario actual si no hay una instalación compatible y prepara las dependencias sin conectarse a Internet. La aplicación, su catálogo, sus registros y sus resultados permanecen en el equipo cliente. El instalador no solicita elevación de privilegios. Las políticas de Windows o de seguridad corporativa pueden impedir scripts o runtime; en ese caso solicita el despliegue aprobado por TI y no intentes eludirlas.

Consulta `dist/Radar-del-Dragon-Cliente-Offline-Python314-win-x64/README-INSTALACION.txt` para los pasos y `INSTRUCCIONES_VALIDACION_CODEX.txt` para una validación controlada.

## Uso desde la red local

El servidor principal y el de respaldo publican la página de entrada según la configuración aprobada por TI. Cada usuario activa el agente en su equipo. En el modo `local_agent`, el cliente realiza las búsquedas contra su agente local y no necesita enviar al servidor central su índice para obtener resultados.

La disponibilidad de los servidores, los DNS/IP, el proxy, TLS, los puertos y las reglas de red debe comprobarse en la LAN. Los valores de `config/site_config.example.json` son ejemplos y no confirman el estado operativo de la red.

## Paquete offline

El ZIP completo contiene el runtime Python x64, la aplicación Python, la interfaz web, los scripts de activación, las dependencias wheel y sus manifiestos de integridad. Su hash SHA-256 está en el archivo lateral `.sha256.txt`. Comparte el hash recibido por un canal confiable antes de desplegarlo.

Para reconstruirlo en una estación aprobada:

```powershell
.\deploy\build-full-offline-client.ps1
```

El constructor exige una firma Authenticode válida de Python Software Foundation para el instalador Python y no sobrescribe una salida existente. El manifiesto y el hash detectan cambios; no sustituyen la aprobación corporativa ni constituyen una firma del paquete.

## Alcance y variantes

El cliente Python es la opción documentada para el despliegue LAN sin distribuir un ejecutable de la aplicación. El proyecto también conserva componentes del motor Rust y paquetes portables anteriores. Su presencia no implica aprobación de CrowdStrike ni paridad funcional con el cliente Python; usa solo la variante autorizada para tu equipo.

Para la configuración de servidores, privacidad, operación y recuperación, consulta `DOCUMENTACION.md` e `INSTRUCCIONES_ANTIGRAVITY_LAN.txt`.
