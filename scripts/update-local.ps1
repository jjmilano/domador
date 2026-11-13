<#
.SYNOPSIS
  Actualiza la instalación local de Domador con un build nuevo y la vuelve a abrir.

.DESCRIPTION
  1. Compila (salvo -SkipBuild) en target-dev para no chocar con binarios abiertos.
  2. Le pide al motor que se apague cerrando también la UI y la bandeja
     (POST /api/shutdown {"close_all":true}); el motor restaura el plan de energía.
  3. Copia domador.exe, domador-ui.exe y domador-tray.exe a la carpeta instalada.
     Si alguno sigue abierto, lo renombra a .old (Windows lo permite) y copia igual.
  4. Abre bandeja + UI con un solo UAC, ya elevados.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\update-local.ps1
#>
param(
    [switch]$SkipBuild,
    [string]$Dest = "$env:LOCALAPPDATA\Programs\Domador",
    [int]$Port = 8787
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$src = Join-Path $repo 'target-dev\release'
$bins = 'domador.exe', 'domador-ui.exe', 'domador-tray.exe'

if (-not $SkipBuild) {
    Write-Host '== Compilando =='
    $env:CARGO_TARGET_DIR = Join-Path $repo 'target-dev'
    Push-Location $repo
    try { cargo build --release --bins; if ($LASTEXITCODE) { throw 'cargo build falló' } }
    finally { Pop-Location }
}
foreach ($b in $bins) { if (-not (Test-Path (Join-Path $src $b))) { throw "falta $b en $src" } }

Write-Host '== Apagando Domador =='
try {
    Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$Port/api/shutdown" `
        -Headers @{ 'X-Domador' = '1' } -ContentType 'application/json' `
        -Body '{"close_all":true}' -TimeoutSec 5 | Out-Null
    Write-Host 'motor apagado (y UI/bandeja cerradas)'
} catch { Write-Host 'el motor no respondía; sigo' }
for ($i = 0; $i -lt 20; $i++) {
    if (-not (Get-Process domador, domador-ui, domador-tray -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 300
}

Write-Host '== Instalando =='
New-Item -ItemType Directory -Force $Dest | Out-Null
Get-ChildItem $Dest -Filter '*.old' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
foreach ($b in $bins) {
    $target = Join-Path $Dest $b
    try { Copy-Item (Join-Path $src $b) $target -Force }
    catch {
        Move-Item $target "$target.old" -Force
        Copy-Item (Join-Path $src $b) $target -Force
        Write-Host "$b estaba abierto: el viejo quedó como .old"
    }
}
Copy-Item (Join-Path $repo 'scripts\install-admin.ps1') $Dest -Force
& (Join-Path $Dest 'domador.exe') --version

Write-Host '== Abriendo (un UAC) =='
$tray = Join-Path $Dest 'domador-tray.exe'
$ui = Join-Path $Dest 'domador-ui.exe'
$cmd = "/c start `"`" `"$tray`" & timeout /t 3 /nobreak >nul & start `"`" `"$ui`""
Start-Process cmd.exe -ArgumentList $cmd -Verb RunAs -WindowStyle Hidden
Write-Host 'listo'
