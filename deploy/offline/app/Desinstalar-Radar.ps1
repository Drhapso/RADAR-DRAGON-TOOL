param(
    [switch]$KeepData,
    [switch]$Force
)
$ErrorActionPreference = "Stop"
$root = (Get-Item -LiteralPath $PSScriptRoot).FullName.TrimEnd("\")
if (-not (Test-Path -LiteralPath (Join-Path $root "dragon.exe"))) {
    throw "Esta carpeta no es una instalacion de Radar del Dragon."
}
if (-not $Force) {
    $answer = Read-Host "Se desinstalara Radar del Dragon de $root. Continuar? (S/N)"
    if ($answer -notmatch '^[sS]') { Write-Host "Cancelado."; exit 0 }
    if (-not $KeepData) {
        $KeepData = (Read-Host "Conservar configuracion e indices locales? (S/N)") -match '^[sS]'
    }
}
$running = @(Get-Process -Name dragon -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and $_.Path.StartsWith($root + "\", [StringComparison]::OrdinalIgnoreCase)
})
foreach ($process in $running) { Stop-Process -Id $process.Id -Force }
if ($running.Count) { Start-Sleep -Seconds 2 }

Set-Location -LiteralPath $env:TEMP
if ($KeepData) {
    $backup = "$root-datos-" + (Get-Date -Format "yyyyMMdd-HHmmss")
    New-Item -ItemType Directory -Path $backup | Out-Null
    foreach ($item in @("config\site_config.json", ".dragon-data", ".dragon-index", ".dragon-shared-data", ".dragon-shared-index", ".dragon-models")) {
        $source = Join-Path $root $item
        if (Test-Path -LiteralPath $source) {
            $destParent = Split-Path -Parent (Join-Path $backup $item)
            if (-not (Test-Path -LiteralPath $destParent)) { New-Item -ItemType Directory -Path $destParent -Force | Out-Null }
            Copy-Item -LiteralPath $source -Destination (Join-Path $backup $item) -Recurse -Force
        }
    }
    Write-Host "Datos conservados en $backup"
}
foreach ($link in @(
    (Join-Path ([Environment]::GetFolderPath("Programs")) "Radar del Dragon"),
    (Join-Path ([Environment]::GetFolderPath("Desktop")) "Radar del Dragon.lnk")
)) {
    if (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Recurse -Force }
}
Remove-Item -LiteralPath $root -Recurse -Force
Write-Host "Radar del Dragon se desinstalo correctamente."
if (-not $Force) { Read-Host "Pulse Enter para cerrar" | Out-Null }
