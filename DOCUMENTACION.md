# Documentación del proyecto

## 1. Propósito y variantes

Radar del Dragón es una herramienta local para indexar y localizar archivos. La entrega de cliente LAN preparada en este proyecto utiliza Python y scripts de PowerShell. El código Rust y los paquetes portables antiguos siguen en el repositorio como variantes históricas/alternativas; no forman parte de la instalación Python offline descrita aquí y su ejecución debe aprobarse por separado.

| Componente | Función | Ubicación |
| --- | --- | --- |
| Aplicación local | API del agente, búsqueda, indexación y vista previa | `server.py`, `radar_indexer.py`, `radar_preview.py` |
| Interfaz | Página, lógica de búsqueda y estilos | `ui/` |
| Configuración de ejemplo | Valores de referencia para el servicio | `config/site_config.example.json` |
| Activación cliente | Copia la app al perfil, prepara entorno Python y activa el agente local | `deploy/activate-client.ps1` |
| Dependencias | Comprobación e instalación desde índice o wheelhouse offline | `deploy/install-client-dependencies.ps1`, `requirements.txt` |
| Instalación offline completa | Verifica paquete, firma de Python, prepara Python y activa el cliente | `deploy/install-client-offline.ps1` |
| Constructor offline | Ensambla runtime, aplicación y wheels en un ZIP autocontenido | `deploy/build-full-offline-client.ps1` |

El paquete actual está en `dist/Radar-del-Dragon-Cliente-Offline-Python314-win-x64.zip`. Se generó para Windows x64 con Python 3.14.10 y las dependencias fijadas en `requirements.txt`. Si se cambia Python o cualquier requisito, hay que reconstruir el wheelhouse y el ZIP y volver a validar el paquete.

## 2. Instalación y datos locales

1. Extraer el paquete completo en una carpeta local con permisos de escritura del usuario.
2. Ejecutar `INSTALL_OFFLINE.bat`.
3. El instalador revisa `MANIFEST-SHA256.txt`, valida la firma Authenticode del instalador Python y usa las ruedas de `wheelhouse` sin conexión.
4. La activación copia la aplicación a `%LOCALAPPDATA%\RadarDelDragon\app`, prepara un entorno virtual y registra una tarea por usuario cuando Windows lo permite. Si el registro de tarea está bloqueado, el script intenta mantener el agente solo durante la sesión actual.
5. La configuración, base SQLite, índices auxiliares, registros y reporte de dependencias se guardan bajo `%LOCALAPPDATA%\RadarDelDragon\data` y `config` dentro del perfil del usuario, según el componente.
6. El agente local escucha en `127.0.0.1:8092`; la interfaz se abre en `http://127.0.0.1:8092`.

La ejecución está diseñada para no requerir elevación. Esto no garantiza que cada entorno corporativo permita Python, PowerShell, tareas por usuario, localhost o los módulos nativos. Si una política bloquea alguno, detener la instalación y solicitar el procedimiento aprobado por TI. Ningún script cambia ExecutionPolicy, Firewall o controles de endpoint.

Para activar desde un servidor LAN después de que el paquete/runtimes estén aprobados, el instalador de cliente puede recibir URL del servidor y ubicación del wheelhouse. La URL entregada al navegador y los orígenes permitidos deben coincidir con la configuración real. No usar listas CORS abiertas ni asumir que las IP de ejemplo siguen vigentes.

## 3. Generación del paquete y control de integridad

El constructor `deploy/build-full-offline-client.ps1` lee el instalador oficial ubicado por defecto en `dist/python-3.14.10-amd64.exe`, verifica firma y copia:

- `Python/python-3.14.10-amd64.exe`.
- `RadarAgent/` con servidor Python, indexador, vista previa, UI, requisitos y scripts.
- `wheelhouse/` con ruedas y `WHEELS-SHA256SUMS.txt`.
- `install-client-offline.ps1`, `INSTALL_OFFLINE.bat` y `README-INSTALACION.txt`.
- `MANIFEST-SHA256.txt` para cada archivo del paquete y un `.sha256.txt` externo para el ZIP.

Comando:

```powershell
.\deploy\build-full-offline-client.ps1
```

El constructor no reemplaza una carpeta/ZIP previo. Para otro runtime aprobado se puede pasar `-PythonInstallerPath`; hay que comprobar que el wheelhouse corresponda a la misma versión y arquitectura antes de empaquetar. El manifiesto detecta corrupción accidental, pero puede ser reemplazado junto con los archivos; para autenticidad, TI debe firmar/publicar el paquete o comunicar el hash por un canal confiable.

El ZIP viejo `client-wheelhouse-py314-win_amd64.zip` contiene únicamente ruedas, requirements, instrucciones y versiones del instalador Python. No es el paquete de instalación completo. Desplegar el ZIP nuevo cuyo nombre incluye `Cliente-Offline-Python314`.

## 4. Arquitectura LAN y privacidad esperada

En el diseño `local_agent`, principal y respaldo/proxy sirven la interfaz y los recursos de activación. Cada cliente busca en su propio agente local. El objetivo es mantener la base de índice, rutas, consultas, fragmentos, vistas previas y resultados en el equipo cliente.

La separación debe confirmarse en la LAN con captura de red, herramientas del navegador y logs del proxy/servidor. La arquitectura no queda demostrada solo porque la configuración diga `local_agent`: se debe verificar que las solicitudes de datos del navegador apunten a `127.0.0.1` y que ningún proxy retransmita esas APIs hacia servidores.

Al desplegar:

- Confirmar con TI principal, respaldo, DNS/VIP, protocolo, puerto, certificados, subred y reglas de Firewall actuales.
- Servir una versión compatible de UI/activador en principal y respaldo.
- Configurar orígenes permitidos exactos según el origen real que ve el navegador.
- No proxyar al servidor central las API de búsqueda, estado del índice, apertura, descarga o vista previa del cliente.
- Confirmar desde otro equipo que el servidor rechaza APIs de datos remotas en `local_agent`.
- No habilitar `shared_server` para esta necesidad: ese modo implica búsquedas sobre rutas del servidor deliberadamente compartidas.

Las IP `172.23.174.103` y `172.23.174.11` en los archivos del proyecto son valores de referencia heredados, no valores confirmados. La configuración operativa se debe cotejar con TI antes de ejecutar cambios.

## 5. Límites administrativos y de seguridad

- El instalador Python oficial del ZIP tiene firma Authenticode de Python Software Foundation comprobada al construir el paquete. Verificar de nuevo la firma y el hash antes de despliegues sensibles.
- Instalación Python por usuario suele evitar UAC, pero CrowdStrike, AppLocker, WDAC, GPO o permisos locales pueden impedirla. No sortear esos controles.
- Crear un servicio de Windows, tarea `SYSTEM`, regla de Firewall, proxy inverso o configuración TLS de servidores requiere aprobación y normalmente privilegios administrativos. Son tareas de infraestructura distintas de la activación local.
- No distribuir ni ejecutar ejecutables Rust no firmados cuando la política de endpoint los bloquea. El instalador no elimina ni modifica ejecutables preexistentes.
- Evitar copiar bases de datos, índices, resultados, configuraciones locales y registros de pruebas a los ZIP de distribución o a servidores centrales.

## 6. Validación y recuperación

`INSTRUCCIONES_VALIDACION_CODEX.txt` contiene una secuencia independiente para que otro cliente Codex inspeccione el repositorio, revise el paquete, ejecute comprobaciones seguras y deje un informe sin asumir acceso a la LAN. `INSTRUCCIONES_ANTIGRAVITY_LAN.txt` describe el trabajo de integración y aceptación que solo se completa en la red corporativa.

Antes de desplegar a más equipos, registrar versiones y hashes, equipo/usuario piloto, alcance de indexación, duración, cantidad de elementos, memoria, errores de acceso, salud tras reinicio, comportamiento de failover y evidencia de que no se enviaron datos de búsqueda a los servidores. Si falla una prueba de privacidad, recuperación o política de seguridad, detener el rollout.

Para retirar un cliente, detener la tarea de usuario si existe, cerrar el proceso local y retirar `%LOCALAPPDATA%\RadarDelDragon` siguiendo la política de retención de la organización. No eliminar datos de usuario sin aprobación y copia si se requiere conservar el índice.
