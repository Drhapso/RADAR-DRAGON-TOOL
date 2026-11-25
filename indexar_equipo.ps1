<#
.SYNOPSIS
    Script de indexacion integral para Dragon Radar Tool (Motor Python/SQLite FTS5, sin .exe).
    Detecta automaticamente todas las unidades locales (C:, D:, E:, etc.),
    omite directorios del sistema operativo y archivos protegidos, e indexa
    todos los documentos, archivos y codigo.
#>

[CmdletBinding()]
param(
    [switch]$SoloDocumentos
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
if ($PSScriptRoot) {
    Set-Location $PSScriptRoot
}

$Host.UI.RawUI.WindowTitle = "Dragon Radar Tool - Indexador Python de Unidades Locales"

Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host "|              DRAGON RADAR TOOL - INDEXADOR DE EQUIPO                 |" -ForegroundColor Green
Write-Host "|       Motor Python / SQLite FTS5 (sin ejecutables .exe)              |" -ForegroundColor Green
Write-Host "+======================================================================+" -ForegroundColor Green
Write-Host ""

$runLan = Join-Path $PSScriptRoot "deploy\run-lan.ps1"
if (-not (Test-Path -LiteralPath $runLan -PathType Leaf)) {
    throw "No se encontro $runLan."
}

$sw = [System.Diagnostics.Stopwatch]::StartNew()
& $runLan -SyncOnly
$exitCode = $LASTEXITCODE
$sw.Stop()

Write-Host ""
if ($exitCode -eq 0) {
    Write-Host "+======================================================================+" -ForegroundColor Green
    Write-Host "|              INDEXACION INTEGRAL FINALIZADA CON EXITO                |" -ForegroundColor Green
    Write-Host "+======================================================================+" -ForegroundColor Green
    Write-Host ("Tiempo total transcurrido: {0}" -f $sw.Elapsed.ToString()) -ForegroundColor Yellow
} else {
    Write-Host "[ERROR] La indexacion finalizo con codigo $exitCode." -ForegroundColor Red
}
exit $exitCode
