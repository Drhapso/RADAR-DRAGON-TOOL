# 📘 Guía Completa de Instalación y Configuración — Radar del Dragón

Esta guía documenta **todo el proceso de instalación desde cero** (descarga de dependencias y Rust, compilación del binario estándar `.\dragon.exe` sin depender de `PORTABLE\`, y ejecución tanto en **Modo Solo Local** como en **Modo Servidor LAN**), además del **script automático para equipos externos** y el **paso a paso específico para nuestra red interna (`172.23.174.103:8092` + proxy `172.23.174.11:8092`)**.

---

## 1. Arquitectura de Ejecución (Ambas Opciones sin Depender de `PORTABLE\`)

Al compilar la **edición estándar (`.\dragon.exe`)**, un único ejecutable nativo en Rust cubre **ambas modalidades** sin depender de `PORTABLE\dragon.exe`:

| Modalidad | Lanzador | Enlace de red | Puerto | Uso |
| --- | --- | --- | --- | --- |
| **Modo Solo Local** *(este equipo)* | `servidor local.bat` o `.\deploy\run-lan.ps1 -LocalOnly -OpenBrowser` | `127.0.0.1` | `8092` | Uso personal 100 % local. No abre puertos de red. |
| **Modo Servidor LAN — Agente Local** (`lan_mode: "local_agent"`) | `servidor para LAN.bat` o `.\deploy\run-lan.ps1` | `0.0.0.0` | `8092` | Sirve el HUD en la LAN; cada PC activa su motor local para buscar solo sus propios discos (el servidor nunca expone sus archivos). |
| **Modo Servidor LAN — Repositorio Compartido** (`lan_mode: "shared_server"`) | `servidor para LAN.bat` o `.\deploy\run-lan.ps1` | `0.0.0.0` | `8092` | Indexa únicamente `shared_paths` (en un índice aislado) y todos los equipos de la LAN buscan, previsualizan y descargan desde el navegador sin instalar nada. |
| **Instalador Offline ZIP** *(estilo EVA PDF)* | `.\deploy\build-offline-package.ps1` → `INSTALAR.bat` | Local (`127.0.0.1`) o LAN (`0.0.0.0`) | `8092` | Paquete autocontenido sin Rust ni permisos de Administrador para distribuir a otros PCs Windows x64. |

---

## 2. Instalación Automática en Equipos Externos (Script de Primera Vez)

Cuando copies o clones el proyecto en un **equipo Windows 10/11/Server x64 externo** que todavía no tiene Rust ni las herramientas C++, usa el configurador automático:

### Opción rápida (doble clic)
Haz doble clic sobre **`CONFIGURAR_PRIMERA_VEZ.bat`** en la raíz del proyecto.

### Opción parametrizada desde PowerShell (como Administrador)
```powershell
cd "RUTA\AL\PROYECTO\RADAR-DRAGON-TOOL"

# Instalación estándar (puerto 8092, modo local_agent, compila .\dragon.exe)
.\configurar_primera_vez.ps1

# Instalación detrás de un proxy corporativo
.\configurar_primera_vez.ps1 -ProxyUrl "http://proxy.empresa.local:3128"

# Instalación lista para Servidor LAN compartido + Firewall + paquete offline ZIP
.\configurar_primera_vez.ps1 `
    -Port 8092 `
    -LanMode shared_server `
    -SharedPaths @("D:\Documentos_Compartidos") `
    -AuthorizedSubnet "192.168.1.0/24" `
    -ConfigurarFirewall `
    -GenerarPaqueteOffline
```

### ¿Qué hace `configurar_primera_vez.ps1` automáticamente?
1. Detecta el proxy del sistema Windows (PAC / WinINet) o usa `-ProxyUrl` y lo propaga a `rustup` y `cargo`.
2. Verifica si **Microsoft C++ Build Tools (MSVC)** está instalado; si falta, descarga `vs_buildtools.exe` directo desde Microsoft (sin depender de `winget`) e instala `VCTools`.
3. Verifica si **Rust Toolchain (`cargo` / `rustc`)** está instalado en `PATH` o `%USERPROFILE%\.cargo\bin`; si falta, descarga `rustup-init.exe` e instala `stable-x86_64-pc-windows-msvc`.
4. Comprueba si **LibreOffice** está instalado (opcional; avisa sin detener el proceso).
5. Crea `config\site_config.json` si no existe (sin sobrescribir configuraciones existentes salvo con `-ForzarConfig`).
6. Compila **`.\dragon.exe`** (edición estándar con soporte Local + LAN) mediante `.\compilador.ps1 -SoloEstandar` y, si pasas `-GenerarPaqueteOffline`, genera el ZIP instalador en `.\dist\`.
7. Si indicas `-ConfigurarFirewall -AuthorizedSubnet "<CIDR>"`, crea la regla TCP entrante `Dragon-Radar-LAN-8092`.

---

## 3. Instalación Manual Paso a Paso (Desde Cero en Cualquier Equipo Windows x64)

### Paso 1 — Instalar Microsoft C++ Build Tools (MSVC)
Rust en Windows (`x86_64-pc-windows-msvc`) necesita el enlazador `link.exe` y el compilador C `cl.exe` (usado por `zstd-sys`). En **PowerShell como Administrador**:

```powershell
Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_buildtools.exe" -OutFile "$env:TEMP\vs_buildtools.exe"
Start-Process -FilePath "$env:TEMP\vs_buildtools.exe" `
    -ArgumentList "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" `
    -Wait
```

### Paso 2 — Descargar e Instalar Rust (`rustup`, `cargo`, `rustc`)
En la misma ventana de PowerShell (si tu red usa proxy corporativo/PAC, define primero `HTTPS_PROXY` y `HTTP_PROXY`):

```powershell
# (Solo si tu red usa proxy HTTP/PAC; omitir si tienes salida directa a Internet)
$env:HTTPS_PROXY = "http://TU_PROXY:PUERTO"
$env:HTTP_PROXY  = "http://TU_PROXY:PUERTO"
$env:CARGO_HTTP_CHECK_REVOKE = "false"

# Descargar e instalar Rust toolchain estable (64 bits MSVC)
Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile "$env:TEMP\rustup-init.exe"
& "$env:TEMP\rustup-init.exe" -y --default-toolchain stable --default-host x86_64-pc-windows-msvc

# Agregar cargo a la sesión actual y verificar
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
rustc --version
cargo --version
```

### Paso 3 — Crear `config\site_config.json`
Copia `config\site_config.example.json` como `config\site_config.json` y ajusta según el uso deseado:

- **Ejemplo A — Modo Local + LAN con Agente Local por equipo (predeterminado):**
  ```json
  {
    "port": 8092,
    "lan_mode": "local_agent",
    "authorized_subnet": "192.168.1.0/24",
    "shared_paths": [],
    "libreoffice_path": ""
  }
  ```
- **Ejemplo B — Modo Servidor LAN Compartido (0 instalación en clientes):**
  ```json
  {
    "port": 8092,
    "lan_mode": "shared_server",
    "authorized_subnet": "192.168.1.0/24",
    "shared_paths": [
      "D:\\Documentos_Compartidos",
      "D:\\Normativa"
    ],
    "libreoffice_path": ""
  }
  ```

> `config\site_config.json` está excluido en `.gitignore` para proteger rutas privadas e IPs/subredes locales.

### Paso 4 — Compilar `.\dragon.exe` (sin depender de `PORTABLE\`)
Desde la raíz del proyecto:

```powershell
cd "RUTA\AL\PROYECTO\RADAR-DRAGON-TOOL"

# Compila pruebas + binario estándar .\dragon.exe (Local + LAN)
.\compilador.ps1 -SoloEstandar
```

*(Opcional) Generar también el instalador offline ZIP para distribuir a otros PCs sin Rust:*
```powershell
.\deploy\build-offline-package.ps1
```

### Paso 5 — (Opcional) LibreOffice para vista previa PDF exacta de Office
- Instala LibreOffice x64 desde `https://www.libreoffice.org/download/download-libreoffice/` (MSI completo).
- Radar del Dragón lo detecta automáticamente en `C:\Program Files\LibreOffice\program\soffice.com`. Si está en otra ruta, indícala en `"libreoffice_path"` dentro de `config\site_config.json`.
- Sin LibreOffice, el visor OOXML integrado sigue funcionando normalmente para `.docx`, `.xlsx` y `.pptx`.

---

## 4. Cómo Ejecutar en Modo Local y en Modo Servidor LAN (Ambas Opciones)

Una vez compilado `.\dragon.exe`, puedes alternar libremente entre ambas opciones:

### Opción A — Modo Solo Local (solo este equipo, sin abrir red y sin usar `PORTABLE\`)
- **Doble clic**: `servidor local.bat`
- **PowerShell (abriendo navegador)**:
  ```powershell
  .\deploy\run-lan.ps1 -LocalOnly -OpenBrowser
  ```
- **Comando directo**:
  ```powershell
  .\dragon.exe ui --host 127.0.0.1 --port 8092 --config .\config\site_config.json
  ```
- **URL**: `http://127.0.0.1:8092/`

### Opción B — Modo Servidor Local en Red LAN (`0.0.0.0:8092`)
1. **Autorizar Firewall (una sola vez, PowerShell como Administrador)**:
   ```powershell
   # Diagnóstico previo (no modifica nada)
   .\deploy\configure-firewall.ps1 -AuthorizedSubnet "192.168.1.0/24" -Port 8092

   # Aplicar regla TCP entrante (solo perfiles Domain/Private y subred autorizada)
   .\deploy\configure-firewall.ps1 -AuthorizedSubnet "192.168.1.0/24" -Port 8092 -Apply
   ```
2. **Iniciar el servidor LAN**:
   - **Doble clic**: `servidor para LAN.bat`
   - **PowerShell**:
     ```powershell
     .\deploy\run-lan.ps1
     ```
   - **Forzar modo compartido sin editar el JSON**:
     ```powershell
     .\deploy\run-lan.ps1 -LanMode shared_server
     ```
3. **Acceso desde otros equipos de la LAN**:
   - Interfaz web: `http://<IP_DEL_SERVIDOR>:8092/`
   - Estado de salud (JSON): `http://<IP_DEL_SERVIDOR>:8092/health`

> **Nota**: No ejecutes `servidor local.bat` y `servidor para LAN.bat` al mismo tiempo en el mismo puerto `8092`. Si quieres cambiar de modo, cierra primero la consola activa de `dragon.exe`.

---

## 5. Paso a Paso para la Instalación Actual en Nuestra Red (`172.23.174.103` Principal + `172.23.174.11` Proxy)

En nuestra infraestructura institucional (`BAINTS604` = `172.23.174.103`, `BAINTS601` = `172.23.174.11`, subred `172.23.174.0/24`), la configuración de red, el espejo en `.11`, las reglas del proxy TCP L4 y el watchdog **ya están preparados**. Sigue únicamente estos pasos en **`172.23.174.103` (`BAINTS604`)**:

### Paso 1 — Verificar o compilar `dragon.exe` en `172.23.174.103`
En **PowerShell como Administrador** (si aún no has instalado Rust en `.103`, propaga primero el proxy institucional):

```powershell
$env:HTTPS_PROXY = "http://proxy_wsa.ejc.ejercito.mil.co:3128"
$env:HTTP_PROXY  = "http://proxy_wsa.ejc.ejercito.mil.co:3128"
$env:CARGO_HTTP_CHECK_REVOKE = "false"

# Solo si falta rustup (si ya corriste vs_buildtools.exe, continúa con rustup-init.exe):
& "$env:TEMP\rustup-init.exe" -y --default-toolchain stable --default-host x86_64-pc-windows-msvc

# Compilar edición estándar .\dragon.exe
cd "D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL"
.\compilador.ps1 -SoloEstandar
```

*(Alternativa sin compilar en el servidor: compila `dragon.exe` en tu PC con Rust y cópialo en `D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL\dragon.exe`).*

### Paso 2 — Revisar `config\site_config.json` en `172.23.174.103`
El archivo `D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL\config\site_config.json` ya está preconfigurado con nuestra red:

```json
{
  "port": 8092,
  "lan_mode": "local_agent",
  "authorized_subnet": "172.23.174.0/24",
  "primary_host": "172.23.174.103",
  "proxy_host": "172.23.174.11",
  "shared_paths": [],
  "libreoffice_path": ""
}
```
- Si deseas que los usuarios de la LAN busquen en carpetas compartidas del servidor sin instalar agente local, cambia `"lan_mode"` a `"shared_server"` y agrega las rutas en `"shared_paths"`.

### Paso 3 — Instalar y activar el servicio `Dragon-Radar-LAN` en `172.23.174.103`
Haz doble clic (o ejecuta en PowerShell como **Administrador**) sobre:

```powershell
cd "D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL"
.\deploy\INSTALAR_SERVICIO_103.ps1
```
*(o doble clic en `deploy\INSTALAR_SERVICIO_103.bat`).*

Este paso realiza automáticamente:
1. Verifica que se ejecuta en `172.23.174.103` (`BAINTS604`) y que existe `.\dragon.exe`.
2. Crea/habilita la regla de Firewall **`Dragon-Radar-LAN-8092`** (TCP `8092`, subred `172.23.174.0/24`, perfiles `Domain,Private`).
3. Sincroniza hacia `D:\script` los archivos actualizados (`config\replica-11.json`, `reiniciar-radar.ps1`, `ACTIVAR_PROXY_11_HACIA_103.ps1`, `Monitor-Recuperacion-LAN.ps1` y `proxy-tls\Caddyfile.*`).
4. Registra e inicia la tarea programada `NT AUTHORITY\SYSTEM` **`Dragon-Radar-LAN`** (arranque automático con Windows sin sesión de usuario, puerto `8092`).
5. Verifica salud en `http://172.23.174.103:8092/health`.

### Paso 4 — Verificar acceso por Principal (`.103`) y Proxy (`.11`)
En `172.23.174.11` (`BAINTS601`), la copia espejo (`\\172.23.174.11\proyectos ia independientes\RADAR-DRAGON-TOOL`) y la configuración del proxy (`_OPERACION\PROXY_103\`) ya están desplegadas. El watchdog `SYSTEM` de `.11` (`ProyectosIA-Watchdog-Proxy-LAN`, cada 5 min) aplica automáticamente `netsh portproxy 172.23.174.11:8092 -> 172.23.174.103:8092` y agrega el puerto `8092` al firewall `ProyectosIA-Proxy-103-LAN`.

Comprueba ambos accesos desde PowerShell o el navegador:
```powershell
Invoke-RestMethod "http://172.23.174.103:8092/health"
Invoke-RestMethod "http://172.23.174.11:8092/health"
```
- **Acceso directo principal**: `http://172.23.174.103:8092/`
- **Acceso vía proxy `.11`**: `http://172.23.174.11:8092/`

> **Regla anti-split-brain**: Nunca iniciar `dragon.exe` ni tareas locales en `172.23.174.11` mientras `172.23.174.103` sea el servidor principal.

### Paso 5 — Operación, LibreOffice en Red y Validación Mensual de Credencial
- **Validación e instalación automática de LibreOffice (`LibreOffice_26.8.1_Win_x86-64.msi`) en equipos de la LAN**:
  - Al activar Radar del Dragón en otro equipo de la red mediante `Activar_Radar_Local.bat` (o `INSTALAR.bat -ServerUrl http://172.23.174.103:8092`), se valida automáticamente si LibreOffice está instalado; si falta, descarga/copia `LibreOffice_26.8.1_Win_x86-64.msi` desde `/api/agent/libreoffice-installer` y lanza la instalación silenciosa (`msiexec /qn /norestart`) con elevación UAC de Windows *(sin exponer nunca contraseñas de administrador en texto plano hacia los clientes)*.
  - **Guardar credencial cifrada (DPAPI) y activar validación mensual en `172.23.174.103`** (ejecutar una vez):
    ```powershell
    cd "D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL"
    .\deploy\gestionar-credencial-admin.ps1 -Username "EJC_NACIONAL\1089196342" -InstalarTareaMensual
    ```
    Esto guarda la clave cifrada con Windows DPAPI en `%LOCALAPPDATA%\DragonRadar\seguridad\admin-libreoffice.cred.xml` (con ACL restringida) y registra la tarea mensual **`Dragon-Radar-ValidarCredencial-Mensual`** (día 1 de cada mes a las 08:00) que valida contra Active Directory si la clave cambió y alerta para actualizarla con `.\deploy\gestionar-credencial-admin.ps1 -Actualizar`.
  - **Despliegue remoto silencioso de LibreOffice hacia cualquier equipo de la LAN desde `.103`**:
    ```powershell
    .\deploy\instalar-libreoffice-red.ps1 -ComputerName "NOMBRE_O_IP_EQUIPO" -UseStoredCredential
    ```
- **Reiniciar solo Radar del Dragón en `.103`** (PowerShell Administrador):
  ```powershell
  & "D:\script\reiniciar-radar.ps1"
  ```
- **Validar salud global de todas las plataformas (incluyendo Radar `8092`)**:
  ```powershell
  & "D:\script\Monitor-Recuperacion-LAN.ps1" -Action HealthCheck
  ```
- **Generar el instalador offline ZIP para equipos cliente de la LAN** (si se desea distribución local):
  ```powershell
  cd "D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL"
  .\deploy\build-offline-package.ps1 -UseExistingBinary .\dragon.exe -Force
  ```

---

## 6. Solución de Problemas Frecuentes

| Síntoma / Error | Causa | Solución |
| --- | --- | --- |
| `winget: 0x8a15000f Data required by the source is missing` | Caché local de `winget` dañado o bloqueado por red. | No uses `winget`: ejecuta `configurar_primera_vez.ps1` o descarga directo con `Invoke-WebRequest` (`aka.ms/vs/17/release/vs_buildtools.exe` y `win.rustup.rs/x86_64`). |
| `rustup-init: tcp connect error (os error 10060)` | Red detrás de proxy corporativo/PAC; `rustup`/`cargo` no leen el PAC de Windows. | Define `$env:HTTPS_PROXY`, `$env:HTTP_PROXY` y `$env:CARGO_HTTP_CHECK_REVOKE = "false"` antes de ejecutar `rustup-init.exe`. `compilador.ps1` y `configurar_primera_vez.ps1` ya lo propagan automáticamente. |
| `No se encontro 'cargo' en el PATH` | Rust se instaló en `%USERPROFILE%\.cargo\bin` pero la sesión actual no refrescó `PATH` (o se instaló bajo otra cuenta Admin). | Ejecuta `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"` o abre PowerShell con el mismo usuario con que instalaste Rust. |
| `link.exe not found` al compilar | Faltan Microsoft C++ Build Tools (`VCTools`). | Ejecuta el Paso 1 (instalación de `vs_buildtools.exe`) y reinicia la consola. |
| `Otro PC de la LAN no conecta al puerto 8092` | Firewall de Windows bloqueando TCP `8092` o IP cliente fuera de `authorized_subnet`. | Ejecuta `.\deploy\configure-firewall.ps1 -Apply` como Administrador y verifica `authorized_subnet` en `config\site_config.json`. |
