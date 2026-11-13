@echo off
rem Ejecuta install-admin.ps1 elevado. Se instala junto a los binarios y se
rem ofrece como acceso directo "Activar arranque como administrador".
rem No crea nada por si solo: delega en scripts/install-admin.ps1 (backend).
setlocal
set "SCRIPT=%~dp0install-admin.ps1"

if not exist "%SCRIPT%" (
  echo Falta install-admin.ps1 en "%~dp0".
  echo Reinstala Domador con el instalador oficial.
  pause
  exit /b 1
)

echo Se va a pedir permiso de administrador para crear la tarea programada Domador.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Start-Process -FilePath 'powershell.exe' -Verb RunAs -Wait -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','%SCRIPT%'"

if errorlevel 1 (
  echo.
  echo No se pudo crear la tarea. Si cancelaste el UAC, volve a intentar.
  pause
  exit /b 1
)

echo.
echo Listo: tarea Domador registrada y Startup sin duplicados.
pause
exit /b 0
