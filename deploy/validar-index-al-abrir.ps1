<#
.SYNOPSIS
    Regla de validacion del indice al abrir Radar del Dragon (Servicio Python/Waitress,
    compatible con .103 Principal, .11 Proxy y equipos locales, sin ejecutables .exe).
#>

[CmdletBinding()]
param(
    [ValidateRange(1, 65535)][int]$Port = 8092,
    [string]$PrimaryIP = "172.23.174.103",
    [string]$ProxyIP = "172.23.174.11",
    [switch]$CleanupOnly,
    [switch]$WaitUntilComplete
)

$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$scriptDir = $PSScriptRoot
$projectRoot = Split-Path -Parent $scriptDir
if (-not (Test-Path -LiteralPath (Join-Path $projectRoot "server.py"))) {
    $fallbackRoot = "D:\PROYECTOS IA INDEPENDIENTES\RADAR-DRAGON-TOOL"
    if (Test-Path -LiteralPath (Join-Path $fallbackRoot "server.py")) {
        $projectRoot = $fallbackRoot
    }
}

$logDir = Join-Path $env:LOCALAPPDATA "DragonRadar\logs"
try { New-Item -ItemType Directory -Path $logDir -Force | Out-Null } catch {}
$logFile = Join-Path $logDir "validacion-index.log"

function Write-IndexLog([string]$Level, [string]$Message) {
    $line = "{0} [{1}] [{2}] {3}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $Level, $env:COMPUTERNAME, $Message
    try { Add-Content -LiteralPath $logFile -Value $line -Encoding UTF8 } catch {}
}

function Get-LocalRole {
    $ips = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty IPAddress)
    if ($ips -contains $PrimaryIP) { return "PrimaryServer" }
    if ($ips -contains $ProxyIP) { return "ProxyServer" }
    return "LocalWorkstation"
}

function Close-RadarCommandWindows {
    $closedCount = 0

    # 1. Proteger el servicio Python activo (RADAR-DRAGON-TOOL\server.py) y sus procesos ancestros
    $protectedPids = New-Object System.Collections.Generic.HashSet[int]
    [void]$protectedPids.Add($PID)

    $allProcs = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $byPid = @{}
    foreach ($p in $allProcs) { $byPid[[int]$p.ProcessId] = $p }

    foreach ($p in $allProcs) {
        $cmd = [string]$p.CommandLine
        if (($p.Name -ilike "python*") -and ($cmd -like "*RADAR-DRAGON-TOOL*server.py*")) {
            [void]$protectedPids.Add([int]$p.ProcessId)
            $cur = $p
            $depth = 0
            while ($cur -and $depth -lt 6) {
                $ppid = [int]$cur.ParentProcessId
                if ($ppid -le 4 -or -not $byPid.ContainsKey($ppid)) { break }
                $parent = $byPid[$ppid]
                if ($parent.Name -imatch '^(cmd|powershell|pwsh|conhost|WindowsTerminal)\.exe$') {
                    [void]$protectedPids.Add($ppid)
                }
                $cur = $parent
                $depth++
            }
        }
    }

    # 2. Cerrar ventanas de comandos auxiliares de Radar ya finalizadas
    foreach ($p in $allProcs) {
        $procId = [int]$p.ProcessId
        if ($protectedPids.Contains($procId)) { continue }
        if ($p.Name -notmatch '^(cmd|powershell|pwsh)\.exe$') { continue }
        $cmdLine = [string]$p.CommandLine
        $isRadarCmd = $cmdLine -imatch '(iniciar_radar\.bat|indexar_equipo\.(bat|ps1)|servidor local\.bat|servidor para LAN\.bat|Activar_Radar_Local\.bat|validar-index-al-abrir\.ps1)'
        if ($isRadarCmd) {
            try {
                $gp = Get-Process -Id $procId -ErrorAction SilentlyContinue
                if ($gp) {
                    if ($gp.MainWindowHandle -ne [IntPtr]::Zero) {
                        [void]$gp.CloseMainWindow()
                        Start-Sleep -Milliseconds 300
                    }
                    if (-not $gp.HasExited) {
                        Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
                    }
                    $closedCount++
                }
            } catch {}
        }
    }

    Write-IndexLog "OK" "Limpieza de ventanas de comandos completada (cerradas=$closedCount)."
}

function Invoke-ValidateEndpoint([string[]]$Urls) {
    foreach ($url in $Urls) {
        try {
            $req = [System.Net.HttpWebRequest]::Create($url)
            $req.Proxy = $null
            $req.Timeout = 4000
            $resp = $req.GetResponse()
            try {
                $body = (New-Object IO.StreamReader($resp.GetResponseStream())).ReadToEnd()
                $json = $body | ConvertFrom-Json
                if ($json -and $json.app -eq "dragon-radar") {
                    return @{ Ok = $true; Url = $url; Data = $json }
                }
            } finally {
                $resp.Close()
            }
        } catch {}
    }
    return @{ Ok = $false; Url = $null; Data = $null }
}

if ($CleanupOnly) {
    Close-RadarCommandWindows
    exit 0
}

$role = Get-LocalRole
$candidateUrls = switch ($role) {
    "PrimaryServer" {
        @(
            "http://127.0.0.1:${Port}/api/index/validate?trigger_script=0",
            "http://${PrimaryIP}:${Port}/api/index/validate?trigger_script=0"
        )
    }
    "ProxyServer" {
        @(
            "http://${ProxyIP}:${Port}/api/index/validate?trigger_script=0",
            "http://${PrimaryIP}:${Port}/api/index/validate?trigger_script=0"
        )
    }
    default {
        @("http://127.0.0.1:${Port}/api/index/validate?trigger_script=0")
    }
}

$result = Invoke-ValidateEndpoint -Urls $candidateUrls

if (-not $result.Ok) {
    if ($role -eq "ProxyServer") {
        Write-IndexLog "WARN" "Rol ProxyServer (.11): Radar en .103:$Port no respondio aun. Por regla anti-split-brain no se inicia servicio en .11."
        exit 0
    }
    $runLan = Join-Path $projectRoot "deploy\run-lan.ps1"
    if (Test-Path -LiteralPath $runLan -PathType Leaf) {
        $alreadyRunning = @(Get-CimInstance Win32_Process -Filter "Name LIKE 'python%'" -ErrorAction SilentlyContinue |
            Where-Object { $_.CommandLine -and $_.CommandLine -like "*RADAR-DRAGON-TOOL*server.py*" })
        if ($alreadyRunning.Count -eq 0) {
            Write-IndexLog "INFO" "Servidor no activo; iniciando sincronizacion Python en segundo plano (sin ventana)..."
            Start-Process -FilePath (Join-Path $PSHOME "powershell.exe") `
                -ArgumentList "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$runLan`" -SyncOnly" `
                -WorkingDirectory $projectRoot -WindowStyle Hidden | Out-Null
        }
    } else {
        Write-IndexLog "WARN" "No se encontro deploy\run-lan.ps1 ni respuesta en puerto $Port."
    }
    exit 0
}

$data = $result.Data
if ($data.complete -eq $true) {
    Write-IndexLog "OK" "Indexacion COMPLETA en $($data.machine_name) (archivos=$($data.redb_records), contenido=$($data.vector_count), checkpoint=$($data.last_checkpoint))."
    Close-RadarCommandWindows
    exit 0
}

Write-IndexLog "INFO" "Indexacion INCOMPLETA en $($data.machine_name) (fase=$($data.phase), pendientes=$($data.pending_targets), reanudada=$($data.resumed_just_now)). Continuando en segundo plano..."
Close-RadarCommandWindows

if ($WaitUntilComplete) {
    $deadline = (Get-Date).AddMinutes(30)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 5
        $poll = Invoke-ValidateEndpoint -Urls $candidateUrls
        if ($poll.Ok -and $poll.Data.complete -eq $true) {
            Write-IndexLog "OK" "Indexacion en segundo plano finalizada completamente."
            Close-RadarCommandWindows
            break
        }
    }
}
exit 0
