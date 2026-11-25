<#
.SYNOPSIS
    Valida mensualmente (o bajo demanda) si la credencial cifrada de administrador sigue
    vigente en Active Directory / equipo local o si la contrasena cambio.

.DESCRIPTION
    Codigos de salida:
      0 = Credencial vigente y validada correctamente.
      2 = Contrasena cambiada o rechazada (requiere ejecutar gestionar-credencial-admin.ps1 -Actualizar).
      3 = Credencial no configurada aun o ilegible.
#>

[CmdletBinding()]
param(
    [string]$StoreDir = (Join-Path $env:LOCALAPPDATA "DragonRadar\seguridad"),
    [int]$MaxDaysWithoutValidation = 30,
    [switch]$Notify
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Add-Type -AssemblyName System.DirectoryServices.AccountManagement

$credFile = Join-Path $StoreDir "admin-libreoffice.cred.xml"
$metaFile = Join-Path $StoreDir "admin-libreoffice.meta.json"
$logDir   = Join-Path $StoreDir "logs"
$logFile  = Join-Path $logDir "validacion-credencial-mensual.log"
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

function Write-Audit([string]$Level, [string]$Msg) {
    $line = "{0} [{1}] [{2}] {3}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $Level, $env:COMPUTERNAME, $Msg
    Add-Content -LiteralPath $logFile -Value $line -Encoding UTF8
    $color = switch ($Level) {
        "OK"    { "Green" }
        "WARN"  { "Yellow" }
        "ERROR" { "Red" }
        default { "Cyan" }
    }
    Write-Host $line -ForegroundColor $color
}

function Show-AlertBox([string]$Text, [string]$Icon = "Warning") {
    if (-not $Notify) { return }
    try {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.MessageBox]::Show($Text, "Radar del Dragon - Validacion Mensual de Credencial", "OK", $Icon) | Out-Null
    } catch {}
}

if (-not (Test-Path -LiteralPath $credFile -PathType Leaf)) {
    $msg = "No existe credencial cifrada en $credFile. Ejecute .\deploy\gestionar-credencial-admin.ps1 -InstalarTareaMensual."
    Write-Audit "WARN" $msg
    Show-AlertBox $msg "Warning"
    exit 3
}

try {
    $cred = Import-Clixml -LiteralPath $credFile
} catch {
    $msg = "No se pudo descifrar $credFile (debe validarse bajo la misma cuenta que la guardo): $($_.Exception.Message)"
    Write-Audit "ERROR" $msg
    Show-AlertBox $msg "Error"
    exit 3
}

$netCred = $cred.GetNetworkCredential()
$user = $netCred.UserName
$domain = $netCred.Domain
$pass = $netCred.Password
$valid = $false
$ctxLabel = "Unknown"

if (-not [string]::IsNullOrWhiteSpace($domain)) {
    try {
        $ctx = New-Object System.DirectoryServices.AccountManagement.PrincipalContext(
            [System.DirectoryServices.AccountManagement.ContextType]::Domain,
            $domain
        )
        try {
            $valid = $ctx.ValidateCredentials($user, $pass)
            $ctxLabel = "Domain:$domain"
        } finally { $ctx.Dispose() }
    } catch {
        Write-Audit "WARN" "Dominio '$domain' no accesible temporalmente ($($_.Exception.Message)); intentando validacion local..."
    }
}

if (-not $valid -and $ctxLabel -eq "Unknown") {
    $localCtx = New-Object System.DirectoryServices.AccountManagement.PrincipalContext(
        [System.DirectoryServices.AccountManagement.ContextType]::Machine
    )
    try {
        $valid = $localCtx.ValidateCredentials($user, $pass)
        $ctxLabel = "Machine:$env:COMPUTERNAME"
    } finally { $localCtx.Dispose() }
}

$now = (Get-Date).ToUniversalTime()
$existingUpdated = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
if (Test-Path -LiteralPath $metaFile -PathType Leaf) {
    try {
        $oldMeta = Get-Content -LiteralPath $metaFile -Raw | ConvertFrom-Json
        if ($oldMeta.last_updated_utc) { $existingUpdated = [string]$oldMeta.last_updated_utc }
    } catch {}
}

if ($valid) {
    $meta = [ordered]@{
        username            = $cred.UserName
        validation_context  = $ctxLabel
        last_updated_utc    = $existingUpdated
        last_validated_utc  = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
        next_validation_utc = $now.AddDays($MaxDaysWithoutValidation).ToString("yyyy-MM-ddTHH:mm:ssZ")
        validation_status   = "VALID"
        host                = $env:COMPUTERNAME
    }
    [System.IO.File]::WriteAllText($metaFile, ($meta | ConvertTo-Json -Depth 4) + "`r`n", [System.Text.UTF8Encoding]::new($false))
    Write-Audit "OK" "Credencial '$($cred.UserName)' VIGENTE ($ctxLabel). Proxima validacion mensual: $($meta.next_validation_utc)"
    exit 0
} else {
    $meta = [ordered]@{
        username            = $cred.UserName
        validation_context  = $ctxLabel
        last_updated_utc    = $existingUpdated
        last_validated_utc  = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
        next_validation_utc = $now.ToString("yyyy-MM-ddTHH:mm:ssZ")
        validation_status   = "PASSWORD_CHANGED_OR_INVALID"
        host                = $env:COMPUTERNAME
    }
    [System.IO.File]::WriteAllText($metaFile, ($meta | ConvertTo-Json -Depth 4) + "`r`n", [System.Text.UTF8Encoding]::new($false))
    $alert = "ATENCION: La contrasena de administrador para '$($cred.UserName)' CAMBIO o ya no es valida ($ctxLabel). Ejecute: .\deploy\gestionar-credencial-admin.ps1 -Actualizar"
    Write-Audit "ERROR" $alert
    Show-AlertBox $alert "Error"
    exit 2
}
