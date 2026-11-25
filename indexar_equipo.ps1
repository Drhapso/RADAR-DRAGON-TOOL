<#
.SYNOPSIS
    Script de indexacion integral para Dragon Radar Tool.
    Detecta automaticamente todas las unidades locales (C:, D:, E:, etc.),
    omite directorios del sistema operativo y archivos protegidos, e indexa
    todos los documentos, archivos y codigo en los motores de busqueda (Tantivy, Redb y Vectores IA).

.DESCRIPTION
    Dragon Radar Tool - Motor Autonomo de Indexacion de Unidades Locales.
#>

[CmdletBinding()]
param(
    [switch]$SoloDocumentos,
    [string]$IndexDir = "",
    [string]$DbPath = "",
    [string]$VectorsFile = ""
)

# Asegurar codificacion UTF-8 para salida de dragon.exe y ubicarse en la raiz del proyecto
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
if ($PSScriptRoot) {
    Set-Location $PSScriptRoot
}

$isNetworkShare = $false
if ($PSScriptRoot -and ($PSScriptRoot.StartsWith("\\") -or ((Get-Location).ProviderPath.StartsWith("\\")))) {
    $isNetworkShare = $true
}
if (-not $IndexDir -or -not $DbPath -or -not $VectorsFile) {
    if ($isNetworkShare) {
        $localBase = Join-Path $env:LOCALAPPDATA "DragonRadar\$env:COMPUTERNAME"
        if (-not $IndexDir)    { $IndexDir = Join-Path $localBase "index" }
        if (-not $DbPath)      { $DbPath = Join-Path $localBase "data\radar.redb" }
        if (-not $VectorsFile) { $VectorsFile = Join-Path $localBase "data\vectors.bin" }
    } else {
        if (-not $IndexDir)    { $IndexDir = ".dragon-index" }
        if (-not $DbPath)      { $DbPath = ".dragon-data/radar.redb" }
        if (-not $VectorsFile) { $VectorsFile = ".dragon-data/vectors.bin" }
    }
}

$Host.UI.RawUI.WindowTitle = "Dragon Radar Tool - Indexador de Unidades Locales"
Clear-Host

Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host "|              DRAGON RADAR TOOL - INDEXADOR DE EQUIPO                 |" -ForegroundColor Green
Write-Host "|      Indexacion Integral de Unidades Locales (MFT, Redb y AI)        |" -ForegroundColor Green
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host ""

# 1. Localizar el ejecutable dragon.exe (priorizando compilacion release mas reciente)
$dragonExe = $null
if ((Test-Path ".\target\release\dragon.exe") -and (Test-Path ".\dragon.exe")) {
    $relInfo = Get-Item ".\target\release\dragon.exe"
    $rootInfo = Get-Item ".\dragon.exe"
    if ($relInfo.LastWriteTimeUtc -gt $rootInfo.LastWriteTimeUtc) {
        try {
            Copy-Item -Path $relInfo.FullName -Destination $rootInfo.FullName -Force -ErrorAction Stop
            $dragonExe = $rootInfo.FullName
        } catch {
            $dragonExe = $relInfo.FullName
        }
    } else {
        $dragonExe = $rootInfo.FullName
    }
} elseif (Test-Path ".\target\release\dragon.exe") {
    $dragonExe = (Resolve-Path ".\target\release\dragon.exe").Path
} elseif (Test-Path ".\dragon.exe") {
    $dragonExe = (Resolve-Path ".\dragon.exe").Path
} else {
    Write-Host "[ERROR] No se encontro dragon.exe. Por favor compila primero con 'cargo build --release'." -ForegroundColor Red
    Exit 1
}

Write-Host "[*] Binario detectado : $dragonExe" -ForegroundColor Cyan
Write-Host "[*] Base de datos     : $DbPath" -ForegroundColor Cyan
Write-Host "[*] Indice Tantivy    : $IndexDir" -ForegroundColor Cyan
Write-Host "[*] Almacen Vectorial : $VectorsFile" -ForegroundColor Cyan
Write-Host ""
Write-Host "Descubriendo unidades y carpetas del usuario (omitiendo sistema operativo)..." -ForegroundColor Yellow

# 2. Descubrir unidades locales y filtrar carpetas del SO
$drives = Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | Select-Object -ExpandProperty DeviceID
$targets = [System.Collections.Generic.List[string]]::new()

$systemExclusions = '^(Windows|Program Files|Program Files \(x86\)|ProgramData|\$Recycle\.Bin|System Volume Information|Recovery|Boot|PerfLogs|MSOCache|Config\.Msi|\.Trash.*|WindowsApps)$'

foreach ($d in $drives) {
    if ($d -eq "C:") {
        # Perfiles de usuario en C:\Users
        $userProfiles = Get-ChildItem "C:\Users" -Directory -ErrorAction SilentlyContinue | 
            Where-Object { $_.Name -notmatch '^(Default|Public|All Users|defaultuser.*)$' }

        foreach ($u in $userProfiles) {
            $userBaseFolders = @("Documents", "Desktop", "Downloads", "Pictures", "Videos", "Music")
            foreach ($sub in $userBaseFolders) {
                $p = Join-Path $u.FullName $sub
                if (Test-Path $p) { $targets.Add($p) }
            }

            if (-not $SoloDocumentos) {
                # Agregar carpetas de proyectos o datos en la raiz del usuario
                Get-ChildItem $u.FullName -Directory -ErrorAction SilentlyContinue | 
                    Where-Object {
                        $_.Name -notmatch '^(AppData|Application Data|Cookies|Local Settings|NetHood|PrintHood|Recent|SendTo|Start Menu|Templates|\..*)$' -and
                        $userBaseFolders -notcontains $_.Name
                    } | ForEach-Object { $targets.Add($_.FullName) }
            }
        }

        if (-not $SoloDocumentos) {
            # Carpetas no-SO en la raiz de C:\
            Get-ChildItem "C:\" -Directory -ErrorAction SilentlyContinue | 
                Where-Object { $_.Name -notmatch $systemExclusions -and $_.Name -ne "Users" } | 
                ForEach-Object { $targets.Add($_.FullName) }
        }
    } else {
        # Unidades secundarias (D:, E:, etc.): Todo excepto carpetas de sistema
        Get-ChildItem "$d\" -Directory -ErrorAction SilentlyContinue | 
            Where-Object { $_.Name -notmatch $systemExclusions } | 
            ForEach-Object { $targets.Add($_.FullName) }
    }
}

$totalTargets = $targets.Count
Write-Host "Se identificaron $totalTargets ubicaciones de datos locales para indexar." -ForegroundColor Green
Write-Host ""

$startTime = [System.Diagnostics.Stopwatch]::StartNew()
$currentIndex = 0

# 3. Indexar cada ubicacion
foreach ($target in $targets) {
    $currentIndex++
    Write-Host "----------------------------------------------------------------------" -ForegroundColor DarkGray
    Write-Host "[$currentIndex/$totalTargets] Indexando: $target" -ForegroundColor Cyan

    & $dragonExe index "$target" --index-dir "$IndexDir" --db-path "$DbPath" --vectors-file "$VectorsFile"
}

$startTime.Stop()
$totalElapsed = $startTime.Elapsed

Write-Host ""
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host "|              INDEXACION INTEGRAL FINALIZADA CON EXITO                |" -ForegroundColor Green
Write-Host "+======================================================================+" -ForegroundColor Green
$elapsedStr = $totalElapsed.ToString()
Write-Host "Tiempo total transcurrido: $elapsedStr" -ForegroundColor Yellow
Write-Host ""

# 4. Mostrar resumen consolidado
& $dragonExe info

Write-Host ""
Write-Host "Todas las unidades locales han sido indexadas." -ForegroundColor Green
Write-Host "Para buscar desde la consola ejecuta: .\dragon.exe hybrid [tu_consulta]" -ForegroundColor Cyan
Write-Host "Para abrir el radar interactivo ejecuta: .\dragon.exe ui (o doble clic en iniciar_radar.bat)" -ForegroundColor Cyan
Write-Host ""
