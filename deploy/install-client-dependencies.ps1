[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PythonPath,
    [string]$RequirementsPath = '',
    [string]$WheelhousePath = '',
    [string]$ReportPath = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (-not $RequirementsPath) { $RequirementsPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'requirements.txt' }
if (-not $ReportPath) { $ReportPath = Join-Path $env:LOCALAPPDATA 'RadarDelDragon\data\dependency-check.json' }
$RequirementsPath = (Resolve-Path -LiteralPath $RequirementsPath -ErrorAction Stop).Path
$PythonPath = (Resolve-Path -LiteralPath $PythonPath -ErrorAction Stop).Path

if (-not (Test-Path -LiteralPath $PythonPath -PathType Leaf)) { throw "Python not found: $PythonPath" }
if (-not (Test-Path -LiteralPath $RequirementsPath -PathType Leaf)) { throw "Requirements file not found: $RequirementsPath" }
$pyVersion = & $PythonPath -c "import sys; print('.'.join(map(str, sys.version_info[:3])))"
if ($LASTEXITCODE -ne 0) { throw 'Could not run the selected Python interpreter.' }
$pyVersionParts = $pyVersion.Trim().Split('.')
if ([version]($pyVersionParts[0] + '.' + $pyVersionParts[1]) -lt [version]'3.10') {
    throw "Python 3.10 or newer is required. Found $pyVersion. No runtime was installed."
}
& $PythonPath -m pip --version *> $null
if ($LASTEXITCODE -ne 0) {
    Write-Host 'pip is missing; trying the Python bundled ensurepip bootstrap (no Internet download).' -ForegroundColor Yellow
    & $PythonPath -m ensurepip --upgrade
    if ($LASTEXITCODE -ne 0) { throw 'pip and ensurepip are unavailable. Ask IT for an approved Python runtime with pip.' }
}
$pipVersion = (& $PythonPath -m pip --version | Out-String).Trim()

$probe = @'
import importlib, importlib.metadata, json, re, sys
module_names = {"flask":"flask", "waitress":"waitress", "openpyxl":"openpyxl", "python-docx":"docx", "pypdf":"pypdf"}
rows = []
with open(sys.argv[1], encoding="utf-8-sig") as source:
    for raw in source:
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        match = re.match(r"^([A-Za-z0-9_.-]+)==([^;\s]+)$", line)
        if not match:
            raise SystemExit("Unsupported requirement format: " + line)
        name, expected = match.groups()
        try:
            actual = importlib.metadata.version(name)
            importlib.import_module(module_names.get(name.lower(), name.replace("-", "_")))
            status = "OK" if actual == expected else "VERSION_MISMATCH"
        except importlib.metadata.PackageNotFoundError:
            actual, status = None, "MISSING"
        except Exception as exc:
            actual, status = None, "IMPORT_ERROR: " + type(exc).__name__
        rows.append({"package": name, "required": expected, "installed": actual, "status": status})
print(json.dumps(rows))
'@

function Get-DependencyState {
    $probeBase64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($probe))
    $probeRunner = "exec(__import__('base64').b64decode('$probeBase64'))"
    $json = & $PythonPath -c $probeRunner $RequirementsPath
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect installed Python packages.' }
    return @($json | ConvertFrom-Json)
}

function Assert-Wheelhouse([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) { throw "Wheelhouse not found: $Path" }
    $manifest = Join-Path $Path 'WHEELS-SHA256SUMS.txt'
    if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw "Wheelhouse manifest is missing: $manifest" }
    $entries = @{}
    foreach ($line in Get-Content -LiteralPath $manifest) {
        if (-not $line.Trim()) { continue }
        $parts = $line -split '  ', 2
        if ($parts.Count -ne 2 -or $parts[1] -match '[\\/]' -or $parts[1] -match '^\.' -or $parts[0] -notmatch '^[A-Fa-f0-9]{64}$') {
            throw "Invalid wheelhouse manifest entry: $line"
        }
        $wheel = Join-Path $Path $parts[1]
        if (-not (Test-Path -LiteralPath $wheel -PathType Leaf)) { throw "Wheel missing from wheelhouse: $($parts[1])" }
        $actual = (Get-FileHash -LiteralPath $wheel -Algorithm SHA256).Hash
        if ($actual -ne $parts[0]) { throw "Wheel checksum mismatch: $($parts[1])" }
        $entries[$parts[1]] = $true
    }
    $wheels = @(Get-ChildItem -LiteralPath $Path -Filter '*.whl' -File)
    if ($wheels.Count -eq 0) { throw 'Wheelhouse contains no .whl files.' }
    foreach ($wheel in $wheels) {
        if (-not $entries.ContainsKey($wheel.Name)) { throw "Wheel is not covered by the checksum manifest: $($wheel.Name)" }
    }
    Write-Host "Wheelhouse integrity verified: $($wheels.Count) wheel(s)." -ForegroundColor Green
}

$script:pipCheckResult = 'not_run'
function Write-Report($State, [string]$InstallMode, [string]$Outcome) {
    $parent = Split-Path -Parent $ReportPath
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    [ordered]@{
        checked_at = (Get-Date).ToString('o')
        computer = $env:COMPUTERNAME
        python = $PythonPath
        python_version = $pyVersion.Trim()
        pip_version = $pipVersion
        mode = $InstallMode
        outcome = $Outcome
        pip_check = $script:pipCheckResult
        dependencies = @($State)
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportPath -Encoding UTF8
}

$state = Get-DependencyState
Write-Host "Python $($pyVersion.Trim()) : $PythonPath"
$state | ForEach-Object {
    $color = if ($_.status -eq 'OK') { 'Green' } else { 'Yellow' }
    Write-Host ("{0,-16} {1,-18} installed={2}" -f $_.package, $_.status, $_.installed) -ForegroundColor $color
}

$pipCheckOutput = & $PythonPath -m pip check 2>&1
$pipCheckExit = $LASTEXITCODE
$script:pipCheckResult = if ($pipCheckExit -eq 0) { 'ok' } else { ($pipCheckOutput | Out-String).Trim() }
if ($pipCheckExit -ne 0) { Write-Warning "pip check found inconsistent dependencies: $($script:pipCheckResult)" }
$missing = @($state | Where-Object { $_.status -ne 'OK' })
if ($missing.Count -eq 0 -and $pipCheckExit -eq 0) {
    Write-Report $state 'none' 'already_satisfied'
    Write-Host "All required direct dependencies are installed. Report: $ReportPath" -ForegroundColor Green
    return
}

$mode = 'configured-index'
$pipArgs = @('-m', 'pip', 'install', '--disable-pip-version-check', '--only-binary=:all:', '--requirement', $RequirementsPath)
if ($pipCheckExit -ne 0) { $pipArgs += @('--upgrade', '--upgrade-strategy', 'eager') }
if ($WheelhousePath) {
    Assert-Wheelhouse $WheelhousePath
    $mode = 'offline-wheelhouse'
    $pipArgs += @('--no-index', '--find-links', (Resolve-Path -LiteralPath $WheelhousePath).Path)
}

if ($missing.Count -eq 0) {
    Write-Host "Repairing transitive dependency consistency using $mode ..." -ForegroundColor Cyan
} else {
    Write-Host "Installing $($missing.Count) missing or incorrect package(s) using $mode ..." -ForegroundColor Cyan
}
& $PythonPath @pipArgs
if ($LASTEXITCODE -ne 0) {
    $state = Get-DependencyState
    Write-Report $state $mode 'installation_failed'
    throw "pip failed with exit code $LASTEXITCODE. See dependency report: $ReportPath"
}

$state = Get-DependencyState
$remaining = @($state | Where-Object { $_.status -ne 'OK' })
$pipCheckOutput = & $PythonPath -m pip check 2>&1
$pipCheckExit = $LASTEXITCODE
$script:pipCheckResult = if ($pipCheckExit -eq 0) { 'ok' } else { ($pipCheckOutput | Out-String).Trim() }
if ($remaining.Count -gt 0 -or $pipCheckExit -ne 0) {
    Write-Report $state $mode 'verification_failed'
    throw "Some dependencies remain missing or incorrect. See: $ReportPath"
}
Write-Report $state $mode 'installed_and_verified'
Write-Host "Dependencies installed and verified. Report: $ReportPath" -ForegroundColor Green
return
