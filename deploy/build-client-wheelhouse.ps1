[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PythonPath,
    [string]$RequirementsPath = (Join-Path (Split-Path -Parent $PSScriptRoot) 'requirements.txt'),
    [string]$OutputDirectory = '',
    [string]$PackageZip = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$RequirementsPath = (Resolve-Path -LiteralPath $RequirementsPath -ErrorAction Stop).Path
$PythonPath = (Resolve-Path -LiteralPath $PythonPath -ErrorAction Stop).Path
$target = & $PythonPath -c "import struct,sys; print(str(sys.version_info[0])+'|'+str(sys.version_info[1])+'|'+sys.platform+'|'+str(struct.calcsize(chr(80))*8))"
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the selected Python runtime.' }
$parts = $target.Trim().Split('|')
if ($parts.Count -ne 4 -or $parts[2] -ne 'win32' -or $parts[3] -ne '64') {
    throw "Use an approved 64-bit Windows Python interpreter. Found: $target"
}
if ([version]($parts[0] + '.' + $parts[1]) -lt [version]'3.10') { throw "Python 3.10 or newer is required. Found: $target" }

$tag = 'py' + $parts[0] + $parts[1] + '-win_amd64'
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $root (Join-Path 'dist' ('client-wheelhouse-' + $tag)) }
if (-not $PackageZip) { $PackageZip = $OutputDirectory.TrimEnd('\') + '.zip' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$PackageZip = [IO.Path]::GetFullPath($PackageZip)
if (Test-Path -LiteralPath $OutputDirectory) { throw "Output directory already exists; choose a new path: $OutputDirectory" }
if (Test-Path -LiteralPath $PackageZip) { throw "Output ZIP already exists; choose a new path: $PackageZip" }

$wheelDir = Join-Path $OutputDirectory 'wheelhouse'
New-Item -ItemType Directory -Path $wheelDir -Force | Out-Null
try {
    Write-Host "Preparing Windows x64 wheels for Python $($parts[0]).$($parts[1]) ..." -ForegroundColor Cyan
    Write-Host 'pip uses the configured package index. Use an approved internal mirror or staging machine.'
    & $PythonPath -m pip download --disable-pip-version-check --only-binary=:all: --dest $wheelDir --requirement $RequirementsPath
    if ($LASTEXITCODE -ne 0) { throw 'pip download failed. Check the approved index, proxy, Python version and wheel availability.' }

    $wheels = @(Get-ChildItem -LiteralPath $wheelDir -Filter '*.whl' -File | Sort-Object Name)
    if ($wheels.Count -eq 0) { throw 'pip produced an empty wheelhouse.' }
    $manifestLines = foreach ($wheel in $wheels) {
        "{0}  {1}" -f (Get-FileHash -LiteralPath $wheel.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $wheel.Name
    }
    $manifestLines | Set-Content -LiteralPath (Join-Path $wheelDir 'WHEELS-SHA256SUMS.txt') -Encoding ASCII
    Copy-Item -LiteralPath $RequirementsPath -Destination (Join-Path $OutputDirectory 'requirements.txt')
    @(
        'Dragon Radar Python offline dependencies',
        "Target: Windows x64, Python $($parts[0]).$($parts[1])",
        "Wheels: $($wheels.Count)",
        'Install using install-client-dependencies.ps1 with -WheelhousePath pointing to this wheelhouse folder.',
        'Review and approve package provenance and hashes with IT before LAN distribution.'
    ) | Set-Content -LiteralPath (Join-Path $OutputDirectory 'README-OFFLINE.txt') -Encoding ASCII

    $parent = Split-Path -Parent $PackageZip
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($OutputDirectory, $PackageZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    $zipHash = (Get-FileHash -LiteralPath $PackageZip -Algorithm SHA256).Hash.ToLowerInvariant()
    "$zipHash  $(Split-Path -Leaf $PackageZip)" | Set-Content -LiteralPath ($PackageZip + '.sha256.txt') -Encoding ASCII
    Write-Host "Wheelhouse ready: $OutputDirectory" -ForegroundColor Green
    Write-Host "LAN package: $PackageZip"
    Write-Host "SHA256: $zipHash"
} catch {
    throw
}
