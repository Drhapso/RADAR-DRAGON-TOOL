param(
    [ValidateRange(0,65535)][int]$Port = 0,
    [string]$HostAddress = "",
    [switch]$LocalOnly,
    [ValidateSet("", "local_agent", "shared_server")][string]$LanMode = "",
    [switch]$OpenBrowser,
    [switch]$SyncOnly
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$workspaceRoot = Split-Path -Parent $root
$configPath = Join-Path $root "config\site_config.json"
$serverPy = Join-Path $root "server.py"

if (-not (Test-Path -LiteralPath $serverPy -PathType Leaf)) {
    throw "No se encontro server.py en $root. Verifique la instalacion del servicio Python de Radar del Dragon."
}

function Resolve-RadarPython {
    $candidates = New-Object System.Collections.Generic.List[string]
    if ($env:DRAGON_PYTHON -and (Test-Path -LiteralPath $env:DRAGON_PYTHON -PathType Leaf)) {
        $candidates.Add($env:DRAGON_PYTHON)
    }
    foreach ($rel in @(
        (Join-Path $root ".venv\Scripts\python.exe"),
        (Join-Path $workspaceRoot "FOLIO PDF\FOLIO PDF ENTERPRISE\.venv\Scripts\python.exe"),
        (Join-Path $workspaceRoot "FALCON\falcon web\.venv\Scripts\python.exe"),
        (Join-Path $workspaceRoot "HELP DESK TELEMATICA\.venv\Scripts\python.exe")
    )) {
        if (Test-Path -LiteralPath $rel -PathType Leaf) {
            $candidates.Add($rel)
        }
    }
    $sysPy = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($sysPy -and $sysPy.Source) {
        $candidates.Add($sysPy.Source)
    }

    foreach ($py in $candidates) {
        try {
            & $py -c "import flask, waitress" 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) {
                return $py
            }
        } catch {}
    }
    throw "No se encontro un interprete Python con Flask y Waitress instalados (verifique .venv de FOLIO PDF ENTERPRISE o cree .venv local)."
}

$pythonExe = Resolve-RadarPython

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

function Test-DragonHealth([int]$CheckPort) {
    try {
        $resp = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/health" -f $CheckPort) -TimeoutSec 2 -ErrorAction Stop
        return ($resp -and $resp.status -eq "ok" -and $resp.app -eq "dragon-radar")
    } catch {
        return $false
    }
}

if (-not $SyncOnly) {
    if (Test-DragonHealth -CheckPort $effectivePort) {
        if ($OpenBrowser) {
            Start-Process ("http://127.0.0.1:{0}" -f $effectivePort)
        }
        exit 0
    }

    # Si ya hay una instancia de RADAR-DRAGON-TOOL\server.py iniciando, esperar brevemente
    $existingRadar = @(Get-CimInstance Win32_Process -Filter "Name LIKE 'python%'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine -like "*RADAR-DRAGON-TOOL*server.py*" })
    if ($existingRadar.Count -gt 0) {
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
}

Set-Location -LiteralPath $root
$env:PYTHONUTF8 = "1"
$env:PYTHONUNBUFFERED = "1"

$arguments = @("-u", $serverPy, "--host", $bindHost, "--port", "$effectivePort")
if ($LocalOnly) {
    $arguments += "--local"
}
if ($SyncOnly) {
    $arguments += "--sync-only"
} elseif ($OpenBrowser) {
    $arguments += "--open-browser"
} else {
    $arguments += "--no-browser"
}
if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    $arguments += @("--config", $configPath)
}
if (-not $LocalOnly -and $LanMode) {
    $arguments += @("--lan-mode", $LanMode)
}

& $pythonExe @arguments
exit $LASTEXITCODE
