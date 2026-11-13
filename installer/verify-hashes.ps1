#Requires -Version 5.1
<#
.SYNOPSIS
    Verifica archivos contra un manifiesto SHA256SUMS.txt.

.DESCRIPTION
    Formato del manifiesto (el mismo de sha256sum):
        <64 hex>  <nombre>
    Ignora líneas vacías y las que empiezan con '#'.

    Con -RequireSignature exige Authenticode válido en cada .exe y, si se pasa
    -ExpectedPublisher, que el sujeto del certificado contenga ese texto.
    Un hash correcto prueba integridad de transporte, no autoría: la firma es
    lo único que prueba quién publicó el binario.

.EXAMPLE
    powershell -NoProfile -File installer\verify-hashes.ps1 -Path dist

.EXAMPLE
    powershell -NoProfile -File installer\verify-hashes.ps1 -Path dist -RequireSignature -ExpectedPublisher "José Milano"

.EXITCODES
    0 = todo coincide; 1 = mismatch/falta archivo/firma inválida; 2 = uso.
#>
[CmdletBinding()]
param(
    [string]$Path = '.',
    [string]$Manifest,
    [switch]$RequireSignature,
    [string]$ExpectedPublisher
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Sha256([string]$File) {
    $stream = [System.IO.File]::OpenRead($File)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $bytes = $sha.ComputeHash($stream) } finally { $sha.Dispose() }
    }
    finally { $stream.Dispose() }
    return ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
}

function Fail([string]$Message) {
    Write-Host "ERROR: $Message" -ForegroundColor Red
    exit 2
}

if (-not (Test-Path -LiteralPath $Path)) { Fail "No existe '$Path'." }

$item = Get-Item -LiteralPath $Path
if ($item.PSIsContainer) {
    $baseDir = $item.FullName
    $manifestPath = if ($Manifest) { Join-Path $baseDir $Manifest } else { Join-Path $baseDir 'SHA256SUMS.txt' }
} else {
    $baseDir = Split-Path -Parent $item.FullName
    $manifestPath = if ($Manifest) { Join-Path $baseDir $Manifest } else { Join-Path $baseDir 'SHA256SUMS.txt' }
}

if (-not (Test-Path -LiteralPath $manifestPath)) { Fail "No encontré el manifiesto '$manifestPath'." }

$entries = @()
foreach ($line in Get-Content -LiteralPath $manifestPath) {
    $trimmed = $line.Trim()
    if (-not $trimmed -or $trimmed.StartsWith('#')) { continue }
    if ($trimmed -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
        Fail "Línea inválida en ${manifestPath}: $line"
    }
    $entries += [pscustomobject]@{
        Hash = $Matches[1].ToLowerInvariant()
        Name = $Matches[2].Trim()
    }
}

if ($entries.Count -eq 0) { Fail "El manifiesto '$manifestPath' está vacío." }

$ok = 0
$bad = 0

foreach ($entry in $entries) {
    $file = Join-Path $baseDir $entry.Name
    if (-not (Test-Path -LiteralPath $file)) {
        Write-Host ("FALTA  {0}" -f $entry.Name) -ForegroundColor Red
        $bad++
        continue
    }

    $actual = Get-Sha256 $file
    if ($actual -ne $entry.Hash) {
        Write-Host ("HASH   {0}" -f $entry.Name) -ForegroundColor Red
        Write-Host ("       esperado {0}" -f $entry.Hash)
        Write-Host ("       real     {0}" -f $actual)
        $bad++
        continue
    }

    if ($RequireSignature -and $file.EndsWith('.exe', [System.StringComparison]::OrdinalIgnoreCase)) {
        $sig = $null
        $sigError = ''
        try { $sig = Get-AuthenticodeSignature -LiteralPath $file }
        catch { $sigError = $_.Exception.Message }
        if ($null -eq $sig) {
            Write-Host ("FIRMA  {0} (no verificable: {1})" -f $entry.Name, $sigError) -ForegroundColor Red
            $bad++
            continue
        }
        if ($sig.Status -ne 'Valid') {
            Write-Host ("FIRMA  {0} ({1})" -f $entry.Name, $sig.Status) -ForegroundColor Red
            $bad++
            continue
        }
        if ($ExpectedPublisher -and ($sig.SignerCertificate.Subject -notlike "*$ExpectedPublisher*")) {
            Write-Host ("FIRMA  {0} (editor inesperado: {1})" -f $entry.Name, $sig.SignerCertificate.Subject) -ForegroundColor Red
            $bad++
            continue
        }
        if (-not $sig.TimeStamperCertificate) {
            Write-Host ("FIRMA  {0} (sin timestamp RFC3161)" -f $entry.Name) -ForegroundColor Yellow
            $bad++
            continue
        }
    }

    Write-Host ("OK     {0}" -f $entry.Name) -ForegroundColor Green
    $ok++
}

Write-Host ''
Write-Host ("{0} OK, {1} con problemas ({2})" -f $ok, $bad, $manifestPath)
if ($bad -gt 0) { exit 1 }
exit 0
