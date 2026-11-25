<#
.SYNOPSIS
    Configurador general de primera vez para Radar del Dragon (equipos externos Windows x64).
    Verifica y descarga automaticamente solo las dependencias faltantes (MSVC C++ Build Tools y
    Rust Toolchain), prepara config\site_config.json y compila dragon.exe (edicion estandar)
    para ejecutar en Modo Local (sin depender de PORTABLE) y Modo Servidor LAN.

.EXAMPLE
    .\configurar_primera_vez.ps1
    Instala dependencias faltantes, crea config\site_config.json (puerto 8092) y compila .\dragon.exe.

.EXAMPLE
    .\configurar_primera_vez.ps1 -Port 8092 -LanMode shared_server -SharedPaths @("D:\Compartidos") -AuthorizedSubnet "192.168.1.0/24" -ConfigurarFirewall
    Instala dependencias, configura modo servidor compartido LAN, compila .\dragon.exe y abre Firewall.

.EXAMPLE
    .\configurar_primera_vez.ps1 -GenerarPaqueteOffline
    Ademas de compilar .\dragon.exe, construye el paquete instalador ZIP en .\dist\.

.EXAMPLE
    .\configurar_primera_vez.ps1 -ProxyUrl "http://proxy.empresa.local:3128"
    Usa un proxy HTTP/HTTPS explicito para descargar Build Tools, Rust y crates.
#>

[CmdletBinding()]
param(
    [ValidateRange(1, 65535)][int]$Port = 8092,
    [ValidateSet("local_agent", "shared_server")][string]$LanMode = "local_agent",
    [string]$AuthorizedSubnet = "",
    [string[]]$SharedPaths = @(),
    [string]$ProxyUrl = "",
    [switch]$ConfigurarFirewall,
    [switch]$GenerarPaqueteOffline,
    [switch]$SkipBuild,
    [switch]$SkipTests,
    [switch]$ForzarConfig
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

if ($PSScriptRoot) {
    Set-Location -LiteralPath $PSScriptRoot
}
$root = (Get-Location).Path

Write-Host "+======================================================================+" -ForegroundColor Cyan
Write-Host "|      CONFIGURACION DE PRIMERA VEZ - RADAR DEL DRAGON (WINDOWS x64)   |" -ForegroundColor Cyan
Write-Host "|      Dependencias + Compilacion Estandar (Modo Local y Modo LAN)     |" -ForegroundColor Cyan
Write-Host "+======================================================================+" -ForegroundColor Cyan
Write-Host ""

# ------------------------------------------------------------------------------
# 0. Validaciones de sistema y privilegios
# ------------------------------------------------------------------------------
if (-not [Environment]::Is64BitOperatingSystem) {
    throw "Radar del Dragon requiere Windows de 64 bits (x64)."
}
if ([Environment]::OSVersion.Version.Major -lt 10) {
    throw "Radar del Dragon requiere Windows 10, Windows 11 o Windows Server 2016+."
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Write-Host ("Equipo: {0} | Usuario: {1} | Administrador: {2}" -f $env:COMPUTERNAME, $identity.Name, $(if ($isAdmin) { "SI" } else { "NO" })) -ForegroundColor Gray

# ------------------------------------------------------------------------------
# 1. Configuracion de TLS 1.2 y deteccion automatica de Proxy Windows (PAC/WinINet)
# ------------------------------------------------------------------------------
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
} catch {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

if (-not [string]::IsNullOrWhiteSpace($ProxyUrl)) {
    $cleanProxy = $ProxyUrl.Trim().TrimEnd('/')
    $env:HTTPS_PROXY = $cleanProxy
    $env:HTTP_PROXY  = $cleanProxy
    $env:NO_PROXY    = "127.0.0.1,localhost"
    $env:CARGO_HTTP_CHECK_REVOKE = "false"
    Write-Host "[PROXY] Usando proxy explicito: $cleanProxy" -ForegroundColor Yellow
} elseif (-not $env:HTTPS_PROXY) {
    try {
        $probeUri = [Uri]"https://static.rust-lang.org/"
        $sysProxy = [System.Net.WebRequest]::GetSystemWebProxy().GetProxy($probeUri)
        if ($sysProxy -and ($sysProxy.AbsoluteUri -ne $probeUri.AbsoluteUri)) {
            $detectedProxy = $sysProxy.AbsoluteUri.TrimEnd('/')
            $env:HTTPS_PROXY = $detectedProxy
            $env:HTTP_PROXY  = $detectedProxy
            $env:NO_PROXY    = "127.0.0.1,localhost"
            $env:CARGO_HTTP_CHECK_REVOKE = "false"
            Write-Host "[PROXY] Proxy de Windows detectado y propagado a Rust/Cargo: $detectedProxy" -ForegroundColor Yellow
        }
    } catch {}
}

function Invoke-SafeDownload {
    param(
        [Parameter(Mandatory=$true)][string]$Url,
        [Parameter(Mandatory=$true)][string]$OutFile
    )
    $params = @{
        Uri             = $Url
        OutFile         = $OutFile
        UseBasicParsing = $true
        TimeoutSec      = 120
    }
    if (-not [string]::IsNullOrWhiteSpace($ProxyUrl)) {
        $params["Proxy"] = $ProxyUrl.Trim()
        $params["ProxyUseDefaultCredentials"] = $true
    }
    Invoke-WebRequest @params
}

function Test-MsvcInstalled {
    if (Get-Command cl.exe -ErrorAction SilentlyContinue) {
        return $true
    }
    $vswhere = Join-Path "${env:ProgramFiles(x86)}" "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if (-not [string]::IsNullOrWhiteSpace($found)) {
            return $true
        }
    }
    foreach ($base in @("${env:ProgramFiles(x86)}\Microsoft Visual Studio", "${env:ProgramFiles}\Microsoft Visual Studio")) {
        if (Test-Path -LiteralPath $base) {
            $cl = Get-ChildItem -Path $base -Filter "cl.exe" -Recurse -Depth 8 -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($cl) { return $true }
        }
    }
    return $false
}

function Find-CargoBinDir {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) { return (Split-Path -Parent $cmd.Source) }
    $userCargo = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path -LiteralPath (Join-Path $userCargo "cargo.exe") -PathType Leaf) {
        return $userCargo
    }
    return $null
}

# ------------------------------------------------------------------------------
# 2. Verificar / Instalar Microsoft C++ Build Tools (MSVC - requerido por Rust)
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "[1/6] Verificando Microsoft C++ Build Tools (MSVC)..." -ForegroundColor Cyan
if (Test-MsvcInstalled) {
    Write-Host "      OK: Herramientas C++ MSVC detectadas." -ForegroundColor Green
} else {
    Write-Host "      Faltan Microsoft C++ Build Tools (requeridas por zstd-sys y enlace MSVC)." -ForegroundColor Yellow
    if (-not $isAdmin) {
        throw "Para instalar Microsoft C++ Build Tools por primera vez se requieren permisos de Administrador. Ejecute CONFIGURAR_PRIMERA_VEZ.bat o abra PowerShell como Administrador."
    }
    $btInstaller = Join-Path $env:TEMP "vs_buildtools_radar.exe"
    Write-Host "      Descargando vs_buildtools.exe desde Microsoft..." -ForegroundColor Cyan
    Invoke-SafeDownload -Url "https://aka.ms/vs/17/release/vs_buildtools.exe" -OutFile $btInstaller
    Write-Host "      Instalando carga de trabajo VCTools (esto puede tardar varios minutos)..." -ForegroundColor Cyan
    $proc = Start-Process -FilePath $btInstaller `
        -ArgumentList "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" `
        -Wait -PassThru
    if ($proc.ExitCode -notin @(0, 3010)) {
        throw "La instalacion de Microsoft C++ Build Tools finalizo con codigo $($proc.ExitCode)."
    }
    if ($proc.ExitCode -eq 3010) {
        Write-Warning "Build Tools instalado (codigo 3010: Windows recomienda reiniciar al finalizar)."
    }
    if (-not (Test-MsvcInstalled)) {
        Write-Warning "No se detecto cl.exe inmediatamente tras la instalacion; si la compilacion falla, reinicie el equipo y vuelva a ejecutar .\compilador.ps1."
    } else {
        Write-Host "      OK: Microsoft C++ Build Tools instalados correctamente." -ForegroundColor Green
    }
}

# ------------------------------------------------------------------------------
# 3. Verificar / Instalar Rust Toolchain (rustup + cargo + rustc)
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "[2/6] Verificando Rust Toolchain (rustup / cargo / rustc)..." -ForegroundColor Cyan
$cargoBin = Find-CargoBinDir
if ($cargoBin) {
    if ($env:PATH -notlike "*$cargoBin*") {
        $env:PATH = "$cargoBin;$env:PATH"
    }
    Write-Host ("      OK: Rust encontrado en {0} ({1})" -f $cargoBin, (& rustc --version)) -ForegroundColor Green
} else {
    $rustupExe = Join-Path $env:TEMP "rustup-init.exe"
    Write-Host "      Descargando rustup-init.exe desde https://win.rustup.rs/x86_64 ..." -ForegroundColor Cyan
    Invoke-SafeDownload -Url "https://win.rustup.rs/x86_64" -OutFile $rustupExe
    Write-Host "      Instalando toolchain stable (x86_64-pc-windows-msvc)..." -ForegroundColor Cyan
    & $rustupExe -y --default-toolchain stable --default-host x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {
        throw "Fallo rustup-init.exe (codigo $LASTEXITCODE). Si usa proxy con autenticacion, pase -ProxyUrl 'http://usuario:clave@proxy:puerto'."
    }
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path -LiteralPath (Join-Path $cargoBin "cargo.exe") -PathType Leaf) {
        $env:PATH = "$cargoBin;$env:PATH"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "Rust se instalo pero 'cargo' no aparece en $cargoBin. Cierre y reabra PowerShell."
    }
    Write-Host ("      OK: Rust instalado -> {0} | {1}" -f (& rustc --version), (& cargo --version)) -ForegroundColor Green
}

# ------------------------------------------------------------------------------
# 4. Verificar LibreOffice (opcional para vista previa PDF exacta de Office)
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "[3/6] Verificando LibreOffice (opcional)..." -ForegroundColor Cyan
$loCandidates = @(
    (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.com"),
    (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.exe"),
    (Join-Path "${env:ProgramFiles(x86)}" "LibreOffice\program\soffice.com"),
    (Join-Path $env:LOCALAPPDATA "Programs\LibreOffice\program\soffice.com")
)
$loFound = $loCandidates | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
if (-not $loFound) {
    $soffCmd = Get-Command soffice.com, soffice.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($soffCmd) { $loFound = $soffCmd.Source }
}
if ($loFound) {
    Write-Host "      OK: LibreOffice detectado en $loFound" -ForegroundColor Green
} else {
    Write-Host "      AVISO: LibreOffice no esta instalado (opcional). El visor OOXML integrado funcionara normalmente." -ForegroundColor Yellow
}

# ------------------------------------------------------------------------------
# 5. Generar / Validar config\site_config.json
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "[4/6] Preparando config\site_config.json..." -ForegroundColor Cyan
$configDir = Join-Path $root "config"
$configFile = Join-Path $configDir "site_config.json"
if (-not (Test-Path -LiteralPath $configDir)) {
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
}

if ((Test-Path -LiteralPath $configFile -PathType Leaf) -and (-not $ForzarConfig)) {
    Write-Host "      OK: config\site_config.json ya existe y se conserva intacto (use -ForzarConfig para regenerar)." -ForegroundColor Green
} else {
    $cleanShared = @($SharedPaths | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $cfgObj = [ordered]@{
        port              = $Port
        lan_mode          = $LanMode
        authorized_subnet = $AuthorizedSubnet
        shared_paths      = $cleanShared
        libreoffice_path  = ""
    }
    $json = $cfgObj | ConvertTo-Json -Depth 4
    [System.IO.File]::WriteAllText($configFile, $json + "`r`n", [System.Text.UTF8Encoding]::new($false))
    Write-Host "      OK: config\site_config.json creado (puerto=$Port, lan_mode=$LanMode)." -ForegroundColor Green
}

# ------------------------------------------------------------------------------
# 6. Compilar dragon.exe (edicion estandar: Modo Local + Modo LAN, sin depender de PORTABLE)
# ------------------------------------------------------------------------------
Write-Host ""
if (-not $SkipBuild) {
    Write-Host "[5/6] Compilando .\dragon.exe (edicion estandar con soporte Local y LAN)..." -ForegroundColor Cyan
    $compArgs = @("-SoloEstandar", "-KillRunning")
    if ($SkipTests) { $compArgs += "-SkipTests" }
    & (Join-Path $root "compilador.ps1") @compArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Fallo la compilacion de .\dragon.exe."
    }

    if ($GenerarPaqueteOffline) {
        Write-Host "      Construyendo paquete instalador offline ZIP en .\dist\..." -ForegroundColor Cyan
        $pkgArgs = @("-UseExistingBinary", (Join-Path $root "dragon.exe"), "-Force")
        if ($SkipTests) { $pkgArgs += "-SkipTests" }
        & (Join-Path $root "deploy\build-offline-package.ps1") @pkgArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Fallo la generacion del paquete instalador offline."
        }
    }
} else {
    Write-Host "[5/6] Compilacion omitida por -SkipBuild." -ForegroundColor DarkGray
}

# ------------------------------------------------------------------------------
# 7. Regla de Firewall LAN opcional
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "[6/6] Verificacion de Firewall LAN..." -ForegroundColor Cyan
if ($ConfigurarFirewall) {
    $fwScript = Join-Path $root "deploy\configure-firewall.ps1"
    if ([string]::IsNullOrWhiteSpace($AuthorizedSubnet)) {
        Write-Warning "Se indico -ConfigurarFirewall pero -AuthorizedSubnet esta vacio. Ejecute manualmente: .\deploy\configure-firewall.ps1 -AuthorizedSubnet <SUBRED/CIDR> -Port $Port -Apply"
    } elseif (-not $isAdmin) {
        Write-Warning "Para crear la regla de Firewall se requieren permisos de Administrador. Ejecute elevado: .\deploy\configure-firewall.ps1 -AuthorizedSubnet $AuthorizedSubnet -Port $Port -Apply"
    } else {
        $existingFw = Get-NetFirewallRule -Name "Dragon-Radar-LAN-$Port" -ErrorAction SilentlyContinue
        if ($existingFw) {
            Write-Host "      OK: La regla Dragon-Radar-LAN-$Port ya existe." -ForegroundColor Green
        } else {
            & $fwScript -AuthorizedSubnet $AuthorizedSubnet -Port $Port -Apply
        }
    }
} else {
    Write-Host "      Sin cambios en Firewall (use -ConfigurarFirewall -AuthorizedSubnet '<CIDR>' si compartira en LAN)." -ForegroundColor Gray
}

# ------------------------------------------------------------------------------
# Resumen final (ambas opciones listas)
# ------------------------------------------------------------------------------
Write-Host ""
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host "|             CONFIGURACION DE PRIMERA VEZ COMPLETADA                  |" -ForegroundColor Green
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host ""
Write-Host "  Ya dispones de AMBAS formas de ejecucion (sin depender de PORTABLE\):" -ForegroundColor White
Write-Host "    1) MODO SOLO LOCAL (este equipo, 127.0.0.1:$Port):" -ForegroundColor Cyan
Write-Host "         Doble clic en:  servidor local.bat" -ForegroundColor White
Write-Host "         o PowerShell:   .\deploy\run-lan.ps1 -LocalOnly -OpenBrowser" -ForegroundColor Gray
Write-Host "         Navegador:      http://127.0.0.1:$Port/" -ForegroundColor Gray
Write-Host ""
Write-Host "    2) MODO SERVIDOR LAN (red local, 0.0.0.0:$Port):" -ForegroundColor Cyan
Write-Host "         Doble clic en:  servidor para LAN.bat" -ForegroundColor White
Write-Host "         o PowerShell:   .\deploy\run-lan.ps1" -ForegroundColor Gray
Write-Host "         Configuracion:  config\site_config.json (lan_mode: local_agent o shared_server)" -ForegroundColor Gray
Write-Host "         Salud HTTP:     http://127.0.0.1:$Port/health" -ForegroundColor Gray
Write-Host ""
