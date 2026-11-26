[CmdletBinding()]
param(
    [string]$PackageRoot = $PSScriptRoot
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PackageRoot = (Resolve-Path -LiteralPath $PackageRoot -ErrorAction Stop).Path.TrimEnd('\')
$manifest = Join-Path $PackageRoot 'MANIFEST-SHA256.txt'

function Assert-PackageManifest {
    if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) { throw 'Package manifest is missing.' }
    $rootPrefix = $PackageRoot + '\'
    $verified = @{}
    foreach ($line in Get-Content -LiteralPath $manifest) {
        if (-not $line.Trim()) { continue }
        $parts = $line -split '  ', 2
        if ($parts.Count -ne 2 -or $parts[0] -notmatch '^[A-Fa-f0-9]{64}$' -or [IO.Path]::IsPathRooted($parts[1]) -or $parts[1] -match '(^|[\\/])\.\.([\\/]|$)') {
            throw "Invalid package manifest entry: $line"
        }
        $relative = $parts[1].Replace('/', '\')
        $path = [IO.Path]::GetFullPath((Join-Path $PackageRoot $relative))
        if (-not $path.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Package path escapes its folder: $relative" }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Package file is missing: $relative" }
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($actual -ne $parts[0]) { throw "Package checksum mismatch: $relative" }
        $verified[$relative] = $true
    }
    if ($verified.Count -lt 10) { throw 'The package manifest looks incomplete.' }
    Write-Host "Verified $($verified.Count) package files." -ForegroundColor Green
}

function Test-Python314([string]$Candidate) {
    if (-not $Candidate -or -not (Test-Path -LiteralPath $Candidate -PathType Leaf)) { return $false }
    try {
        $probe = "import platform,struct,sys; print(str(sys.version_info[0])+'|'+str(sys.version_info[1])+'|'+platform.python_implementation()+'|'+str(struct.calcsize(chr(80))*8))"
        $result = & $Candidate -c $probe 2>$null
        return ($LASTEXITCODE -eq 0 -and $result.Trim() -eq '3|14|CPython|64')
    } catch { return $false }
}

function Find-Python314 {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python314\python.exe'),
        (Join-Path $env:ProgramFiles 'Python314\python.exe')
    )
    $command = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($command -and $command.Source) { $candidates += $command.Source }
    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        if (Test-Python314 $candidate) { return $candidate }
    }
    $launcher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($launcher -and $launcher.Source) {
        try {
            $resolved = & $launcher.Source -3.14 -c 'import sys; print(sys.executable)' 2>$null
            if ($LASTEXITCODE -eq 0 -and $resolved -and (Test-Python314 ($resolved | Select-Object -First 1).Trim())) {
                return ($resolved | Select-Object -First 1).Trim()
            }
        } catch {}
    }
    return $null
}

Assert-PackageManifest
if (-not [Environment]::Is64BitOperatingSystem) { throw 'This offline package requires 64-bit Windows.' }

$pythonPath = Find-Python314
if (-not $pythonPath) {
    $pythonInstaller = Join-Path $PackageRoot 'Python\python-3.14.10-amd64.exe'
    if (-not (Test-Path -LiteralPath $pythonInstaller -PathType Leaf)) { throw 'Python 3.14.10 x64 installer is missing from the package.' }
    $signature = Get-AuthenticodeSignature -LiteralPath $pythonInstaller
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Python Software Foundation') {
        throw 'The Python installer signature is invalid or is not from Python Software Foundation.'
    }
    $pythonTarget = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python314'
    New-Item -ItemType Directory -Path $pythonTarget -Force | Out-Null
    Write-Host 'Installing the signed Python runtime for this user only ...' -ForegroundColor Cyan
    $installerArgs = '/quiet InstallAllUsers=0 TargetDir="' + $pythonTarget + '" PrependPath=0 Include_launcher=0 Include_pip=1 Include_test=0'
    $process = Start-Process -FilePath $pythonInstaller -ArgumentList $installerArgs -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -notin @(0, 3010)) { throw "Python installer returned exit code $($process.ExitCode). No policy was changed." }
    $pythonPath = Join-Path $pythonTarget 'python.exe'
    if (-not (Test-Python314 $pythonPath)) { throw 'Python 3.14 could not be verified after installation.' }
}

$agentRoot = Join-Path $PackageRoot 'RadarAgent'
$wheelhouse = Join-Path $PackageRoot 'wheelhouse'
foreach ($required in @('server.py', 'radar_indexer.py', 'radar_preview.py', 'requirements.txt', 'activate-client.ps1', 'install-client-dependencies.ps1', 'ui\index.html')) {
    if (-not (Test-Path -LiteralPath (Join-Path $agentRoot $required) -PathType Leaf)) { throw "Radar application file is missing: $required" }
}
if (-not (Test-Path -LiteralPath (Join-Path $wheelhouse 'WHEELS-SHA256SUMS.txt') -PathType Leaf)) { throw 'Offline dependency wheelhouse is missing.' }

$activation = Join-Path $agentRoot 'activate-client.ps1'
Write-Host 'Installing Radar locally and resolving all packages from the offline wheelhouse ...' -ForegroundColor Cyan
& $activation -ServerUrl 'http://127.0.0.1:8092' -WheelhousePath $wheelhouse -PythonPath $pythonPath
if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { throw "Radar activation returned exit code $LASTEXITCODE." }
Write-Host 'Offline installation complete. The application and its index remain on this computer.' -ForegroundColor Green
