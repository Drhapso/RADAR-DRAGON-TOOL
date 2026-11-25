param(
    [switch]$Lan,
    [ValidateRange(0, 65535)][int]$Port = 0,
    [switch]$NoBrowser,
    [ValidateSet("", "local_agent", "shared_server")][string]$LanMode = ""
)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$exe = Join-Path $root "dragon.exe"
$configPath = Join-Path $root "config\site_config.json"

function Show-Message([string]$Text, [string]$Icon = "Information") {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show($Text, "Radar del Dragon", "OK", $Icon) | Out-Null
}

function Get-ConfiguredPort {
    if (Test-Path -LiteralPath $configPath -PathType Leaf) {
        try {
            $cfg = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
            if ($cfg.port -and [int]$cfg.port -ge 1 -and [int]$cfg.port -le 65535) {
                return [int]$cfg.port
            }
        } catch {}
    }
    return 8092
}

function Get-RadarHealth([int]$Candidate) {
    # Sin proxy: la consulta es siempre al propio equipo.
    $request = [System.Net.HttpWebRequest]::Create("http://127.0.0.1:$Candidate/health")
    $request.Proxy = $null
    $request.Timeout = 2000
    try {
        $response = $request.GetResponse()
        try { $body = (New-Object IO.StreamReader($response.GetResponseStream())).ReadToEnd() }
        finally { $response.Close() }
        try { $data = $body | ConvertFrom-Json } catch { $data = $null }
        if ($data -and $data.app -eq "dragon-radar" -and $data.status -eq "ok") {
            return @{ State = "radar"; Data = $data }
        }
        return @{ State = "other"; Data = $null }
    }
    catch {
        if (Get-NetTCPConnection -State Listen -LocalPort $Candidate -ErrorAction SilentlyContinue) {
            return @{ State = "other"; Data = $null }
        }
        return @{ State = "free"; Data = $null }
    }
}

try {
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw "No se encuentra dragon.exe en $root. Reinstale Radar del Dragon."
    }

    $basePort = if ($Port -gt 0) { $Port } else { Get-ConfiguredPort }
    $selected = $null
    $alreadyRunning = $false
    $healthData = $null
    foreach ($candidate in $basePort..([Math]::Min($basePort + 9, 65535))) {
        $probe = Get-RadarHealth $candidate
        if ($probe.State -eq "radar") {
            $selected = $candidate
            $alreadyRunning = $true
            $healthData = $probe.Data
            break
        }
        if ($probe.State -eq "free") {
            $selected = $candidate
            break
        }
    }
    if (-not $selected) {
        throw "Los puertos $basePort a $($basePort + 9) estan ocupados por otras aplicaciones."
    }

    if (-not $alreadyRunning) {
        $bind = if ($Lan) { "0.0.0.0" } else { "127.0.0.1" }
        $argsList = @("ui", "--host", $bind, "--port", "$selected", "--no-browser", "--strict-port")
        if (Test-Path -LiteralPath $configPath -PathType Leaf) {
            $argsList += @("--config", $configPath)
        }
        if ($Lan -and $LanMode) {
            $argsList += @("--lan-mode", $LanMode)
        }
        $server = Start-Process -FilePath $exe -ArgumentList $argsList `
            -WorkingDirectory $root -WindowStyle Minimized -PassThru
        $deadline = (Get-Date).AddSeconds(60)
        while ($true) {
            $probe = Get-RadarHealth $selected
            if ($probe.State -eq "radar") {
                $healthData = $probe.Data
                break
            }
            if ($server.HasExited) {
                throw "El servidor Radar del Dragon se cerro al iniciar (codigo $($server.ExitCode)). Ejecute dragon.exe ui desde $root para ver el detalle."
            }
            if ((Get-Date) -gt $deadline) {
                throw "Radar del Dragon no respondio en 60 segundos. Revise la ventana minimizada de dragon.exe."
            }
            Start-Sleep -Milliseconds 400
        }
    }

    $url = "http://127.0.0.1:$selected/"
    if (-not $NoBrowser) { Start-Process $url }

    # Regla al abrir la plataforma: validar indice (cierra consolas si completo, continua en segundo plano si falta)
    $indexValidator = @(
        (Join-Path $root "herramientas\validar-index-al-abrir.ps1"),
        (Join-Path $root "deploy\validar-index-al-abrir.ps1")
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if ($indexValidator) {
        Start-Process -FilePath (Join-Path $PSHOME "powershell.exe") `
            -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", $indexValidator, "-Port", "$selected") `
            -WindowStyle Hidden | Out-Null
    }

    if ($Lan) {
        $addresses = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object {
            $_.IPAddress -notlike "127.*" -and $_.IPAddress -notlike "169.254.*"
        } | ForEach-Object { "  http://$($_.IPAddress):$selected/" })
        if (-not $addresses.Count) {
            $addresses = @("  http://127.0.0.1:$selected/")
        }

        $note = if ($alreadyRunning -and $healthData -and -not $healthData.bind_lan) {
            "ATENCION: Radar del Dragon ya estaba ejecutandose en modo SOLO LOCAL. Cierre la ventana minimizada de dragon.exe y vuelva a abrir este acceso para compartirlo en LAN.`r`n`r`n"
        } elseif ($alreadyRunning) {
            "Radar del Dragon ya estaba en ejecucion en modo LAN.`r`n`r`n"
        } else {
            ""
        }

        $activeMode = if ($healthData -and $healthData.lan_mode) { [string]$healthData.lan_mode } else { "local_agent" }
        $modeDesc = if ($activeMode -eq "shared_server") {
            $count = if ($healthData) { [int]$healthData.shared_paths_count } else { 0 }
            "Modo LAN activo: SERVIDOR COMPARTIDO ($count carpeta(s) en config\site_config.json). Los clientes buscan y descargan directo desde el navegador."
        } else {
            "Modo LAN activo: AGENTE LOCAL POR EQUIPO (cada PC de la LAN indexa y busca solo sus propios discos)."
        }

        Show-Message ($note + "Radar del Dragon esta compartido en la red local. Otros equipos pueden entrar con:`r`n" + ($addresses -join "`r`n") +
            "`r`n`r`n" + $modeDesc +
            "`r`n`r`nSi otros equipos no conectan, autorice el puerto $selected en el Firewall (ver COMPLEMENTOS_ADICIONALES.txt).`r`nNo cierre la ventana minimizada de dragon.exe mientras se use.")
    }
}
catch {
    Show-Message $_.Exception.Message "Error"
    exit 1
}
