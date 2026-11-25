param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Radar-del-Dragon"),
    [string]$ServerUrl = "",
    [switch]$NoShortcuts,
    [switch]$OmitirLibreOffice
)
$ErrorActionPreference = "Stop"
$package = $PSScriptRoot
$payload = Join-Path $package "Dragon-Radar"
$appScripts = Join-Path $package "app"
$startMenu = Join-Path ([Environment]::GetFolderPath("Programs")) "Radar del Dragon"
$desktopLink = Join-Path ([Environment]::GetFolderPath("Desktop")) "Radar del Dragon.lnk"

function Assert-Checksums([string]$Folder, [string]$ListName) {
    $list = Join-Path $Folder $ListName
    if (-not (Test-Path -LiteralPath $list -PathType Leaf)) { throw "Paquete incompleto: falta $list" }
    $count = 0
    foreach ($line in Get-Content -LiteralPath $list) {
        if (-not $line.Trim()) { continue }
        $parts = $line -split "  ", 2
        if ($parts.Count -ne 2 -or $parts[1] -match '(^|[\\/])\.\.([\\/]|$)' -or [IO.Path]::IsPathRooted($parts[1])) {
            throw "Lista de integridad no valida: $line"
        }
        $file = Join-Path $Folder $parts[1]
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Falta archivo del paquete: $($parts[1])" }
        if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $parts[0]) {
            throw "Archivo alterado o danado: $($parts[1]). Vuelva a copiar el ZIP completo."
        }
        $count++
    }
    return $count
}

function New-Shortcut([string]$Path, [string]$Arguments, [string]$Description) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($Path)
    $link.TargetPath = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
    $link.Arguments = $Arguments
    $link.WorkingDirectory = $InstallDir
    $link.IconLocation = (Join-Path $InstallDir "dragon.exe") + ",0"
    $link.Description = $Description
    $link.Save()
}

Write-Host "=== Instalador offline de Radar del Dragon ===" -ForegroundColor Cyan
if (-not [Environment]::Is64BitOperatingSystem) { throw "Radar del Dragon requiere Windows de 64 bits (x64)." }
if ([Environment]::OSVersion.Version.Major -lt 10) { throw "Radar del Dragon requiere Windows 10 u 11." }
if (-not (Test-Path -LiteralPath (Join-Path $payload "dragon.exe") -PathType Leaf)) {
    throw "Paquete incompleto: extraiga TODO el ZIP en una carpeta antes de ejecutar INSTALAR."
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir).TrimEnd("\")
# Get-Item expande nombres cortos 8.3 (p. ej. %TEMP%) para comparar con la ruta de los procesos.
if (Test-Path -LiteralPath $InstallDir) { $InstallDir = (Get-Item -LiteralPath $InstallDir).FullName.TrimEnd("\") }
if ($InstallDir.StartsWith((Get-Item -LiteralPath $package).FullName.TrimEnd("\"), [StringComparison]::OrdinalIgnoreCase)) {
    throw "Elija una carpeta de instalacion fuera del paquete extraido."
}

Write-Host "Verificando integridad del paquete..."
$verified = (Assert-Checksums $payload "SHA256SUMS.txt") + (Assert-Checksums $appScripts "APP-SHA256SUMS.txt")
Write-Host "Integridad correcta: $verified archivos." -ForegroundColor Green

$running = @(Get-Process -Name dragon -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and $_.Path.StartsWith($InstallDir + "\", [StringComparison]::OrdinalIgnoreCase)
})
if ($running.Count) {
    throw "Radar del Dragon esta en ejecucion desde $InstallDir. Cierre su ventana de servidor y repita la instalacion."
}

if (Test-Path -LiteralPath $InstallDir) {
    $existing = @(Get-ChildItem -LiteralPath $InstallDir -Force)
    # Solo se reemplaza una instalacion previa de Radar del Dragon o una carpeta vacia.
    if ($existing.Count -and -not (Test-Path -LiteralPath (Join-Path $InstallDir "dragon.exe"))) {
        throw "La carpeta $InstallDir ya existe y no es una instalacion de Radar del Dragon. Indique otra con -InstallDir."
    }
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$staging = "$InstallDir.nuevo-$stamp"
$previous = "$InstallDir.anterior-$stamp"
Write-Host "Copiando archivos a $InstallDir ..."
New-Item -ItemType Directory -Path $staging | Out-Null
try {
    Copy-Item -Path (Join-Path $payload "*") -Destination $staging -Recurse -Force
    Copy-Item -Path (Join-Path $appScripts "*") -Destination $staging -Recurse -Force
    foreach ($text in @("LEEME.txt", "COMPLEMENTOS_ADICIONALES.txt")) {
        Copy-Item -LiteralPath (Join-Path $package $text) -Destination $staging -Force
    }
    $stagingConfigDir = Join-Path $staging "config"
    if (-not (Test-Path -LiteralPath $stagingConfigDir)) {
        New-Item -ItemType Directory -Path $stagingConfigDir | Out-Null
    }
    if (Test-Path -LiteralPath $InstallDir) {
        # Actualizacion: conservar configuracion local e indices previos del equipo.
        $oldConfig = Join-Path $InstallDir "config\site_config.json"
        if (Test-Path -LiteralPath $oldConfig) {
            Copy-Item -LiteralPath $oldConfig -Destination $stagingConfigDir -Force
        }
        foreach ($dataDir in @(".dragon-data", ".dragon-index", ".dragon-shared-data", ".dragon-shared-index", ".dragon-models")) {
            $oldDir = Join-Path $InstallDir $dataDir
            if (Test-Path -LiteralPath $oldDir) {
                Copy-Item -LiteralPath $oldDir -Destination $staging -Recurse -Force
            }
        }
        Rename-Item -LiteralPath $InstallDir -NewName (Split-Path -Leaf $previous)
    }
    # Si es instalacion limpia, crear config\site_config.json a partir del ejemplo.
    $destConfig = Join-Path $stagingConfigDir "site_config.json"
    $exampleConfig = Join-Path $stagingConfigDir "site_config.example.json"
    if (-not (Test-Path -LiteralPath $destConfig) -and (Test-Path -LiteralPath $exampleConfig)) {
        Copy-Item -LiteralPath $exampleConfig -Destination $destConfig -Force
    }
    Rename-Item -LiteralPath $staging -NewName (Split-Path -Leaf $InstallDir)
}
catch {
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue }
    if ((Test-Path -LiteralPath $previous) -and -not (Test-Path -LiteralPath $InstallDir)) {
        Rename-Item -LiteralPath $previous -NewName (Split-Path -Leaf $InstallDir)
    }
    throw
}
if (Test-Path -LiteralPath $previous) { Remove-Item -LiteralPath $previous -Recurse -Force -ErrorAction SilentlyContinue }
# Evita bloqueos de "archivo descargado de Internet" heredados del ZIP.
Get-ChildItem -LiteralPath $InstallDir -Recurse -File | Unblock-File -ErrorAction SilentlyContinue

# Regla de validacion de LibreOffice al activar/instalar en otro equipo de la red
if (-not $OmitirLibreOffice) {
    $loInstalled = @(
        (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.com"),
        (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.exe"),
        (Join-Path "${env:ProgramFiles(x86)}" "LibreOffice\program\soffice.com"),
        (Join-Path $env:LOCALAPPDATA "Programs\LibreOffice\program\soffice.com")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1

    if ($loInstalled) {
        Write-Host "LibreOffice detectado en este equipo: $loInstalled" -ForegroundColor Green
    } else {
        $localMsi = Get-ChildItem -LiteralPath $package -Filter "LibreOffice*.msi" -File -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
        $tmpMsi = ""
        try {
            if ($localMsi) {
                $tmpDir = Join-Path $env:TEMP "DragonRadar"
                New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null
                $tmpMsi = Join-Path $tmpDir $localMsi.Name
                Write-Host "LibreOffice no detectado. Copiando $($localMsi.Name) ..." -ForegroundColor Yellow
                Copy-Item -LiteralPath $localMsi.FullName -Destination $tmpMsi -Force
            } elseif (-not [string]::IsNullOrWhiteSpace($ServerUrl)) {
                $base = $ServerUrl.TrimEnd('/')
                $info = Invoke-RestMethod -Uri "$base/api/agent/libreoffice-info" -TimeoutSec 5
                if ($info.available) {
                    $tmpDir = Join-Path $env:TEMP "DragonRadar"
                    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null
                    $tmpMsi = Join-Path $tmpDir $info.filename
                    Write-Host "LibreOffice no detectado. Descargando $($info.filename) desde $base ..." -ForegroundColor Yellow
                    $ProgressPreference = 'SilentlyContinue'
                    Invoke-WebRequest -Uri "$base/api/agent/libreoffice-installer" -OutFile $tmpMsi -UseBasicParsing
                }
            }
            if ($tmpMsi -and (Test-Path -LiteralPath $tmpMsi -PathType Leaf)) {
                Write-Host "Instalando LibreOffice (se solicitara autorizacion de Administrador UAC)..." -ForegroundColor Cyan
                $p = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$tmpMsi`" /qn /norestart" -Verb RunAs -Wait -PassThru
                if ($p.ExitCode -in @(0, 3010)) {
                    Write-Host "LibreOffice instalado correctamente." -ForegroundColor Green
                } else {
                    Write-Warning "La instalacion de LibreOffice finalizo con codigo $($p.ExitCode)."
                }
            }
        } catch {
            Write-Warning "No se pudo instalar LibreOffice automaticamente: $($_.Exception.Message)"
        } finally {
            if ($tmpMsi -and (Test-Path -LiteralPath $tmpMsi)) {
                Remove-Item -LiteralPath $tmpMsi -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

if (-not $NoShortcuts) {
    New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
    $launcher = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $InstallDir "Iniciar-Radar.ps1") + '"'
    New-Shortcut (Join-Path $startMenu "Radar del Dragon.lnk") $launcher "Iniciar Radar del Dragon en este equipo (puerto 8092)"
    New-Shortcut (Join-Path $startMenu "Radar del Dragon (compartir en LAN).lnk") "$launcher -Lan" "Iniciar Radar del Dragon accesible desde la red local (puerto 8092)"
    New-Shortcut (Join-Path $startMenu "Desinstalar Radar del Dragon.lnk") ('-NoProfile -ExecutionPolicy Bypass -File "' + (Join-Path $InstallDir "Desinstalar-Radar.ps1") + '"') "Desinstalar Radar del Dragon"
    New-Shortcut $desktopLink $launcher "Iniciar Radar del Dragon en este equipo (puerto 8092)"
    Write-Host "Accesos directos creados en el Escritorio y en el menu Inicio > Radar del Dragon."
}

Write-Host ""
Write-Host "Radar del Dragon instalado en: $InstallDir" -ForegroundColor Green
Write-Host "Use el acceso directo 'Radar del Dragon'. Se abrira http://127.0.0.1:8092/ en el navegador."
Write-Host "Para compartir en LAN use 'Radar del Dragon (compartir en LAN)' y revise config\site_config.json."
Write-Host "Para vista previa PDF exacta de Office instale LibreOffice (ver COMPLEMENTOS_ADICIONALES.txt)."
