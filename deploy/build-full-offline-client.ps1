[CmdletBinding()]
param(
    [string]$PythonInstallerPath = '',
    [string]$PackageName = 'Radar-del-Dragon-Cliente-Offline-Python314-win-x64'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
if (-not $PythonInstallerPath) { $PythonInstallerPath = Join-Path $dist 'python-3.14.10-amd64.exe' }
$PythonInstallerPath = (Resolve-Path -LiteralPath $PythonInstallerPath -ErrorAction Stop).Path
$wheelSource = Join-Path $dist 'client-wheelhouse-py314-win_amd64\wheelhouse'
$stage = Join-Path $dist $PackageName
$zipPath = Join-Path $dist ($PackageName + '.zip')
$zipHashPath = $zipPath + '.sha256.txt'

foreach ($output in @($stage, $zipPath, $zipHashPath)) {
    if (Test-Path -LiteralPath $output) { throw "No se sobrescribe una salida existente: $output" }
}
if (-not (Test-Path -LiteralPath $wheelSource -PathType Container)) { throw "No se encontró wheelhouse: $wheelSource" }
$signature = Get-AuthenticodeSignature -LiteralPath $PythonInstallerPath
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Python Software Foundation') {
    throw 'El instalador de Python no tiene una firma Authenticode válida de Python Software Foundation.'
}

New-Item -ItemType Directory -Path $stage -Force | Out-Null
try {
    $agent = Join-Path $stage 'RadarAgent'
    $pythonDir = Join-Path $stage 'Python'
    $wheelDest = Join-Path $stage 'wheelhouse'
    New-Item -ItemType Directory -Path $agent, $pythonDir, $wheelDest -Force | Out-Null

    foreach ($file in @('server.py', 'radar_indexer.py', 'radar_preview.py', 'requirements.txt')) {
        Copy-Item -LiteralPath (Join-Path $root $file) -Destination $agent
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'activate-client.ps1') -Destination $agent
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-client-dependencies.ps1') -Destination $agent
    Copy-Item -LiteralPath (Join-Path $root 'ui') -Destination $agent -Recurse
    $config = Join-Path $agent 'config'
    New-Item -ItemType Directory -Path $config -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'config\site_config.example.json') -Destination $config
    Copy-Item -LiteralPath $PythonInstallerPath -Destination (Join-Path $pythonDir 'python-3.14.10-amd64.exe')
    Copy-Item -Path (Join-Path $wheelSource '*') -Destination $wheelDest -Recurse

    @'
Radar del Dragon - instalacion offline para Windows x64

1. Extraiga el ZIP completo en una carpeta local con permisos de escritura del usuario.
2. Ejecute INSTALL_OFFLINE.bat. PowerShell usa la politica configurada por su organizacion; no se cambia ni se omite.
3. El instalador verifica los hashes del paquete, valida la firma del instalador Python,
   instala Python 3.14 para el usuario actual si hace falta, instala las dependencias desde wheelhouse
   y activa Radar localmente.
4. Abra http://127.0.0.1:8092 en el navegador del equipo.

Los indices, la configuracion local y los resultados se guardan en el perfil local del usuario.
La instalacion no necesita acceso a Internet ni permisos de administrador en condiciones normales.
Si una politica corporativa o CrowdStrike bloquea un archivo o script, detengase y solicite a TI
que apruebe y despliegue este paquete. No desactive controles de seguridad.

Este paquete es Windows x64. El manifiesto verifica integridad frente a cambios accidentales;
el archivo SHA-256 externo permite comparar el ZIP recibido. Solicite el hash por un canal confiable.
'@ | Set-Content -LiteralPath (Join-Path $stage 'README-INSTALACION.txt') -Encoding UTF8

    @'
@echo off
setlocal
powershell.exe -NoLogo -NoProfile -File "%~dp0install-client-offline.ps1"
set "RESULT=%ERRORLEVEL%"
if not "%RESULT%"=="0" (
  echo.
  echo La instalacion termino con error %RESULT%. Revise el mensaje y consulte a TI si una politica lo bloqueo.
  pause
)
exit /b %RESULT%
'@ | Set-Content -LiteralPath (Join-Path $stage 'INSTALL_OFFLINE.bat') -Encoding ASCII

    $offlineScript = Join-Path $PSScriptRoot 'install-client-offline.ps1'
    Copy-Item -LiteralPath $offlineScript -Destination $stage

    $manifestLines = Get-ChildItem -LiteralPath $stage -File -Recurse |
        Where-Object { $_.Name -ne 'MANIFEST-SHA256.txt' } |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($stage.Length + 1).Replace('\', '/')
            '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
        }
    if (($manifestLines | Measure-Object).Count -lt 10) { throw 'El contenido ensamblado parece incompleto.' }
    $manifestLines | Set-Content -LiteralPath (Join-Path $stage 'MANIFEST-SHA256.txt') -Encoding ASCII

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $zipPath, [IO.Compression.CompressionLevel]::Optimal, $false)
    $zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    '{0}  {1}' -f $zipHash, (Split-Path -Leaf $zipPath) | Set-Content -LiteralPath $zipHashPath -Encoding ASCII

    Write-Host "Paquete completo: $zipPath" -ForegroundColor Green
    Write-Host "Archivos en manifiesto: $(($manifestLines | Measure-Object).Count)" -ForegroundColor Green
    Write-Host "SHA-256 ZIP: $zipHash" -ForegroundColor Green
} catch {
    # Keep the partial staging directory for diagnosis; never remove user data automatically.
    throw
}
