<#
.SYNOPSIS
    Compilador oficial de Radar del Dragón (Dragon Radar Tool).
    Compila los cambios en dragon.exe (edición estándar) y genera la edición PORTABLE
    autónoma (sin red LAN y con runtime estático) en .\PORTABLE\dragon.exe.

.EXAMPLE
    .\compilador.ps1
    Compila tests, versión estándar .\dragon.exe, versión PORTABLE .\PORTABLE\dragon.exe
    y valida los límites de tamaño para GitHub Desktop.

.EXAMPLE
    .\compilador.ps1 -SoloEstandar -SkipTests
    Compila únicamente .\dragon.exe sin ejecutar pruebas previas.

.EXAMPLE
    .\compilador.ps1 -SoloPortable -KillRunning
    Cierra instancias activas de dragon.exe y compila solo .\PORTABLE\dragon.exe.
#>

[CmdletBinding()]
param(
    [switch]$SoloEstandar,
    [switch]$SoloPortable,
    [switch]$SkipTests,
    [switch]$KillRunning
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

if ($PSScriptRoot) {
    Set-Location $PSScriptRoot
}

$Host.UI.RawUI.WindowTitle = "Compilador - Radar del Dragón"

Write-Host "+======================================================================+" -ForegroundColor DarkYellow
Write-Host "|               COMPILADOR - RADAR DEL DRAGON TOOL                     |" -ForegroundColor Yellow
Write-Host "|      Build Release (dragon.exe) + Edicion PORTABLE (sin LAN)         |" -ForegroundColor Yellow
Write-Host "+======================================================================+" -ForegroundColor DarkYellow
Write-Host ""

if ($SoloEstandar -and $SoloPortable) {
    Write-Host "[ERROR] No puedes usar -SoloEstandar y -SoloPortable al mismo tiempo." -ForegroundColor Red
    exit 1
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "[ERROR] No se encontro 'cargo' (Rust Toolchain) en el PATH." -ForegroundColor Red
    exit 1
}

# 1. Verificar procesos dragon.exe activos que puedan bloquear el ejecutable
$runningDragon = Get-Process -Name "dragon" -ErrorAction SilentlyContinue
if ($runningDragon) {
    if ($KillRunning) {
        Write-Host "[*] Cerrando instancias activas de dragon.exe ($($runningDragon.Count))..." -ForegroundColor Yellow
        $runningDragon | Stop-Process -Force
        Start-Sleep -Milliseconds 600
    } else {
        Write-Host "[AVISO] Hay una instancia de dragon.exe en ejecucion (PID: $($runningDragon.Id -join ', '))." -ForegroundColor Yellow
        Write-Host "        Si falla al copiar el ejecutable, cierra la ventana del radar o usa -KillRunning." -ForegroundColor Yellow
    }
}

# 2. Pruebas y validacion previa (JS + Rust Workspace + Portable)
if (-not $SkipTests) {
    if (Get-Command node -ErrorAction SilentlyContinue) {
        Write-Host "[1/4] Validando sintaxis de ui\radar.js..." -ForegroundColor Cyan
        & node --check "ui\radar.js"
        if ($LASTEXITCODE -ne 0) { throw "Fallo la validacion de sintaxis JavaScript en ui\radar.js" }
    } else {
        Write-Host "[1/4] Node.js no encontrado; omitiendo node --check ui\radar.js." -ForegroundColor DarkGray
    }

    Write-Host "[2/4] Ejecutando suite de pruebas del workspace y perfil sin LAN..." -ForegroundColor Cyan
    & cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "Fallo cargo test --workspace" }

    & cargo test -p dragon-cli --no-default-features --target-dir "target\portable"
    if ($LASTEXITCODE -ne 0) { throw "Fallo cargo test -p dragon-cli --no-default-features" }
} else {
    Write-Host "[1-2/4] Pruebas omitidas por -SkipTests." -ForegroundColor DarkGray
}

# 3. Compilar edicion estandar -> .\dragon.exe
if (-not $SoloPortable) {
    Write-Host "[3/4] Compilando edicion estandar (release) -> .\dragon.exe..." -ForegroundColor Cyan
    & cargo build --release --bin dragon
    if ($LASTEXITCODE -ne 0) { throw "Fallo la compilacion estandar de dragon.exe" }

    $stdSrc = ".\target\release\dragon.exe"
    $stdDst = ".\dragon.exe"
    if (Test-Path $stdSrc) {
        Copy-Item -Path $stdSrc -Destination $stdDst -Force
        $sizeMb = [Math]::Round((Get-Item $stdDst).Length / 1MB, 2)
        Write-Host "      OK: .\dragon.exe actualizado ($sizeMb MB)" -ForegroundColor Green
    }
}

# 4. Compilar edicion PORTABLE (sin LAN y con CRT estatico) -> .\PORTABLE\dragon.exe
if (-not $SoloEstandar) {
    Write-Host "[4/4] Compilando edicion PORTABLE (sin LAN, CRT estatico) -> .\PORTABLE\dragon.exe..." -ForegroundColor Cyan
    $portableDir = ".\PORTABLE"
    if (-not (Test-Path $portableDir)) {
        New-Item -ItemType Directory -Path $portableDir | Out-Null
    }

    $prevRustFlags = $env:RUSTFLAGS
    try {
        if ([string]::IsNullOrWhiteSpace($prevRustFlags)) {
            $env:RUSTFLAGS = "-C target-feature=+crt-static"
        } else {
            $env:RUSTFLAGS = "$prevRustFlags -C target-feature=+crt-static"
        }

        & cargo build --release --bin dragon --no-default-features --target-dir "target\portable"
        if ($LASTEXITCODE -ne 0) { throw "Fallo la compilacion PORTABLE de dragon.exe" }
    } finally {
        if ($null -eq $prevRustFlags) {
            Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
        } else {
            $env:RUSTFLAGS = $prevRustFlags
        }
    }

    $portSrc = ".\target\portable\release\dragon.exe"
    $portDst = ".\PORTABLE\dragon.exe"
    if (Test-Path $portSrc) {
        Copy-Item -Path $portSrc -Destination $portDst -Force
        $sizeMb = [Math]::Round((Get-Item $portDst).Length / 1MB, 2)
        Write-Host "      OK: .\PORTABLE\dragon.exe generado ($sizeMb MB)" -ForegroundColor Green
    }
}

# 5. Validacion de limites para commit y push en GitHub Desktop
Write-Host ""
Write-Host "[GitHub Desktop] Validando pesos de archivos trackeables (limite GitHub: 100 MB, alerta: 50 MB)..." -ForegroundColor Cyan
$candidates = @()
if (Get-Command git -ErrorAction SilentlyContinue) {
    $tracked = & git ls-files
    $untracked = & git ls-files --others --exclude-standard
    $allPaths = ($tracked + $untracked) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Sort-Object -Unique
    foreach ($rel in $allPaths) {
        if (Test-Path $rel -PathType Leaf) {
            $candidates += Get-Item $rel
        }
    }
}

$overHardLimit = @($candidates | Where-Object { $_.Length -ge 100MB })
$overSoftWarn  = @($candidates | Where-Object { $_.Length -ge 50MB })
$totalMb = [Math]::Round((($candidates | Measure-Object -Property Length -Sum).Sum) / 1MB, 2)
$largest = $candidates | Sort-Object Length -Descending | Select-Object -First 5

Write-Host ("  Archivos listos para Git : {0} | Peso total conjunto: {1} MB" -f $candidates.Count, $totalMb) -ForegroundColor White
foreach ($f in $largest) {
    $mb = [Math]::Round($f.Length / 1MB, 2)
    Write-Host ("    - {0,-45} {1,7} MB" -f $f.Name, $mb) -ForegroundColor Gray
}

if ($overHardLimit.Count -gt 0) {
    Write-Host "[ERROR] Hay archivos >= 100 MB que GitHub rechazara al hacer push!" -ForegroundColor Red
    exit 1
} elseif ($overSoftWarn.Count -gt 0) {
    Write-Host "[AVISO] Hay archivos >= 50 MB (GitHub mostrara advertencia, aunque permite el push)." -ForegroundColor Yellow
} else {
    Write-Host "  OK: Todos los archivos estan por debajo de 50 MB (listo para commit y push en GitHub Desktop)." -ForegroundColor Green
}

Write-Host ""
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host "|                 COMPILACION COMPLETADA CORRECTAMENTE                 |" -ForegroundColor Green
Write-Host "+======================================================================+" -ForegroundColor Green
