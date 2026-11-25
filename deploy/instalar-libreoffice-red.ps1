<#
.SYNOPSIS
    Regla de validacion, copia e instalacion silenciosa de LibreOffice_26.8.1_Win_x86-64.msi
    desde la carpeta del proyecto Radar del Dragon hacia un equipo local o remoto de la LAN.

.DESCRIPTION
    1. Valida si LibreOffice ya esta instalado en el equipo destino.
    2. Si NO se encuentra instalado:
       - Copia LibreOffice_26.8.1_Win_x86-64.msi desde la carpeta del proyecto (o desde el
         servidor HTTP de Radar del Dragon) al directorio temporal del equipo destino.
       - Ejecuta la instalacion silenciosa (msiexec /i ... /qn /norestart) usando:
           a) La credencial cifrada DPAPI (validando primero su vigencia mensual con
              validar-credencial-mensual.ps1) cuando se indica -UseStoredCredential o
              cuando se instala en un equipo remoto (-ComputerName).
           b) Elevacion UAC estandar de Windows (-Verb RunAs) en ejecucion local sin
              credencial almacenada.
    3. Verifica que soffice.com / soffice.exe haya quedado instalado y elimina el MSI temporal.

.EXAMPLE
    .\deploy\instalar-libreoffice-red.ps1 -ComputerName "PC-OFICINA-05" -UseStoredCredential
    Valida LibreOffice en PC-OFICINA-05; si falta, copia el MSI y lo instala remotamente
    con la credencial cifrada de administrador (validada mensualmente).

.EXAMPLE
    .\deploy\instalar-libreoffice-red.ps1 -UseStoredCredential
    Valida e instala LibreOffice en el equipo local usando la credencial cifrada.

.EXAMPLE
    .\deploy\instalar-libreoffice-red.ps1 -ServerUrl "http://172.23.174.103:8092"
    En otro PC de la LAN: descarga el MSI desde el servidor de Radar y eleva via UAC.
#>

[CmdletBinding()]
param(
    [string]$ComputerName = "",
    [string]$InstallerSource = "",
    [string]$ServerUrl = "",
    [string]$StoreDir = (Join-Path $env:LOCALAPPDATA "DragonRadar\seguridad"),
    [switch]$UseStoredCredential
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$projectRoot = Split-Path -Parent $PSScriptRoot

function Find-ProjectLibreOfficeMsi {
    if (-not [string]::IsNullOrWhiteSpace($InstallerSource)) {
        if (-not (Test-Path -LiteralPath $InstallerSource -PathType Leaf)) {
            throw "El instalador especificado no existe: $InstallerSource"
        }
        return (Resolve-Path -LiteralPath $InstallerSource).Path
    }
    $exact = Join-Path $projectRoot "LibreOffice_26.8.1_Win_x86-64.msi"
    if (Test-Path -LiteralPath $exact -PathType Leaf) {
        return $exact
    }
    $any = Get-ChildItem -LiteralPath $projectRoot -Filter "LibreOffice*.msi" -File -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($any) {
        return $any.FullName
    }
    return $null
}

function Test-LocalLibreOfficeInstalled {
    $candidates = @(
        (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.com"),
        (Join-Path $env:ProgramFiles "LibreOffice\program\soffice.exe"),
        (Join-Path "${env:ProgramFiles(x86)}" "LibreOffice\program\soffice.com"),
        (Join-Path "${env:ProgramFiles(x86)}" "LibreOffice\program\soffice.exe"),
        (Join-Path $env:LOCALAPPDATA "Programs\LibreOffice\program\soffice.com")
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c -PathType Leaf)) { return $c }
    }
    $cmd = Get-Command soffice.com, soffice.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) { return $cmd.Source }
    return $null
}

function Get-ValidatedAdminCredential {
    $validator = Join-Path $PSScriptRoot "validar-credencial-mensual.ps1"
    $credFile = Join-Path $StoreDir "admin-libreoffice.cred.xml"

    if (Test-Path -LiteralPath $credFile -PathType Leaf) {
        Write-Host "[CRED] Validando vigencia de la credencial cifrada de administrador..." -ForegroundColor Cyan
        & $validator -StoreDir $StoreDir
        if ($LASTEXITCODE -eq 0) {
            return (Import-Clixml -LiteralPath $credFile)
        }
        Write-Warning "La credencial almacenada cambio o caduco (codigo $LASTEXITCODE). Ejecute .\deploy\gestionar-credencial-admin.ps1 -Actualizar."
    } else {
        Write-Warning "No existe credencial cifrada en $credFile. Puede guardarla con .\deploy\gestionar-credencial-admin.ps1 -InstalarTareaMensual."
    }

    Write-Host "Solicitando credencial de administrador en pantalla..." -ForegroundColor Yellow
    $prompted = Get-Credential -UserName "EJC_NACIONAL\1089196342" -Message "Ingrese la clave vigente de Administrador para instalar LibreOffice"
    if (-not $prompted) { throw "Instalacion cancelada: se requiere credencial de administrador." }
    return $prompted
}

$isRemote = (-not [string]::IsNullOrWhiteSpace($ComputerName)) -and
    ($ComputerName -ine "localhost") -and
    ($ComputerName -ine "127.0.0.1") -and
    ($ComputerName -ine $env:COMPUTERNAME)

# ==============================================================================
# CASO A: INSTALACION REMOTA EN OTRO EQUIPO DE LA RED LAN
# ==============================================================================
if ($isRemote) {
    Write-Host "=== Despliegue Remoto de LibreOffice en $ComputerName ===" -ForegroundColor Cyan
    $msiPath = Find-ProjectLibreOfficeMsi
    if (-not $msiPath) {
        throw "No se encontro LibreOffice_26.8.1_Win_x86-64.msi en $projectRoot."
    }
    $msiName = Split-Path -Leaf $msiPath
    $cred = Get-ValidatedAdminCredential

    Write-Host "[1/4] Validando si LibreOffice ya esta instalado en $ComputerName..." -ForegroundColor Cyan
    $remoteInstalled = Invoke-Command -ComputerName $ComputerName -Credential $cred -ScriptBlock {
        foreach ($p in @(
            "$env:ProgramFiles\LibreOffice\program\soffice.com",
            "$env:ProgramFiles\LibreOffice\program\soffice.exe",
            "${env:ProgramFiles(x86)}\LibreOffice\program\soffice.com"
        )) {
            if ($p -and (Test-Path -LiteralPath $p -PathType Leaf)) { return $p }
        }
        return $null
    }
    if ($remoteInstalled) {
        Write-Host "[OK] LibreOffice ya se encuentra instalado en ${ComputerName}: $remoteInstalled" -ForegroundColor Green
        exit 0
    }

    Write-Host "[2/4] Copiando $msiName hacia \\$ComputerName\C$\Windows\Temp\DragonRadar ..." -ForegroundColor Cyan
    $driveName = "DRLO_" + (Get-Random -Minimum 1000 -Maximum 9999)
    New-PSDrive -Name $driveName -PSProvider FileSystem -Root "\\$ComputerName\C$" -Credential $cred | Out-Null
    try {
        $remoteStage = "${driveName}:\Windows\Temp\DragonRadar"
        New-Item -ItemType Directory -Path $remoteStage -Force | Out-Null
        Copy-Item -LiteralPath $msiPath -Destination (Join-Path $remoteStage $msiName) -Force
    } finally {
        Remove-PSDrive -Name $driveName -Force -ErrorAction SilentlyContinue
    }

    Write-Host "[3/4] Ejecutando instalacion silenciosa de LibreOffice en $ComputerName..." -ForegroundColor Cyan
    $exitCode = Invoke-Command -ComputerName $ComputerName -Credential $cred -ArgumentList $msiName -ScriptBlock {
        param($fileName)
        $localMsi = "C:\Windows\Temp\DragonRadar\$fileName"
        try {
            $p = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$localMsi`" /qn /norestart" -Wait -PassThru
            return $p.ExitCode
        } finally {
            Remove-Item -LiteralPath $localMsi -Force -ErrorAction SilentlyContinue
        }
    }
    if ($exitCode -notin @(0, 3010)) {
        throw "La instalacion remota de LibreOffice en $ComputerName fallo con codigo $exitCode."
    }

    Write-Host "[4/4] Verificando instalacion final en $ComputerName..." -ForegroundColor Cyan
    $verified = Invoke-Command -ComputerName $ComputerName -Credential $cred -ScriptBlock {
        foreach ($p in @(
            "$env:ProgramFiles\LibreOffice\program\soffice.com",
            "$env:ProgramFiles\LibreOffice\program\soffice.exe"
        )) {
            if (Test-Path -LiteralPath $p -PathType Leaf) { return $p }
        }
        return $null
    }
    if (-not $verified) {
        Write-Warning "msiexec finalizo (codigo $exitCode), pero no se detecto soffice.com inmediatamente en $ComputerName."
    } else {
        Write-Host "[OK] LibreOffice instalado y verificado en ${ComputerName}: $verified" -ForegroundColor Green
    }
    exit 0
}

# ==============================================================================
# CASO B: VALIDACION E INSTALACION EN ESTE EQUIPO (LOCAL)
# ==============================================================================
Write-Host "=== Validacion de LibreOffice en $env:COMPUTERNAME ===" -ForegroundColor Cyan
$existing = Test-LocalLibreOfficeInstalled
if ($existing) {
    Write-Host "[OK] LibreOffice ya esta instalado: $existing" -ForegroundColor Green
    exit 0
}

Write-Host "LibreOffice no se encontro instalado. Preparando copia del instalador..." -ForegroundColor Yellow
$tmpDir = Join-Path $env:TEMP "DragonRadar"
New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

$msiSrc = Find-ProjectLibreOfficeMsi
$localMsi = ""
if ($msiSrc) {
    $localMsi = Join-Path $tmpDir (Split-Path -Leaf $msiSrc)
    Write-Host "Copiando $(Split-Path -Leaf $msiSrc) desde $msiSrc ..." -ForegroundColor Cyan
    Copy-Item -LiteralPath $msiSrc -Destination $localMsi -Force
} elseif (-not [string]::IsNullOrWhiteSpace($ServerUrl)) {
    $baseUrl = $ServerUrl.TrimEnd('/')
    $info = Invoke-RestMethod -Uri "$baseUrl/api/agent/libreoffice-info" -TimeoutSec 10
    if (-not $info.available) {
        throw "El servidor $baseUrl no dispone de instalador de LibreOffice."
    }
    $localMsi = Join-Path $tmpDir $info.filename
    Write-Host "Descargando $($info.filename) desde $baseUrl ..." -ForegroundColor Cyan
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri "$baseUrl/api/agent/libreoffice-installer" -OutFile $localMsi -UseBasicParsing
} else {
    throw "No se encontro LibreOffice_26.8.1_Win_x86-64.msi en $projectRoot ni se especifico -ServerUrl."
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

try {
    Write-Host "Ejecutando instalacion silenciosa de LibreOffice (msiexec /qn /norestart)..." -ForegroundColor Cyan
    if ($isAdmin) {
        $proc = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$localMsi`" /qn /norestart" -Wait -PassThru
    } elseif ($UseStoredCredential) {
        $cred = Get-ValidatedAdminCredential
        $proc = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$localMsi`" /qn /norestart" -Credential $cred -Wait -PassThru
    } else {
        $proc = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$localMsi`" /qn /norestart" -Verb RunAs -Wait -PassThru
    }

    if ($proc.ExitCode -notin @(0, 3010)) {
        throw "msiexec finalizo con codigo de error $($proc.ExitCode)."
    }
} finally {
    Remove-Item -LiteralPath $localMsi -Force -ErrorAction SilentlyContinue
}

$postCheck = Test-LocalLibreOfficeInstalled
if ($postCheck) {
    Write-Host "[OK] LibreOffice instalado correctamente en: $postCheck" -ForegroundColor Green
} else {
    Write-Warning "La instalacion finalizo, pero soffice.com no fue detectado aun (puede requerir reinicio)."
}
