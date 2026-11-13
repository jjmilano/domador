# Registra Domador como tarea de inicio con privilegios elevados.
# Ejecutar una sola vez con UAC (click derecho > Ejecutar con PowerShell como admin).
$ErrorActionPreference = "Stop"
$exe = Join-Path $env:LOCALAPPDATA "Programs\Domador\domador-tray.exe"
if (-not (Test-Path $exe)) { throw "No existe $exe. Instalá Domador primero." }
$action = New-ScheduledTaskAction -Execute $exe
$trigger = New-ScheduledTaskTrigger -AtLogOn
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
Register-ScheduledTask -TaskName "Domador" -Action $action -Trigger $trigger -Principal $principal -Description "Domador: motor y bandeja con privilegios de administrador" -Force | Out-Null
Start-ScheduledTask -TaskName "Domador"
$startup = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\Domador (bandeja).lnk"
if (Test-Path $startup) { Remove-Item $startup -Force }
Write-Output "OK: tarea Domador creada y Startup retirado"
