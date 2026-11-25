<#
.SYNOPSIS
    Regla de validacion del indice al abrir Radar del Dragon (compatible con .103 Principal,
    .11 Proxy y equipos locales).

.DESCRIPTION
    Al abrir la plataforma:
    1. Consulta /api/index/validate para verificar si la indexacion esta completa o si faltan
       rutas/etapas por indexar.
    2. Si la indexacion ESTA COMPLETA: cierra las ventanas de comandos auxiliares de Radar
       del Dragon (y oculta cualquier consola interactiva remanente sin detener el servicio).
    3. Si la indexacion ESTA INCOMPLETA: garantiza que continue en segundo plano (sin ventanas
       de comandos visibles) respetando la regla anti-split-brain en el proxy 172.23.174.11.
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
if (-not (Test-Path -LiteralPath (Join-Path $projectRoot "config")) -and (Test-Path -LiteralPath (Split-Path -Parent $projectRoot))) {
    $parentTry = Split-Path -Parent $projectRoot
    if (Test-Path -LiteralPath (Join-Path $parentTry "dragon.exe")) {
        $projectRoot = $parentTry
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

function Hide-ProcessConsoleWindow([IntPtr]$Hwnd) {
    if ($Hwnd -eq [IntPtr]::Zero) { return }
    try {
        if (-not ("DragonWin32Console" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class DragonWin32Console {
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}
"@ -ErrorAction SilentlyContinue
        }
        [void][DragonWin32Console]::ShowWindow($Hwnd, 0) # SW_HIDE = 0
    } catch {}
}

function Close-RadarCommandWindows {
    $closedCount = 0
    $hiddenCount = 0

    # 1. Identificar PIDs ancestros de dragon.exe activos para NUNCA matar el servicio web activo
    $protectedPids = New-Object System.Collections.Generic.HashSet[int]
    [void]$protectedPids.Add($PID)

    $allProcs = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $byPid = @{}
    foreach ($p in $allProcs) { $byPid[[int]$p.ProcessId] = $p }

    foreach ($p in $allProcs) {
        if ($p.Name -ieq "dragon.exe") {
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

    # 2. Ocultar consolas visibles de dragon.exe y sus padres directos (mantiene el servidor vivo sin ventanas abiertas)
    foreach ($pidProtect in $protectedPids) {
        if ($pidProtect -eq $PID) { continue }
        try {
            $procObj = Get-Process -Id $pidProtect -ErrorAction SilentlyContinue
            if ($procObj -and $procObj.MainWindowHandle -ne [IntPtr]::Zero) {
                $title = [string]$procObj.MainWindowTitle
                $isRadarWindow = ($procObj.ProcessName -ieq "dragon") -or
                    ($title -imatch 'Radar del Dragon|Dragon Radar|servidor local|servidor para LAN|run-lan|iniciar_radar|Activar_Radar')
                if ($isRadarWindow) {
                    Hide-ProcessConsoleWindow -Hwnd $procObj.MainWindowHandle
                    $hiddenCount++
                }
            }
        } catch {}
    }

    # 3. Cerrar ventanas de comandos auxiliares de Radar (indexar_equipo, lanzadores ya terminados, etc.)
    foreach ($p in $allProcs) {
        $procId = [int]$p.ProcessId
        if ($protectedPids.Contains($procId)) { continue }
        if ($p.Name -notmatch '^(cmd|powershell|pwsh)\.exe$') { continue }
        $cmdLine = [string]$p.CommandLine
        $isRadarCmd = $cmdLine -imatch '(iniciar_radar\.bat|indexar_equipo\.(bat|ps1)|servidor local\.bat|servidor para LAN\.bat|Activar_Radar_Local\.bat|validar-index-al-abrir\.ps1)'
        if (-not $isRadarCmd) {
            try {
                $gp = Get-Process -Id $procId -ErrorAction SilentlyContinue
                if ($gp -and ($gp.MainWindowTitle -imatch 'Radar del Dragon|Dragon Radar|Indexador - Radar')) {
                    $isRadarCmd = $true
                }
            } catch {}
        }
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

    Write-IndexLog "OK" "Limpieza de ventanas de comandos completada (cerradas=$closedCount, ocultadas=$hiddenCount)."
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
        Write-IndexLog "WARN" "Rol ProxyServer (.11): Radar en .103:$Port no respondio aun. Por regla anti-split-brain no se inicia dragon.exe en .11."
        exit 0
    }
    # En .103 o equipo local: si dragon.exe existe pero el servidor no respondio, lanzar sync silencioso en segundo plano
    $exe = @(
        (Join-Path $projectRoot "dragon.exe"),
        (Join-Path $projectRoot "target\release\dragon.exe")
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1

    if ($exe) {
        $alreadySync = @(Get-CimInstance Win32_Process -Filter "Name='dragon.exe'" -ErrorAction SilentlyContinue)
        if ($alreadySync.Count -eq 0) {
            Write-IndexLog "INFO" "Servidor no activo; iniciando 'dragon.exe sync' en segundo plano (sin ventana)..."
            Start-Process -FilePath $exe -ArgumentList "sync" -WorkingDirectory $projectRoot -WindowStyle Hidden | Out-Null
        }
    } else {
        Write-IndexLog "WARN" "No se encontro dragon.exe ni respuesta en puerto $Port."
    }
    exit 0
}

$data = $result.Data
if ($data.complete -eq $true) {
    Write-IndexLog "OK" "Indexacion COMPLETA en $($data.machine_name) (archivos=$($data.redb_records), vectores=$($data.vector_count), checkpoint=$($data.last_checkpoint)). Cerrando ventanas de comandos..."
    Close-RadarCommandWindows
    exit 0
}

Write-IndexLog "INFO" "Indexacion INCOMPLETA en $($data.machine_name) (fase=$($data.phase), pendientes=$($data.pending_targets), reanudada=$($data.resumed_just_now)). Continuando en segundo plano..."
# Ocultar consolas visibles para que continue sin molestar en segundo plano
Close-RadarCommandWindows

if ($WaitUntilComplete) {
    $deadline = (Get-Date).AddMinutes(30)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 5
        $poll = Invoke-ValidateEndpoint -Urls $candidateUrls
        if ($poll.Ok -and $poll.Data.complete -eq $true) {
            Write-IndexLog "OK" "Indexacion en segundo plano finalizada completamente. Cerrando ventanas de comandos..."
            Close-RadarCommandWindows
            break
        }
    }
}
exit 0
