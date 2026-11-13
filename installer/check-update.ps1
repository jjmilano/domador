#Requires -Version 5.1
<#
.SYNOPSIS
    Chequeo de actualizaciones manual y opt-in para Domador.

.DESCRIPTION
    No corre en segundo plano, no manda telemetría y no se ejecuta solo: se
    dispara desde el acceso directo "Buscar actualizaciones" o a mano.

    Flujo:
      1. lee update.json del release (HTTPS) o un manifiesto local con -ManifestPath
      2. compara versiones
      3. descarga el setup a %TEMP%
      4. verifica SHA-256 contra el manifiesto (obligatorio)
      5. verifica Authenticode si hay editor esperado o -RequireSignature
      6. pide confirmación y corre el setup per-user en modo silencioso

    El setup es per-user (PrivilegesRequired=lowest): actualizar NO pide UAC.
    Si el usuario tenía la tarea programada elevada, sigue apuntando a la misma
    ruta y sigue funcionando; el instalador no la duplica.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File check-update.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File check-update.ps1 -DryRun -ManifestPath .\update.json

.EXITCODES
    0 = al día, actualización instalada o dry-run correcto; 1 = error; 2 = uso.
#>
[CmdletBinding()]
param(
    [string]$ManifestUrl = 'https://github.com/jjmilano/domador/releases/latest/download/update.json',
    [string]$ManifestPath,
    [string]$CurrentVersion,
    [string]$ExpectedPublisher,
    [string]$InstallDir,
    [switch]$RequireSignature,
    [switch]$Yes,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    Write-Host "ERROR: $Message" -ForegroundColor Red
    exit 1
}

function Get-Sha256([string]$File) {
    $stream = [System.IO.File]::OpenRead($File)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $bytes = $sha.ComputeHash($stream) } finally { $sha.Dispose() }
    }
    finally { $stream.Dispose() }
    return ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
}

function Get-Prop($Object, [string]$Name, $Default = $null) {
    if ($null -ne $Object -and $Object.PSObject.Properties[$Name]) { return $Object.$Name }
    return $Default
}

function ConvertTo-Version([string]$Value) {
    $clean = $Value.Trim()
    $clean = $clean -replace '^[vV]', ''
    if ($clean -notmatch '^(\d+(?:\.\d+){0,3})') { Fail "Versión inválida: '$Value'." }
    $parts = $Matches[1].Split('.')
    while ($parts.Count -lt 2) { $parts += '0' }
    return [version]($parts -join '.')
}

if (-not $InstallDir) { $InstallDir = $PSScriptRoot }
$exe = Join-Path $InstallDir 'domador.exe'
if (-not (Test-Path -LiteralPath $exe)) { Fail "No encontré domador.exe en '$InstallDir'." }

if (-not $CurrentVersion) {
    $raw = (& $exe --version 2>$null | Out-String).Trim()
    if ($raw -match '(\d+(?:\.\d+){1,3})') { $CurrentVersion = $Matches[1] }
    else { Fail "No pude leer la versión instalada; pasala con -CurrentVersion." }
}
$installed = ConvertTo-Version $CurrentVersion

# --- Manifiesto -------------------------------------------------------------
if ($ManifestPath) {
    if (-not (Test-Path -LiteralPath $ManifestPath)) { Fail "No existe '$ManifestPath'." }
    Write-Host "Manifiesto local: $ManifestPath"
    $manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
} else {
    if ($ManifestUrl -notmatch '^https://') { Fail 'La URL del manifiesto debe ser HTTPS.' }
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Write-Host "Consultando $ManifestUrl"
    $manifest = Invoke-RestMethod -Uri $ManifestUrl -UseBasicParsing -TimeoutSec 30 `
        -Headers @{ 'User-Agent' = "Domador/$CurrentVersion"; 'Accept' = 'application/json' }
}

$app = Get-Prop $manifest 'app'
if ($app -and $app -ne 'domador') { Fail "El manifiesto no es de Domador (app=$app)." }

$latestRaw = [string](Get-Prop $manifest 'version' '')
if (-not $latestRaw) { Fail 'El manifiesto no tiene "version".' }
$latest = ConvertTo-Version $latestRaw

Write-Host "Instalada: $CurrentVersion   Disponible: $latestRaw"
if ($latest -le $installed) {
    Write-Host 'Estás al día.' -ForegroundColor Green
    exit 0
}

$assets = @(Get-Prop $manifest 'assets' @())
$asset = $assets | Where-Object { (Get-Prop $_ 'kind' '') -eq 'installer' } | Select-Object -First 1
if (-not $asset) {
    $asset = $assets | Where-Object { ([string](Get-Prop $_ 'name' '')) -match '(?i)setup.*\.exe$' } | Select-Object -First 1
}
if (-not $asset) { Fail 'El manifiesto no incluye un instalador (.exe).' }

$url = [string](Get-Prop $asset 'url' '')
$expectedHash = ([string](Get-Prop $asset 'sha256' '')).ToLowerInvariant()
$expectedSize = Get-Prop $asset 'size' $null
$name = [string](Get-Prop $asset 'name' "Domador-$latestRaw-x64-setup.exe")

if ($url -notmatch '^https://') {
    # Sólo se aceptan rutas locales para pruebas explícitas con -ManifestPath.
    if (-not ($ManifestPath -and (Test-Path -LiteralPath $url))) {
        Fail "El asset no es HTTPS ni un archivo local válido: $url"
    }
}
if ($expectedHash -notmatch '^[0-9a-f]{64}$') {
    Fail 'El manifiesto no trae un sha256 válido para el instalador; no se descarga nada.'
}

# --- Descarga ---------------------------------------------------------------
$tmp = Join-Path $env:TEMP ("Domador-{0}-setup.exe" -f $latest.ToString())
if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }

Write-Host "Descargando $name"
if (Test-Path -LiteralPath $url) {
    Copy-Item -LiteralPath $url -Destination $tmp -Force
} else {
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing -TimeoutSec 300
}

$size = (Get-Item -LiteralPath $tmp).Length
if ($expectedSize -and ([int64]$expectedSize -ne $size)) {
    Remove-Item -LiteralPath $tmp -Force
    Fail "Tamaño inesperado: $size bytes (manifiesto: $expectedSize)."
}

$actualHash = Get-Sha256 $tmp
if ($actualHash -ne $expectedHash) {
    Remove-Item -LiteralPath $tmp -Force
    Fail "SHA-256 no coincide. Esperado $expectedHash, real $actualHash. Se descartó el archivo."
}
Write-Host 'SHA-256 OK.' -ForegroundColor Green

# --- Firma ------------------------------------------------------------------
$publisher = if ($ExpectedPublisher) { $ExpectedPublisher } else { [string](Get-Prop $manifest 'publisher' '') }
$sig = $null
$sigError = ''
try { $sig = Get-AuthenticodeSignature -LiteralPath $tmp }
catch { $sigError = $_.Exception.Message }

if ($null -eq $sig) {
    if ($RequireSignature -or $ExpectedPublisher) {
        Remove-Item -LiteralPath $tmp -Force
        Fail "No pude verificar la firma Authenticode: $sigError"
    }
    Write-Host "Aviso: no pude leer la firma ($sigError). El SHA-256 coincide; seguí sólo si confiás en el origen." -ForegroundColor Yellow
}
elseif ($sig.Status -eq 'Valid') {
    Write-Host ("Firma válida: {0}" -f $sig.SignerCertificate.Subject) -ForegroundColor Green
    if ($publisher -and $sig.SignerCertificate.Subject -notlike "*$publisher*") {
        Remove-Item -LiteralPath $tmp -Force
        Fail "El instalador está firmado por un editor distinto de '$publisher'."
    }
    if (-not $sig.TimeStamperCertificate) {
        Write-Host 'Aviso: la firma no tiene timestamp RFC3161.' -ForegroundColor Yellow
    }
} else {
    if ($RequireSignature -or $ExpectedPublisher) {
        Remove-Item -LiteralPath $tmp -Force
        Fail "Firma Authenticode inválida o ausente ($($sig.Status)); no se instala."
    }
    Write-Host "Aviso: el instalador no está firmado ($($sig.Status)). Verificá el SHA-256 publicado." -ForegroundColor Yellow
}

Write-Host ''
Write-Host "Instalador verificado: $tmp"
if ($DryRun) {
    Write-Host 'Dry-run: no se ejecuta el setup.' -ForegroundColor Cyan
    exit 0
}

if (-not $Yes) {
    $answer = Read-Host '¿Instalar esta actualización ahora? [s/N]'
    if ($answer -notmatch '^(s|si|sí|y|yes)$') {
        Write-Host 'Cancelado. El archivo queda en %TEMP% para inspección.'
        exit 0
    }
}

Write-Host 'Ejecutando el instalador (per-user, sin UAC)...'
$proc = Start-Process -FilePath $tmp -ArgumentList '/SILENT', '/SP-', '/NORESTART', '/CLOSEAPPLICATIONS' -Wait -PassThru
if ($proc.ExitCode -ne 0) { Fail "El instalador terminó con código $($proc.ExitCode)." }

Write-Host 'Actualización aplicada. Abrí Domador para confirmar la versión.' -ForegroundColor Green
exit 0
