#Requires -Version 5.1
<#
.SYNOPSIS
    Compila los tres binarios de Domador, arma el staging, calcula hashes y
    compila el instalador de Inno Setup.

.DESCRIPTION
    Pasos:
      1. cargo build --release --locked --bin domador --bin domador-ui --bin domador-tray
      2. copia binarios + scripts a dist\stage
      3. firma los .exe si se pasa -Sign (antes de hashear)
      4. escribe dist\stage\SHA256SUMS.txt
      5. compila installer\domador.iss con ISCC pasando /D y los hashes
      6. firma el setup si corresponde
      7. escribe dist\SHA256SUMS.txt y, si hay -ReleaseBaseUrl, dist\update.json

    No instala ni ejecuta el setup. No pide UAC: la elevación sólo ocurre si
    un usuario final marca la tarea programada opcional.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1 -SkipBuild -SkipInstaller
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutputDir,
    [switch]$SkipBuild,
    [switch]$SkipInstaller,
    [switch]$Sign,
    [string]$CertThumbprint = $env:DOMADOR_CERT_THUMBPRINT,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [string]$ReleaseBaseUrl,
    [string]$RepoSlug = 'jjmilano/domador'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$bins = @('domador', 'domador-ui', 'domador-tray')
if (-not $OutputDir) { $OutputDir = Join-Path $repo 'dist' }
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
$stage = Join-Path $OutputDir 'stage'

function Write-Step([string]$Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Get-Sha256([string]$Path) {
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $bytes = $sha.ComputeHash($stream) } finally { $sha.Dispose() }
    }
    finally { $stream.Dispose() }
    return ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant()
}

function Resolve-Iscc {
    $candidates = New-Object System.Collections.Generic.List[string]
    if ($env:ISCC_PATH) { $candidates.Add($env:ISCC_PATH) }
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { $candidates.Add($cmd.Source) }
    foreach ($base in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $base) { continue }
        foreach ($edition in @('Inno Setup 7', 'Inno Setup 6')) {
            $candidates.Add((Join-Path $base "$edition\ISCC.exe"))
        }
    }
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) { return $candidate }
    }
    throw @'
No encontré ISCC.exe (Inno Setup 6.3+).
Instalalo con:  winget install --id JRSoftware.InnoSetup -e
o definí ISCC_PATH apuntando a ISCC.exe.
'@
}

function Resolve-SignTool {
    $cmd = Get-Command 'signtool.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $roots = @()
    if (${env:ProgramFiles(x86)}) { $roots += Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin' }
    if ($env:ProgramFiles) { $roots += Join-Path $env:ProgramFiles 'Windows Kits\10\bin' }
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        $found = Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1
        if ($found) { return $found }
    }
    throw 'No encontré signtool.exe. Instalá Windows SDK o agregalo al PATH.'
}

if (-not $SkipBuild -and -not (Get-Command 'cargo.exe' -ErrorAction SilentlyContinue)) {
    throw 'No encontré cargo.exe en el PATH.'
}

# --- Versión ---------------------------------------------------------------
if (-not $Version) {
    $toml = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw
    if ($toml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'No pude leer la versión de Cargo.toml; pasala con -Version.'
    }
    $Version = $Matches[1]
}
$numericParts = @($Version.Split('-')[0].Split('.'))
while ($numericParts.Count -lt 4) { $numericParts += '0' }
$numericVersion = ($numericParts[0..3] | ForEach-Object { [int]$_ }) -join '.'

Write-Step "Domador $Version (PE $numericVersion)"
Write-Step "Repo: $repo"
Write-Step "Salida: $OutputDir"

# --- Firma: validar antes de compilar --------------------------------------
$signTool = $null
if ($Sign) {
    if (-not $CertThumbprint) {
        throw 'Pasaste -Sign pero falta -CertThumbprint (o DOMADOR_CERT_THUMBPRINT).'
    }
    $signTool = Resolve-SignTool
    Write-Step "Firma habilitada (thumbprint $CertThumbprint)"
}

function Invoke-SignFile([string]$Path) {
    if (-not $Sign) { return }
    & $signTool sign /sha1 $CertThumbprint /fd sha256 /tr $TimestampUrl /td sha256 /d 'Domador' $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool falló para $Path (código $LASTEXITCODE)" }
}

# --- Build -----------------------------------------------------------------
if (-not $SkipBuild) {
    Write-Step 'Compilando binarios release'
    $cargoArgs = @('build', '--release', '--locked')
    foreach ($bin in $bins) { $cargoArgs += @('--bin', $bin) }
    Push-Location $repo
    try {
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build falló (código $LASTEXITCODE)" }
    }
    finally { Pop-Location }
}

# --- Staging ---------------------------------------------------------------
Write-Step 'Armando staging'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

foreach ($bin in $bins) {
    $src = Join-Path $repo "target\release\$bin.exe"
    if (-not (Test-Path -LiteralPath $src)) { throw "Falta $src; compilá sin -SkipBuild." }
    Copy-Item -LiteralPath $src -Destination $stage -Force
}
Copy-Item -LiteralPath (Join-Path $repo 'scripts\install-admin.ps1') -Destination $stage -Force
foreach ($file in @('activate-admin.cmd', 'check-update.ps1', 'verify-hashes.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $file) -Destination $stage -Force
}

# --- Firma de binarios ANTES de hashear ------------------------------------
foreach ($bin in $bins) { Invoke-SignFile (Join-Path $stage "$bin.exe") }

# --- Hashes del payload -----------------------------------------------------
Write-Step 'Calculando SHA-256'
$hashDomador = Get-Sha256 (Join-Path $stage 'domador.exe')
$hashUi = Get-Sha256 (Join-Path $stage 'domador-ui.exe')
$hashTray = Get-Sha256 (Join-Path $stage 'domador-tray.exe')
$hashAdmin = Get-Sha256 (Join-Path $stage 'install-admin.ps1')

$stageManifest = Join-Path $stage 'SHA256SUMS.txt'
$stageLines = Get-ChildItem -LiteralPath $stage -File |
    Sort-Object Name |
    ForEach-Object { "{0}  {1}" -f (Get-Sha256 $_.FullName), $_.Name }
Set-Content -LiteralPath $stageManifest -Value $stageLines -Encoding ASCII

# --- Instalador -------------------------------------------------------------
$setupPath = Join-Path $OutputDir "Domador-$Version-x64-setup.exe"
if (-not $SkipInstaller) {
    $iscc = Resolve-Iscc
    Write-Step "Compilando instalador con $iscc"
    $isccArgs = @(
        '/Qp',
        "/DAppVersion=$Version",
        "/DNumericVersion=$numericVersion",
        "/DStageDir=$stage",
        "/DOutputDir=$OutputDir",
        "/DHashDomador=$hashDomador",
        "/DHashDomadorUi=$hashUi",
        "/DHashDomadorTray=$hashTray",
        "/DHashAdminScript=$hashAdmin",
        (Join-Path $PSScriptRoot 'domador.iss')
    )
    & $iscc @isccArgs
    if ($LASTEXITCODE -ne 0) { throw "ISCC falló (código $LASTEXITCODE)" }
    if (-not (Test-Path -LiteralPath $setupPath)) { throw "ISCC no generó $setupPath" }
    Invoke-SignFile $setupPath
}

# --- Layout de release ------------------------------------------------------
Write-Step 'Copiando binarios al directorio de salida'
foreach ($bin in $bins) {
    Copy-Item -LiteralPath (Join-Path $stage "$bin.exe") -Destination $OutputDir -Force
}

$updatePath = Join-Path $OutputDir 'update.json'
if (Test-Path -LiteralPath $updatePath) { Remove-Item -LiteralPath $updatePath -Force }

$releaseFiles = @()
foreach ($bin in $bins) { $releaseFiles += @(Get-Item -LiteralPath (Join-Path $OutputDir "$bin.exe")) }
if (Test-Path -LiteralPath $setupPath) { $releaseFiles += @(Get-Item -LiteralPath $setupPath) }

if ($ReleaseBaseUrl) {
    $base = $ReleaseBaseUrl.TrimEnd('/')
    $assets = @()
    foreach ($file in $releaseFiles) {
        $kind = if ($file.Extension -eq '.exe' -and $file.Name -like '*setup*') { 'installer' } else { 'file' }
        $assets += [ordered]@{
            name   = $file.Name
            url    = "$base/$($file.Name)"
            sha256 = Get-Sha256 $file.FullName
            size   = $file.Length
            kind   = $kind
        }
    }
    $update = [ordered]@{
        app         = 'domador'
        channel     = 'stable'
        version     = $Version
        released    = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')
        min_os      = '10.0.17763'
        notes_url   = "https://github.com/$RepoSlug/releases/tag/v$Version"
        publisher   = ''
        assets      = $assets
    }
    $update | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $updatePath -Encoding UTF8
    Write-Step "update.json: $updatePath"
    $releaseFiles += @(Get-Item -LiteralPath $updatePath)
}

# --- Manifiesto final (artefactos planos de dist, sin auto-referencia) ------
Write-Step 'Escribiendo manifiesto de release'
$releaseFiles = @($releaseFiles | Sort-Object Name)
$releaseLines = $releaseFiles | ForEach-Object { "{0}  {1}" -f (Get-Sha256 $_.FullName), $_.Name }
$releaseManifest = Join-Path $OutputDir 'SHA256SUMS.txt'
Set-Content -LiteralPath $releaseManifest -Value $releaseLines -Encoding ASCII

Write-Step 'Listo'
Write-Host ''
Write-Host 'Artefactos:' -ForegroundColor Green
$releaseFiles | ForEach-Object {
    Write-Host ("  {0,-42} {1,10:N0} bytes  {2}" -f $_.Name, $_.Length, (Get-Sha256 $_.FullName))
}
Write-Host ''
Write-Host "Manifiesto: $releaseManifest"
Write-Host 'Verificación: powershell -NoProfile -File installer\verify-hashes.ps1 -Path dist'
Write-Host 'El setup NO se ejecutó. Probalo en una VM con snapshot antes de publicar.'
