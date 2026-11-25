param(
    [string]$PrimaryIP = "172.23.174.103",
    [string]$AuthorizedSubnet = "172.23.174.0/24",
    [ValidateRange(1,65535)][int]$Port = 8092
)
$ErrorActionPreference = "Stop"

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Ejecute este instalador como Administrador en $PrimaryIP (BAINTS604)."
}

$root = Split-Path -Parent $PSScriptRoot
$runLan = Join-Path $PSScriptRoot "run-lan.ps1"
$fwScript = Join-Path $PSScriptRoot "configure-firewall.ps1"

Write-Host "=== 1. Validando servidor principal $PrimaryIP y binario dragon.exe ===" -ForegroundColor Cyan
$ipObj = Get-NetIPAddress -AddressFamily IPv4 -IPAddress $PrimaryIP -ErrorAction SilentlyContinue | Where-Object AddressState -eq 'Preferred'
if (-not $ipObj) {
    throw "La IP $PrimaryIP no esta activa en este equipo. Este servicio debe instalarse unicamente en el servidor principal ($PrimaryIP), nunca en el proxy (.11)."
}

$exe = @(
    (Join-Path $root "dragon.exe"),
    (Join-Path $root "target\release\dragon.exe"),
    (Join-Path $root "target\offline-lan\release\dragon.exe")
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $exe) {
    throw "No se encontro dragon.exe (compilacion estandar con LAN) en $root. Compile primero con .\compilador.ps1 o copie dragon.exe en la raiz del proyecto."
}
Write-Host "Binario detectado: $exe" -ForegroundColor Green

Write-Host "=== 2. Configurando Firewall TCP $Port ($AuthorizedSubnet) ===" -ForegroundColor Cyan
$fwName = "Dragon-Radar-LAN-$Port"
$existingFw = Get-NetFirewallRule -Name $fwName -ErrorAction SilentlyContinue
if ($existingFw) {
    Enable-NetFirewallRule -Name $fwName | Out-Null
    Write-Host "Regla de firewall existente verificada y habilitada: $fwName" -ForegroundColor Green
} else {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $fwScript -AuthorizedSubnet $AuthorizedSubnet -Port $Port -Apply
}

Write-Host "=== 2.5. Sincronizando scripts operativos hacia D:\script (replica-11, watchdog, reiniciar-radar) ===" -ForegroundColor Cyan
$srcScriptDir = Join-Path (Split-Path -Parent $root) "script"
if ((Test-Path -LiteralPath "D:\script") -and (Test-Path -LiteralPath $srcScriptDir)) {
    foreach ($rel in @(
        "config\replica-11.json",
        "reiniciar-radar.ps1",
        "ACTIVAR_PROXY_11_HACIA_103.ps1",
        "Monitor-Recuperacion-LAN.ps1",
        "proxy-tls\Caddyfile.baints604",
        "proxy-tls\Caddyfile.baints601"
    )) {
        $s = Join-Path $srcScriptDir $rel
        $d = Join-Path "D:\script" $rel
        if (Test-Path -LiteralPath $s -PathType Leaf) {
            $dParent = Split-Path -Parent $d
            if (-not (Test-Path -LiteralPath $dParent)) { New-Item -ItemType Directory -Path $dParent -Force | Out-Null }
            Copy-Item -LiteralPath $s -Destination $d -Force
        }
    }
    Write-Host "D:\script sincronizado con configuracion de Radar del Dragon (8092)." -ForegroundColor Green
}
$indexValSrc = Join-Path $PSScriptRoot "validar-index-al-abrir.ps1"
if ((Test-Path -LiteralPath "D:\script") -and (Test-Path -LiteralPath $indexValSrc -PathType Leaf)) {
    Copy-Item -LiteralPath $indexValSrc -Destination "D:\script\validar-index-radar.ps1" -Force
}

Write-Host "=== 3. Registrando tarea programada SYSTEM: Dragon-Radar-LAN ===" -ForegroundColor Cyan
$psExe = Join-Path $PSHOME "powershell.exe"
$taskArgs = "-NoProfile -ExecutionPolicy Bypass -File `"$runLan`" -Port $Port"
$action = New-ScheduledTaskAction -Execute $psExe -Argument $taskArgs -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtStartup
$trigger.Delay = "PT30S"
$principalTask = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew `
    -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1)

Register-ScheduledTask -TaskName "Dragon-Radar-LAN" -Action $action -Trigger $trigger `
    -Principal $principalTask -Settings $settings -Force `
    -Description "Radar del Dragon LAN en ${PrimaryIP}:${Port} (inicio sin sesion de usuario, subred ${AuthorizedSubnet})." | Out-Null

Write-Host "=== 4. Iniciando Dragon-Radar-LAN y validando salud ===" -ForegroundColor Cyan
Stop-ScheduledTask -TaskName "Dragon-Radar-LAN" -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Start-ScheduledTask -TaskName "Dragon-Radar-LAN"

$healthUrl = "http://${PrimaryIP}:${Port}/health"
$ok = $false
for ($i = 0; $i -lt 15; $i++) {
    Start-Sleep -Seconds 1
    try {
        $req = [System.Net.HttpWebRequest]::Create($healthUrl)
        $req.Proxy = $null
        $req.Timeout = 3000
        $resp = $req.GetResponse()
        try {
            $body = (New-Object IO.StreamReader($resp.GetResponseStream())).ReadToEnd() | ConvertFrom-Json
            if ($body.app -eq "dragon-radar" -and $body.status -eq "ok") {
                Write-Host "[OK] Radar del Dragon operativo en $healthUrl (version: $($body.version), modo: $($body.lan_mode))" -ForegroundColor Green
                $ok = $true
                break
            }
        } finally { $resp.Close() }
    } catch {}
}
if (-not $ok) {
    Write-Warning "La tarea Dragon-Radar-LAN fue registrada e iniciada, pero $healthUrl aun no respondio en 15s. Verifique que dragon.exe incluya la feature lan."
}
