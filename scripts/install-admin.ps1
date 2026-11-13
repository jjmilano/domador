# Registra Domador como tarea de inicio con privilegios elevados.
#
# Uso normal: lo lanza el motor (POST /api/admin-task) o a mano:
#   powershell -NoProfile -ExecutionPolicy Bypass -File install-admin.ps1
# Opciones:
#   -Start   arranca la tarea al terminar (por defecto NO, para no duplicar
#            el motor que ya está corriendo)
#
# Es idempotente: si la tarea ya existe con el mismo ejecutable y RunLevel
# HighestAvailable, no la recrea. Sale con 0 sólo si la tarea quedó verificada.
#
# La salida es ASCII a propósito: el motor la lee con from_utf8_lossy.

[CmdletBinding()]
param(
    [switch]$Start,
    [string]$ExeDir = ""
)

$ErrorActionPreference = "Stop"
$taskName = "Domador"

function Fail([string]$message) {
    Write-Output ("ERROR: " + $message)
    exit 1
}

# ---------------------------------------------------------------- ejecutable
# `-ExeDir` lo pasa el motor cuando corre el script embebido (ahí no hay
# $MyInvocation.MyCommand.Path porque es un scriptblock, no un archivo).
$here = $ExeDir
if (-not $here) {
    $here = Split-Path -Parent $MyInvocation.MyCommand.Path
}
$candidates = @()
if ($here) {
    $candidates += (Join-Path $here "domador-tray.exe")
    $candidates += (Join-Path $here "domador.exe")
    $candidates += (Join-Path $here "scripts\domador-tray.exe")
}
if ($env:LOCALAPPDATA) {
    $candidates += (Join-Path $env:LOCALAPPDATA "Programs\Domador\domador-tray.exe")
}
$exe = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $exe) {
    Fail ("no encuentro domador-tray.exe; buscado en: " + ($candidates -join "; ") + ". Instala Domador primero.")
}
$exe = (Resolve-Path -LiteralPath $exe).Path
$exeDir = Split-Path -Parent $exe

# ---------------------------------------------------------------- identidad
$userId = $null
try {
    $userId = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
} catch {
    $userId = $null
}
if (-not $userId) {
    if ($env:USERDOMAIN) {
        $userId = "$env:USERDOMAIN\$env:USERNAME"
    } else {
        $userId = "$env:COMPUTERNAME\$env:USERNAME"
    }
}

# ---------------------------------------------------------------- idempotencia
$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
$needsRegister = $true
if ($existing) {
    $currentExe = ""
    if (@($existing.Actions).Count -gt 0) {
        $currentExe = [string](@($existing.Actions)[0].Execute)
    }
    $currentLevel = [string]$existing.Principal.RunLevel
    if ($currentExe -and ($currentExe -ieq $exe) -and ($currentLevel -ieq "HighestAvailable")) {
        $needsRegister = $false
        Write-Output ("INFO: tarea existente reutilizada (exe=" + $currentExe + ")")
    } else {
        Write-Output ("INFO: tarea existente desactualizada (exe=" + $currentExe + ", runlevel=" + $currentLevel + "); se recrea")
    }
}

if ($needsRegister) {
    $action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $exeDir
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $userId
    $principal = New-ScheduledTaskPrincipal -UserId $userId -LogonType Interactive -RunLevel Highest
    # Sin límite de ejecución y sin relanzar en bucle: el motor vive en bandeja.
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -StartWhenAvailable `
        -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit ([TimeSpan]::Zero)
    try {
        Register-ScheduledTask -TaskName $taskName `
            -Action $action `
            -Trigger $trigger `
            -Principal $principal `
            -Settings $settings `
            -Description "Domador: motor y bandeja con privilegios de administrador" `
            -Force | Out-Null
    } catch {
        Fail ("no pude registrar la tarea: " + $_.Exception.Message)
    }
}

# ---------------------------------------------------------------- verificación
$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if (-not $task) {
    Fail "la tarea no existe despues de registrarla"
}
$verifyExe = ""
if (@($task.Actions).Count -gt 0) {
    $verifyExe = [string](@($task.Actions)[0].Execute)
}
$verifyLevel = [string]$task.Principal.RunLevel
if (-not $verifyExe) {
    Fail "la tarea no tiene accion configurada"
}
if (-not (Test-Path -LiteralPath $verifyExe)) {
    Fail ("la accion apunta a un ejecutable inexistente: " + $verifyExe)
}
if ($verifyLevel -ine "HighestAvailable") {
    Fail ("RunLevel inesperado: " + $verifyLevel)
}

# El acceso directo del Startup duplicaría el arranque: se retira sólo cuando
# la tarea quedó verificada.
$startup = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\Domador (bandeja).lnk"
$startupRemoved = $false
if (Test-Path -LiteralPath $startup) {
    try {
        Remove-Item -LiteralPath $startup -Force
        $startupRemoved = $true
    } catch {
        Write-Output ("WARN: no pude quitar el acceso de Startup: " + $_.Exception.Message)
    }
}

$started = $false
if ($Start) {
    try {
        Start-ScheduledTask -TaskName $taskName
        $started = $true
    } catch {
        Fail ("la tarea quedó creada pero no pude iniciarla: " + $_.Exception.Message)
    }
}

# Una sola línea parseable por el motor.
Write-Output ("OK task=" + $taskName + " state=" + [string]$task.State + " runlevel=" + $verifyLevel + " started=" + $started.ToString().ToLower() + " startup_removed=" + $startupRemoved.ToString().ToLower() + " exe=" + $verifyExe)
exit 0
