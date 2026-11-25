<#
.SYNOPSIS
    Genera el paquete instalador offline autocontenido de Radar del Dragon para Windows x64,
    con estructura, verificacion SHA-256 y accesos directos (Local y Compartir en LAN)
    equivalentes al instalador offline de EVA PDF Enterprise.

.EXAMPLE
    .\deploy\build-offline-package.ps1
    Compila dragon.exe (con soporte LAN y CRT estatico), ejecuta pruebas y genera:
    dist\Radar-del-Dragon-Offline-1.3.0-win-x64.zip + .sha256.txt

.EXAMPLE
    .\deploy\build-offline-package.ps1 -Force -SkipTests
    Reemplaza una salida previa de la misma version omitiendo cargo test.

.EXAMPLE
    .\deploy\build-offline-package.ps1 -UseExistingBinary .\dragon.exe -Force
    Empaqueta un binario dragon.exe ya compilado (debe incluir feature lan).
#>
param(
    [string]$Version = "1.3.0",
    [string]$UseExistingBinary = "",
    [switch]$SkipTests,
    [switch]$Force
)
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
$kit = Join-Path $PSScriptRoot "offline"
$distDir = Join-Path $root "dist"
$name = "Radar-del-Dragon-Offline-$Version-win-x64"
$stage = Join-Path $distDir $name
$zip = "$stage.zip"
$zipHashFile = "$zip.sha256.txt"

Write-Host "+======================================================================+" -ForegroundColor Cyan
Write-Host "|      CONSTRUCTOR DE INSTALADOR OFFLINE - RADAR DEL DRAGON v$Version     |" -ForegroundColor Cyan
Write-Host "+======================================================================+" -ForegroundColor Cyan

foreach ($output in @($stage, $zip, $zipHashFile)) {
    if (Test-Path -LiteralPath $output) {
        if ($Force) {
            Remove-Item -LiteralPath $output -Recurse -Force
        } else {
            throw "La salida ya existe y no se sobrescribe: $output (use -Force para reemplazarla)."
        }
    }
}

$binaryPath = ""
if (-not [string]::IsNullOrWhiteSpace($UseExistingBinary)) {
    $resolved = Resolve-Path -LiteralPath $UseExistingBinary -ErrorAction Stop
    $binaryPath = $resolved.Path
    Write-Host "[1/4] Usando binario existente: $binaryPath" -ForegroundColor Yellow
} else {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if ((Test-Path -LiteralPath (Join-Path $cargoBin "cargo.exe")) -and ($env:PATH -notlike "*$cargoBin*")) {
        $env:PATH = "$cargoBin;$env:PATH"
    }
    if (-not $env:HTTPS_PROXY) {
        try {
            $probeUri = [Uri]"https://static.rust-lang.org/"
            $sysProxy = [System.Net.WebRequest]::GetSystemWebProxy().GetProxy($probeUri)
            if ($sysProxy -and ($sysProxy.AbsoluteUri -ne $probeUri.AbsoluteUri)) {
                $env:HTTPS_PROXY = $sysProxy.AbsoluteUri.TrimEnd('/')
                $env:HTTP_PROXY  = $env:HTTPS_PROXY
                $env:NO_PROXY    = "127.0.0.1,localhost"
            }
        } catch {}
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "No se encontro 'cargo' (Rust Toolchain) en el PATH ni en $cargoBin. Instale Rust (winget install Rustlang.Rustup) o indique -UseExistingBinary."
    }

    Push-Location $root
    try {
        if (-not $SkipTests) {
            if (Get-Command node -ErrorAction SilentlyContinue) {
                Write-Host "[1/4] Validando sintaxis de ui\radar.js..." -ForegroundColor Cyan
                & node --check "ui\radar.js"
                if ($LASTEXITCODE -ne 0) { throw "Fallo la validacion de sintaxis JS en ui\radar.js" }
            }
            Write-Host "[2/4] Ejecutando pruebas del workspace Rust..." -ForegroundColor Cyan
            & cargo test --workspace
            if ($LASTEXITCODE -ne 0) { throw "Fallo cargo test --workspace" }
        } else {
            Write-Host "[1-2/4] Pruebas omitidas por -SkipTests." -ForegroundColor DarkGray
        }

        Write-Host "[3/4] Compilando dragon.exe (release, con soporte LAN + CRT estatico)..." -ForegroundColor Cyan
        $prevRustFlags = $env:RUSTFLAGS
        try {
            if ([string]::IsNullOrWhiteSpace($prevRustFlags)) {
                $env:RUSTFLAGS = "-C target-feature=+crt-static"
            } else {
                $env:RUSTFLAGS = "$prevRustFlags -C target-feature=+crt-static"
            }
            & cargo build --release --bin dragon --target-dir "target\offline-lan"
            if ($LASTEXITCODE -ne 0) { throw "Fallo cargo build --release --bin dragon" }
        } finally {
            if ($null -eq $prevRustFlags) {
                Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
            } else {
                $env:RUSTFLAGS = $prevRustFlags
            }
        }
        $binaryPath = Join-Path $root "target\offline-lan\release\dragon.exe"
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "No se encontro el binario compilado en $binaryPath"
}

Write-Host "[4/4] Ensamblando paquete offline y generando firmas SHA-256..." -ForegroundColor Cyan
if (-not (Test-Path -LiteralPath $distDir)) {
    New-Item -ItemType Directory -Path $distDir | Out-Null
}

$payloadDir = Join-Path $stage "Dragon-Radar"
$payloadConfigDir = Join-Path $payloadDir "config"
$appDir = Join-Path $stage "app"
$toolsDir = Join-Path $appDir "herramientas"

New-Item -ItemType Directory -Path $payloadConfigDir -Force | Out-Null
New-Item -ItemType Directory -Path $toolsDir -Force | Out-Null

Copy-Item -LiteralPath $binaryPath -Destination (Join-Path $payloadDir "dragon.exe") -Force
Copy-Item -LiteralPath (Join-Path $root "config\site_config.example.json") -Destination $payloadConfigDir -Force

# Verificar que no se incluyan datos privados o bases locales en el payload
foreach ($forbidden in @(".dragon-data", ".dragon-index", ".dragon-shared-data", ".dragon-shared-index", "config\site_config.json")) {
    if (Test-Path -LiteralPath (Join-Path $payloadDir $forbidden)) {
        throw "Contenido local prohibido en el payload: $forbidden"
    }
}

Copy-Item -Path (Join-Path $kit "app\*") -Destination $appDir -Recurse -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "configure-firewall.ps1") -Destination $toolsDir -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "validar-index-al-abrir.ps1") -Destination $toolsDir -Force
foreach ($file in @("INSTALAR.bat", "INSTALAR.ps1", "LEEME.txt", "COMPLEMENTOS_ADICIONALES.txt")) {
    Copy-Item -LiteralPath (Join-Path $kit $file) -Destination $stage -Force
}

# Generar SHA256SUMS.txt del payload
Get-ChildItem -LiteralPath $payloadDir -File -Recurse | Sort-Object FullName | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash, $_.FullName.Substring($payloadDir.Length + 1)
} | Set-Content -LiteralPath (Join-Path $payloadDir "SHA256SUMS.txt") -Encoding UTF8

# Generar APP-SHA256SUMS.txt de scripts de aplicacion
Get-ChildItem -LiteralPath $appDir -File -Recurse | Sort-Object FullName | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash, $_.FullName.Substring($appDir.Length + 1)
} | Set-Content -LiteralPath (Join-Path $appDir "APP-SHA256SUMS.txt") -Encoding UTF8

$exeHash = (Get-FileHash -LiteralPath (Join-Path $payloadDir "dragon.exe") -Algorithm SHA256).Hash
$sumsHash = (Get-FileHash -LiteralPath (Join-Path $payloadDir "SHA256SUMS.txt") -Algorithm SHA256).Hash
@(
    "Radar del Dragon offline $Version (Windows x64)",
    "Puerto oficial LAN/Local: 8092 (respaldo 8093-8101)",
    "Modos LAN soportados: local_agent (predeterminado) y shared_server (config\site_config.json)",
    "Generado: $((Get-Date).ToString('yyyy-MM-dd HH:mm'))",
    "Runtime: Rust nativo x64 (+crt-static, sin dependencias externas)",
    "SHA256 dragon.exe:       $exeHash",
    "SHA256 SHA256SUMS.txt:   $sumsHash",
    "Binario sin firma Authenticode: verifique las huellas antes de distribuir."
) | Set-Content -LiteralPath (Join-Path $stage "PAQUETE-INFO.txt") -Encoding UTF8

Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [IO.Compression.CompressionLevel]::Optimal, $true)
$zipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
"$zipHash  $name.zip" | Set-Content -LiteralPath $zipHashFile -Encoding ASCII

$sizeMb = [Math]::Round((Get-Item -LiteralPath $zip).Length / 1MB, 2)
Write-Host ""
Write-Host "Paquete generado: $zip ($sizeMb MB)" -ForegroundColor Green
Write-Host "SHA256 ZIP:       $zipHash" -ForegroundColor Green
Write-Host "Archivo hash:     $zipHashFile" -ForegroundColor Gray
