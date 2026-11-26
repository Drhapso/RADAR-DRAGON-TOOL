[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ServerUrl,
    [string]$WheelhousePath = '',
    [string]$PythonPath = '',
    [switch]$SkipDependencyInstall
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$sourceRoot = Split-Path -Parent $PSCommandPath
$dependencyInstaller = Join-Path $sourceRoot 'install-client-dependencies.ps1'
$appRoot = Join-Path $env:LOCALAPPDATA 'RadarDelDragon\app'
$dataRoot = Join-Path $env:LOCALAPPDATA 'RadarDelDragon\data'
$configDir = Join-Path $appRoot 'config'
$configPath = Join-Path $configDir 'site_config.json'
$userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$taskName = 'RadarDelDragon-LocalAgent-' + ($userSid -replace '[^A-Za-z0-9_-]', '_')
$port = 8092

function Resolve-Python([string]$RequestedPython = '') {
    if ($RequestedPython) {
        $requested = (Resolve-Path -LiteralPath $RequestedPython -ErrorAction Stop).Path
        $requestedVersion = & $requested -c "import sys; print('.'.join(map(str, sys.version_info[:2])))"
        if ($LASTEXITCODE -ne 0 -or [version]$requestedVersion.Trim() -lt [version]'3.10') {
            throw 'The requested Python executable is invalid or older than Python 3.10.'
        }
        return $requested
    }
    $candidates = @()
    $cmd = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source -and $cmd.Source -notmatch '\\WindowsApps\\') { $candidates += $cmd.Source }
    $launcher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($launcher -and $launcher.Source) {
        try {
            $resolved = & $launcher.Source -3 -c 'import sys; print(sys.executable)' 2>$null
            if ($LASTEXITCODE -eq 0 -and $resolved) { $candidates += ($resolved | Select-Object -First 1).Trim() }
        } catch {}
    }
    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        try {
            $version = & $candidate -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>$null
            if ($LASTEXITCODE -eq 0 -and [version]$version -ge [version]'3.10') { return $candidate }
        } catch {}
    }
    throw 'No se encontró Python 3.10 o superior instalado y permitido. Solicite a TI el runtime Python aprobado; este instalador no requiere ni solicita elevación.'
}

try {
    $baseUri = [Uri]$ServerUrl
    if ($baseUri.Scheme -notin @('http', 'https') -or -not $baseUri.IsDefaultPort -and $baseUri.Port -lt 1) {
        throw 'La dirección de Radar no es válida.'
    }
    New-Item -ItemType Directory -Path $appRoot, $dataRoot, $configDir -Force | Out-Null

    # Detener la tarea de este usuario antes de reemplazar scripts abiertos.
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    try { Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$port/api/shutdown" -TimeoutSec 2 | Out-Null } catch {}
    Get-CimInstance Win32_Process -Filter "Name LIKE 'python%'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains((Join-Path $appRoot 'server.py')) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    Copy-Item -Path (Join-Path $sourceRoot 'server.py') -Destination $appRoot -Force
    Copy-Item -Path (Join-Path $sourceRoot 'radar_indexer.py') -Destination $appRoot -Force
    Copy-Item -Path (Join-Path $sourceRoot 'radar_preview.py') -Destination $appRoot -Force
    Copy-Item -Path (Join-Path $sourceRoot 'requirements.txt') -Destination $appRoot -Force
    Copy-Item -LiteralPath $dependencyInstaller -Destination (Join-Path $appRoot 'install-client-dependencies.ps1') -Force
    Copy-Item -Path (Join-Path $sourceRoot 'ui') -Destination $appRoot -Recurse -Force

    $python = Resolve-Python $PythonPath
    $venvPython = Join-Path $appRoot '.venv\Scripts\python.exe'
    $pythonw = Join-Path $appRoot '.venv\Scripts\pythonw.exe'
    if (-not (Test-Path -LiteralPath $venvPython)) {
        & $python -m venv (Join-Path $appRoot '.venv')
        if ($LASTEXITCODE -ne 0) { throw 'No se pudo crear el entorno Python local.' }
    }
    if (-not $SkipDependencyInstall) {
        if (-not $WheelhousePath -and $env:DRAGON_RADAR_WHEELHOUSE) {
            $WheelhousePath = $env:DRAGON_RADAR_WHEELHOUSE
        }
        $dependencyArgs = @(
            '-PythonPath', $venvPython,
            '-RequirementsPath', (Join-Path $appRoot 'requirements.txt'),
            '-ReportPath', (Join-Path $dataRoot 'dependency-check.json')
        )
        if ($WheelhousePath) { $dependencyArgs += @('-WheelhousePath', $WheelhousePath) }
        & (Join-Path $appRoot 'install-client-dependencies.ps1') @dependencyArgs
    }

    $configUrl = $baseUri.AbsoluteUri.TrimEnd('/') + '/api/agent/client-config'
    if ($baseUri.IsLoopback) {
        $allowedOrigins = @($baseUri.GetLeftPart([UriPartial]::Authority))
    } else {
        try {
            $clientConfig = Invoke-RestMethod -Uri $configUrl -TimeoutSec 10
            $allowedOrigins = @($clientConfig.allowed_ui_origins | Where-Object { $_ })
            if ($clientConfig.port) { $port = [int]$clientConfig.port }
        } catch {
            $allowedOrigins = @($baseUri.GetLeftPart([UriPartial]::Authority))
            Write-Warning 'The LAN server did not publish client config; allowing only the origin used to download the activator.'
        }
    }

    $localConfig = @{
        port = $port
        lan_mode = 'local_agent'
        authorized_subnet = ''
        shared_paths = @()
        allowed_ui_origins = $allowedOrigins
        sync_interval_minutes = 15
    } | ConvertTo-Json -Depth 4
    Set-Content -LiteralPath $configPath -Value $localConfig -Encoding utf8

    $account = "$env:USERDOMAIN\$env:USERNAME"
    $action = New-ScheduledTaskAction -Execute $pythonw -Argument "`"$appRoot\server.py`" --local --port $port --config `"$configPath`" --data-dir `"$dataRoot`" --no-browser" -WorkingDirectory $appRoot
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $account
    $principal = New-ScheduledTaskPrincipal -UserId $account -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1)
    $scheduled = $false
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $scheduled = $true
    } catch {
        Write-Warning "Windows no permitió registrar una tarea del usuario ($($_.Exception.Message)); se iniciará solo para esta sesión."
        Start-Process -FilePath $pythonw -ArgumentList @("`"$appRoot\server.py`"", '--local', '--port', "$port", '--config', "`"$configPath`"", '--data-dir', "`"$dataRoot`"", '--no-browser') -WorkingDirectory $appRoot -WindowStyle Hidden
    }

    $health = $false
    for ($i = 0; $i -lt 25; $i++) {
        Start-Sleep -Milliseconds 800
        try {
            $h = Invoke-RestMethod -Uri "http://127.0.0.1:$port/health" -TimeoutSec 2
            if ($h.app -eq 'dragon-radar' -and $h.status -eq 'ok') { $health = $true; break }
        } catch {}
    }
    if (-not $health) { throw "El agente local no respondió en 127.0.0.1:$port. Revise $dataRoot\radar-service.log. Tarea registrada: $scheduled" }
    Start-Process "http://127.0.0.1:$port/"
    Write-Host "Radar local instalado y activo para $env:USERNAME. No se solicitaron credenciales administrativas."
    Write-Host "Base de datos local: $dataRoot\radar_service.sqlite3"
} catch {
    Write-Error $_
    exit 1
}
