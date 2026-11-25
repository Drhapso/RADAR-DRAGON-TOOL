param(
    [string]$AuthorizedSubnet = "",
    [ValidateRange(1,65535)][int]$Port = 8092,
    [switch]$Apply
)
$ErrorActionPreference = "Stop"

# Si no se indica -AuthorizedSubnet, intenta leer authorized_subnet de config\site_config.json
if ([string]::IsNullOrWhiteSpace($AuthorizedSubnet)) {
    $rootCandidates = @(
        (Split-Path -Parent $PSScriptRoot),
        (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
    )
    foreach ($base in $rootCandidates) {
        $cfgFile = Join-Path $base "config\site_config.json"
        if (Test-Path -LiteralPath $cfgFile -PathType Leaf) {
            try {
                $cfg = Get-Content -LiteralPath $cfgFile -Raw | ConvertFrom-Json
                if (-not [string]::IsNullOrWhiteSpace($cfg.authorized_subnet)) {
                    $AuthorizedSubnet = [string]$cfg.authorized_subnet
                    break
                }
            } catch {}
        }
    }
}

if ([string]::IsNullOrWhiteSpace($AuthorizedSubnet)) {
    throw "Indique -AuthorizedSubnet con una subred IPv4 CIDR explicita (ejemplo: 172.23.174.0/24) o configurela en config\site_config.json."
}
if ($AuthorizedSubnet -notmatch '^\d{1,3}(\.\d{1,3}){3}/\d{1,2}$') {
    throw "Indique una subred IPv4 CIDR explicita (ejemplo: 172.23.174.0/24)."
}
$parts = $AuthorizedSubnet.Split("/")
$address = $null
if (-not [System.Net.IPAddress]::TryParse($parts[0], [ref]$address) -or [int]$parts[1] -lt 1 -or [int]$parts[1] -gt 32) {
    throw "Subred IPv4 no valida."
}
if (-not $Apply) {
    Write-Host "Diagnostico: TCP $Port para $AuthorizedSubnet, perfiles Domain/Private. No se modifica el firewall."
    Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue | Select-Object LocalAddress,LocalPort,OwningProcess
    return
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Se requieren permisos administrativos para aplicar reglas de Firewall."
}
$name = "Dragon-Radar-LAN-$Port"
$existing = Get-NetFirewallRule -Name $name -ErrorAction SilentlyContinue
if ($existing) {
    throw "La regla $name ya existe. Revisela antes de sustituirla."
}
New-NetFirewallRule -Name $name -DisplayName "Radar del Dragon LAN ($Port)" -Direction Inbound -Action Allow -Protocol TCP -LocalPort $Port -RemoteAddress $AuthorizedSubnet -Profile Domain,Private | Out-Null
Write-Host "Regla $name creada para TCP $Port desde $AuthorizedSubnet (Domain/Private). No se modificaron otros puertos ni servicios."
