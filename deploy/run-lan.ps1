param(
    [ValidateRange(0,65535)][int]$Port = 0,
    [string]$HostAddress = "",
    [switch]$LocalOnly,
    [ValidateSet("", "local_agent", "shared_server")][string]$LanMode = "",
    [switch]$OpenBrowser
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $root "config\site_config.json"

$exeCandidates = @(
    (Join-Path $root "dragon.exe"),
    (Join-Path $root "target\release\dragon.exe"),
    (Join-Path $root "target\offline-lan\release\dragon.exe")
)
$exe = $exeCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $exe) {
    throw "No se encontro dragon.exe con soporte LAN. Compile primero con .\compilador.ps1 o .\deploy\build-offline-package.ps1."
}

$effectivePort = $Port
if ($effectivePort -eq 0) {
    $effectivePort = 8092
    if (Test-Path -LiteralPath $configPath -PathType Leaf) {
        try {
            $cfg = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
            if ($cfg.port -and [int]$cfg.port -ge 1 -and [int]$cfg.port -le 65535) {
                $effectivePort = [int]$cfg.port
            }
        } catch {}
    }
}

$bindHost = if ($LocalOnly) {
    "127.0.0.1"
} elseif (-not [string]::IsNullOrWhiteSpace($HostAddress)) {
    $HostAddress.Trim()
} else {
    "0.0.0.0"
}

# Evitar colisiones de bloqueo en Redb si dragon.exe ya esta corriendo o arrancando en este puerto
function Test-DragonHealth([int]$CheckPort) {
    try {
        $resp = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/health" -f $CheckPort) -TimeoutSec 2 -ErrorAction Stop
        return ($resp -and $resp.status -eq "ok")
    } catch {
        return $false
    }
}

if (Test-DragonHealth -CheckPort $effectivePort) {
    if ($OpenBrowser) {
        Start-Process ("http://127.0.0.1:{0}" -f $effectivePort)
    }
    exit 0
}

# Si ya hay una instancia previa de dragon.exe cargando la DB, esperar brevemente a que abra el puerto
$existingDragon = Get-Process -Name "dragon" -ErrorAction SilentlyContinue
if ($existingDragon) {
    for ($i = 0; $i -lt 15; $i++) {
        Start-Sleep -Seconds 1
        if (Test-DragonHealth -CheckPort $effectivePort) {
            if ($OpenBrowser) {
                Start-Process ("http://127.0.0.1:{0}" -f $effectivePort)
            }
            exit 0
        }
    }
}

Set-Location -LiteralPath $root
$arguments = @("ui", "--host", $bindHost, "--port", "$effectivePort", "--strict-port")
if (-not $OpenBrowser) {
    $arguments += "--no-browser"
}
if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    $arguments += @("--config", $configPath)
}
if (-not $LocalOnly -and $LanMode) {
    $arguments += @("--lan-mode", $LanMode)
}

& $exe @arguments
exit $LASTEXITCODE
