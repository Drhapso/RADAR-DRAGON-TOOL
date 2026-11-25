<#
.SYNOPSIS
    Almacena y actualiza de forma cifrada (Windows DPAPI) la credencial de administrador
    utilizada para instalar LibreOffice en equipos de la red LAN, y registra la validacion
    mensual automatica en este equipo.

.DESCRIPTION
    Por seguridad, la contrasena NUNCA se guarda en texto plano en scripts, archivos JSON
    ni repositorios Git. Se solicita de forma segura (Get-Credential), se valida contra
    Active Directory / equipo local antes de guardarla y se cifra con DPAPI (Export-Clixml)
    restringida por ACL al usuario propietario y Administradores.

.EXAMPLE
    .\deploy\gestionar-credencial-admin.ps1 -InstalarTareaMensual
    Solicita la clave de EJC_NACIONAL\1089196342, valida contra AD, la guarda cifrada
    y crea la tarea programada mensual Dragon-Radar-ValidarCredencial-Mensual.

.EXAMPLE
    .\deploy\gestionar-credencial-admin.ps1 -Actualizar
    Actualiza la clave cifrada tras un cambio mensual de contrasena de administrador.
#>

[CmdletBinding()]
param(
    [string]$Username = "EJC_NACIONAL\1089196342",
    [string]$StoreDir = (Join-Path $env:LOCALAPPDATA "DragonRadar\seguridad"),
    [switch]$Actualizar,
    [switch]$InstalarTareaMensual,
    [switch]$OmitirValidacionAD
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Add-Type -AssemblyName System.DirectoryServices.AccountManagement

function Test-WindowsCredential {
    param(
        [Parameter(Mandatory=$true)][System.Management.Automation.PSCredential]$Credential
    )
    $netCred = $Credential.GetNetworkCredential()
    $user = $netCred.UserName
    $domain = $netCred.Domain
    $pass = $netCred.Password

    if (-not [string]::IsNullOrWhiteSpace($domain)) {
        try {
            $ctx = New-Object System.DirectoryServices.AccountManagement.PrincipalContext(
                [System.DirectoryServices.AccountManagement.ContextType]::Domain,
                $domain
            )
            try {
                return @{ Valid = $ctx.ValidateCredentials($user, $pass); Context = "Domain:$domain" }
            } finally { $ctx.Dispose() }
        } catch {
            Write-Warning "No se pudo contactar al controlador de dominio '$domain' ($($_.Exception.Message)). Probando validacion local..."
        }
    }

    $localCtx = New-Object System.DirectoryServices.AccountManagement.PrincipalContext(
        [System.DirectoryServices.AccountManagement.ContextType]::Machine
    )
    try {
        return @{ Valid = $localCtx.ValidateCredentials($user, $pass); Context = "Machine:$env:COMPUTERNAME" }
    } finally { $localCtx.Dispose() }
}

function Protect-StoreDirectoryAcl {
    param([Parameter(Mandatory=$true)][string]$Path)
    try {
        $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        & icacls.exe $Path /inheritance:r /grant:r "${me}:(OI)(CI)F" "SYSTEM:(OI)(CI)F" "BUILTIN\Administrators:(OI)(CI)F" 2>$null | Out-Null
    } catch {
        Write-Warning "No se pudo ajustar ACL estricta en ${Path}: $($_.Exception.Message)"
    }
}

Write-Host "=== Gestion Segura de Credencial Admin (LibreOffice LAN) ===" -ForegroundColor Cyan
New-Item -ItemType Directory -Path $StoreDir -Force | Out-Null
Protect-StoreDirectoryAcl -Path $StoreDir

$credFile = Join-Path $StoreDir "admin-libreoffice.cred.xml"
$metaFile = Join-Path $StoreDir "admin-libreoffice.meta.json"

$mustPrompt = $Actualizar -or (-not (Test-Path -LiteralPath $credFile -PathType Leaf))
if ($mustPrompt) {
    Write-Host "Ingrese la contrasena vigente para '$Username' (se guardara cifrada con Windows DPAPI):" -ForegroundColor Yellow
    $cred = Get-Credential -UserName $Username -Message "Credencial de Administrador para instalacion de LibreOffice en equipos LAN (Radar del Dragon)"
    if (-not $cred) {
        throw "Operacion cancelada por el usuario."
    }

    $validationContext = "Skipped"
    if (-not $OmitirValidacionAD) {
        Write-Host "Validando credencial contra Active Directory / equipo local..." -ForegroundColor Cyan
        $check = Test-WindowsCredential -Credential $cred
        $validationContext = $check.Context
        if (-not $check.Valid) {
            throw "La credencial ingresada para '$($cred.UserName)' fue RECHAZADA ($validationContext). Verifique que la clave este vigente y reintente."
        }
        Write-Host "OK: Credencial validada correctamente ($validationContext)." -ForegroundColor Green
    }

    # Export-Clixml cifra la SecureString usando DPAPI del usuario actual (sin texto plano)
    $cred | Export-Clixml -Path $credFile -Force
    $now = (Get-Date).ToUniversalTime()
    $meta = [ordered]@{
        username            = $cred.UserName
        validation_context  = $validationContext
        last_updated_utc    = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
        last_validated_utc  = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
        next_validation_utc = $now.AddDays(30).ToString("yyyy-MM-ddTHH:mm:ssZ")
        validation_status   = "VALID"
        host                = $env:COMPUTERNAME
    }
    [System.IO.File]::WriteAllText($metaFile, ($meta | ConvertTo-Json -Depth 4) + "`r`n", [System.Text.UTF8Encoding]::new($false))
    Write-Host "Credencial cifrada guardada en: $credFile" -ForegroundColor Green
    Write-Host "Proxima validacion mensual programada: $($meta.next_validation_utc)" -ForegroundColor Gray
} else {
    Write-Host "Ya existe una credencial cifrada en $credFile (use -Actualizar para cambiar la clave)." -ForegroundColor Green
}

if ($InstalarTareaMensual) {
    Write-Host ""
    Write-Host "Registrando tarea programada mensual: Dragon-Radar-ValidarCredencial-Mensual ..." -ForegroundColor Cyan
    $validatorScript = Join-Path $PSScriptRoot "validar-credencial-mensual.ps1"
    if (-not (Test-Path -LiteralPath $validatorScript -PathType Leaf)) {
        throw "No se encontro $validatorScript"
    }

    $taskName = "Dragon-Radar-ValidarCredencial-Mensual"
    $psExe = Join-Path $PSHOME "powershell.exe"
    $argsStr = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$validatorScript`" -StoreDir `"$StoreDir`" -Notify"

    # Se ejecuta bajo la cuenta actual (necesario para descifrar DPAPI) el dia 1 de cada mes a las 08:00
    # y con StartWhenAvailable para ejecutarse al iniciar sesion si el equipo estuvo apagado.
    $xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Valida mensualmente en $env:COMPUTERNAME si cambio la clave de administrador almacenada para instalacion de LibreOffice (Radar del Dragon).</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>$(Get-Date -Format 'yyyy-MM-01')T08:00:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByMonth>
        <DaysOfMonth>
          <Day>1</Day>
        </DaysOfMonth>
        <Months>
          <January /><February /><March /><April /><May /><June />
          <July /><August /><September /><October /><November /><December />
        </Months>
      </ScheduleByMonth>
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$([Security.Principal.WindowsIdentity]::GetCurrent().Name)</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>$psExe</Command>
      <Arguments>$argsStr</Arguments>
      <WorkingDirectory>$PSScriptRoot</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@
    $tmpXml = Join-Path $env:TEMP "dragon-radar-validacion-mensual.xml"
    [System.IO.File]::WriteAllText($tmpXml, $xml, [System.Text.Encoding]::Unicode)
    try {
        & schtasks.exe /Create /TN $taskName /XML $tmpXml /F | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "schtasks.exe fallo creando $taskName (codigo $LASTEXITCODE)."
        }
        Write-Host "OK: Tarea mensual '$taskName' registrada (dia 1 de cada mes a las 08:00 + al iniciar sesion si quedo pendiente)." -ForegroundColor Green
    } finally {
        Remove-Item -LiteralPath $tmpXml -Force -ErrorAction SilentlyContinue
    }
}
