<#
.SYNOPSIS
    Regla de arranque automatico del servicio de indexacion de Radar del Dragon al encender
    el equipo y mantenimiento periodico de la base de datos (redb + Tantivy + vectores).

.DESCRIPTION
    1. Respeta la regla anti-split-brain (bloquea instalacion de indexador local en el proxy 172.23.174.11).
    2. Registra la tarea 'Dragon-Radar-Indexador-Arranque':
       - Como Administrador: inicia al encender Windows (AtStartup, SYSTEM, sin sesion de usuario).
       - Como usuario estandar: inicia al iniciar sesion (AtLogon, oculto en segundo plano).
    3. Registra la tarea 'Dragon-Radar-Mantener-DB' (cada 30 min) que ejecuta validar-index-al-abrir.ps1
       para verificar que todas las unidades fijas (C:\, D:\, ...) esten indexadas y mantener la DB al dia.
    4. Ejecuta una validacion inmediata de las unidades locales detectadas en el equipo.

.EXAMPLE
    .\deploy\instalar-servicio-indexacion-arranque.ps1
    Instala el arranque automatico en modo LAN (puerto 8092) y mantenimiento cada 30 min.

.EXAMPLE
    .\deploy\instalar-servicio-indexacion-arranque.ps1 -SoloLocal
    Instala el arranque automatico solo local (127.0.0.1:8092, sin abrir puertos de red).

.EXAMPLE
    .\deploy\instalar-servicio-indexacion-arranque.ps1 -Desinstalar
    Elimina las tareas programadas de indexacion automatica.
#>

[CmdletBinding()]
param(
    [ValidateRange(1, 65535)][int]$Port = 8092,
    [ValidateRange(5, 1440)][int]$IntervaloMinutos = 30,
    [string]$ProxyIP = "172.23.174.11",
    [switch]$SoloLocal,
    [switch]$Desinstalar
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path -Parent $PSScriptRoot
$runLanScript = Join-Path $PSScriptRoot "run-lan.ps1"
$validateScript = Join-Path $PSScriptRoot "validar-index-al-abrir.ps1"
$bootTaskName = "Dragon-Radar-Indexador-Arranque"
$keepFreshTaskName = "Dragon-Radar-Mantener-DB"

if ($Desinstalar) {
    foreach ($tn in @($bootTaskName, $keepFreshTaskName)) {
        & schtasks.exe /Delete /TN $tn /F 2>$null | Out-Null
        Write-Host "Tarea eliminada (si existia): $tn" -ForegroundColor Yellow
    }
    exit 0
}

# Anti-split-brain: nunca instalar indexador local en el servidor proxy (.11)
$localIps = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty IPAddress)
if ($localIps -contains $ProxyIP) {
    throw "Este equipo ($ProxyIP) opera como servidor PROXY. Por regla anti-split-brain no se permite instalar el indexador local aqui."
}

$exe = @(
    (Join-Path $root "dragon.exe"),
    (Join-Path $root "target\release\dragon.exe"),
    (Join-Path $root "target\offline-lan\release\dragon.exe")
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $exe) {
    throw "No se encontro dragon.exe en $root. Compile primero con .\compilador.ps1 -SoloEstandar."
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$psExe = Join-Path $PSHOME "powershell.exe"

Write-Host "=== Instalacion de Servicio de Indexacion al Encender ($env:COMPUTERNAME) ===" -ForegroundColor Cyan
Write-Host ("Modo: {0} | Puerto: {1} | Privilegios Admin: {2}" -f $(if ($SoloLocal) { "SOLO LOCAL" } else { "SERVIDOR LAN" }), $Port, $(if ($isAdmin) { "SI (AtStartup/SYSTEM)" } else { "NO (AtLogon/Usuario)" })) -ForegroundColor Gray

# 1. Tarea de arranque automatico (al encender / iniciar sesion)
$runArgs = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$runLanScript`" -Port $Port"
if ($SoloLocal) { $runArgs += " -LocalOnly" }

if ($isAdmin) {
    $principalId = "Service"
    $triggerBoot = "<BootTrigger><Enabled>true</Enabled><Delay>PT20S</Delay></BootTrigger>"
    $principalXml = "<Principal id=`"$principalId`"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel></Principal>"
} else {
    $principalId = "Author"
    $userId = $identity.Name
    $triggerBoot = "<LogonTrigger><Enabled>true</Enabled><UserId>$userId</UserId><Delay>PT15S</Delay></LogonTrigger>"
    $principalXml = "<Principal id=`"$principalId`"><UserId>$userId</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal>"
}

$runArgsXml = [System.Security.SecurityElement]::Escape($runArgs)
$rootXml = [System.Security.SecurityElement]::Escape($root)
$psExeXml = [System.Security.SecurityElement]::Escape($psExe)

$bootXml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Inicia automaticamente Radar del Dragon e indexa las unidades locales al encender el equipo ($env:COMPUTERNAME, puerto $Port).</Description>
  </RegistrationInfo>
  <Triggers>
    $triggerBoot
  </Triggers>
  <Principals>
    $principalXml
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>5</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="$principalId">
    <Exec>
      <Command>$psExeXml</Command>
      <Arguments>$runArgsXml</Arguments>
      <WorkingDirectory>$rootXml</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@

$tmpBootXml = Join-Path $env:TEMP "dragon-radar-boot-task.xml"
[System.IO.File]::WriteAllText($tmpBootXml, $bootXml, [System.Text.Encoding]::Unicode)
try {
    & schtasks.exe /Create /TN $bootTaskName /XML $tmpBootXml /F | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "No se pudo crear la tarea $bootTaskName (codigo $LASTEXITCODE)." }
    Write-Host "[OK] Tarea de arranque registrada: $bootTaskName" -ForegroundColor Green
} finally {
    Remove-Item -LiteralPath $tmpBootXml -Force -ErrorAction SilentlyContinue
}

# 2. Tarea periodica de verificacion de unidades y actualizacion continua de la DB
$valArgs = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$validateScript`" -Port $Port"
$valArgsXml = [System.Security.SecurityElement]::Escape($valArgs)
$startTime = (Get-Date).AddMinutes(2).ToString("yyyy-MM-ddTHH:mm:ss")
$keepXml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Valida cada $IntervaloMinutos minutos que todas las unidades locales de $env:COMPUTERNAME esten indexadas y mantiene la DB actualizada.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>PT${IntervaloMinutos}M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>$startTime</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    $principalXml
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT20M</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="$principalId">
    <Exec>
      <Command>$psExeXml</Command>
      <Arguments>$valArgsXml</Arguments>
      <WorkingDirectory>$rootXml</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@

$tmpKeepXml = Join-Path $env:TEMP "dragon-radar-keepdb-task.xml"
[System.IO.File]::WriteAllText($tmpKeepXml, $keepXml, [System.Text.Encoding]::Unicode)
try {
    & schtasks.exe /Create /TN $keepFreshTaskName /XML $tmpKeepXml /F | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "No se pudo crear la tarea $keepFreshTaskName (codigo $LASTEXITCODE)." }
    Write-Host "[OK] Tarea de mantenimiento DB registrada: $keepFreshTaskName (cada $IntervaloMinutos min)" -ForegroundColor Green
} finally {
    Remove-Item -LiteralPath $tmpKeepXml -Force -ErrorAction SilentlyContinue
}

# 3. Validacion inmediata de unidades fijas detectadas en el equipo
Write-Host ""
Write-Host "=== Validacion de Unidades Fijas Locales ===" -ForegroundColor Cyan
$fixedDrives = @(Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" -ErrorAction SilentlyContinue | Select-Object DeviceID, VolumeName, @{N='FreeGB';E={[Math]::Round($_.FreeSpace/1GB,1)}}, @{N='SizeGB';E={[Math]::Round($_.Size/1GB,1)}})
foreach ($d in $fixedDrives) {
    Write-Host ("  - Unidad {0}\  [{1}]  ({2} GB libres de {3} GB)" -f $d.DeviceID, $d.VolumeName, $d.FreeGB, $d.SizeGB) -ForegroundColor White
}

try {
    $st = Invoke-RestMethod -Uri "http://127.0.0.1:${Port}/api/status" -TimeoutSec 4
    Write-Host ""
    Write-Host ("Estado live del motor (puerto $Port): fase={0} | progreso={1}/{2} | archivos={3} | vectores={4}" -f $st.phase, $st.target_index, $st.total_targets, $st.redb_records, $st.vector_count) -ForegroundColor Green
    if ($st.drives) {
        foreach ($drv in $st.drives) {
            Write-Host ("  -> {0}  Estado: {1,-9} | Archivos: {2,-8} | Carpetas: {3}/{4}" -f $drv.drive, $drv.status, $drv.indexed_files, $drv.targets_completed, $drv.targets_total) -ForegroundColor Cyan
        }
    }
} catch {
    Write-Host "Motor en puerto $Port aun no responde; la tarea $bootTaskName lo iniciara automaticamente al encender (o puede iniciarlo ahora)." -ForegroundColor Yellow
}
