# Packaging e instalación — Domador v2

Estado: **propuesta lista para revisar**. Los scripts viven en `installer/` y no
modifican source, assets, README ni `scripts/install-admin.ps1`. No se publicó
nada ni se ejecutó el instalador.

## Decisión: Inno Setup 6 (no MSI/WiX)

Se eligió **Inno Setup 6.3+** con un único `.iss` versionado y `ISCC.exe` por
línea de comandos.

| Criterio | Inno Setup | WiX / MSI |
|---|---|---|
| Instalación per-user sin UAC | `PrivilegesRequired=lowest`, natural | MSI per-user existe pero es frágil (ICE, componentes, `ALLUSERS`) |
| Instalador .exe de un solo archivo | sí | no (MSI + bootstrapper) |
| Checkbox de tareas (Startup, tarea elevada) | `[Tasks]` + `[Code]` | requiere dialogos propios/custom actions |
| Desinstalación que preserva config | archivos no instalados no se tocan | igual, pero más superficie |
| Compilar en CI | `ISCC.exe archivo.iss` | candle/light + validación ICE |
| Curva de mantenimiento | baja, un archivo de texto | alta, authoring + upgrades mayores |
| Firma y SmartScreen | firma el setup y los .exe | firma MSI + bootstrapper |

MSI tiene sentido si hace falta despliegue por GPO/Intune o instalación
machine-wide. Hoy Domador es una app personal que escribe en
`%LOCALAPPDATA%`, así que MSI agrega complejidad sin beneficio. Si en el futuro
se necesita GPO, se puede agregar un MSI wrapper sin rehacer el instalador.

Versión usada para validar este paquete: **Inno Setup 6.7.3** (ISCC compiló el
`.iss` sin errores). El repo no fija la versión; el build imprime la que usa.
Recomendación: fijar 6.7.x en CI y en la máquina de release.

## Layout instalado

```
%LOCALAPPDATA%\Programs\Domador\
  domador.exe            motor + API + CLI
  domador-ui.exe         UI nativa (egui)
  domador-tray.exe       bandeja animada
  install-admin.ps1      copia literal de scripts/install-admin.ps1 (backend)
  activate-admin.cmd     elevación bajo demanda de install-admin.ps1
  check-update.ps1       chequeo manual opt-in
  verify-hashes.ps1      verificación local de integridad
  SHA256SUMS.txt         hashes del payload instalado
  unins000.exe           desinstalador
```

- Config y datos de usuario: `%LOCALAPPDATA%\Domador\` (`config.json`,
  `config.bad.json`, frames de bandeja en `anim\`). **Nunca se borra.**
- Menú Inicio: `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Domador`.
- Escritorio: opcional (checkbox).
- Startup: `...\Startup\Domador (bandeja).lnk`, opcional y desmarcado.
- Tarea programada `Domador`: opcional y desmarcada; requiere UAC una vez.

El directorio es fijo (`DisableDirPage=yes`) por soporte y reproducibilidad: el
instalador, los accesos y la documentación asumen esa ruta. La versión actual de
`scripts/install-admin.ps1` se auto-localiza (busca `domador-tray.exe` al lado
del script), así que no depende de la ruta; versiones anteriores sí la
hardcodeaban. `/api/admin-task` busca `install-admin.ps1` al lado de
`domador.exe`, que se cumple en cualquier carpeta.

## Requisitos y build

Requisitos: Windows 10/11 x64, Rust 1.88+, linker MSVC o GNU, e Inno Setup 6.3+.

```powershell
winget install --id JRSoftware.InnoSetup -e
```

Build completo (compila los tres binarios, staged, hashes e instalador):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1
```

Pasos equivalentes a mano:

```powershell
cargo build --release --locked --bin domador --bin domador-ui --bin domador-tray
# staging: target\release\{domador,domador-ui,domador-tray}.exe
#           scripts\install-admin.ps1 + installer\{activate-admin.cmd,check-update.ps1,verify-hashes.ps1}
#           + SHA256SUMS.txt generado por el script
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /Qp `
  "/DStageDir=$PWD\dist\stage" "/DOutputDir=$PWD\dist" `
  "/DHashDomador=<sha256>" "/DHashDomadorUi=<sha256>" `
  "/DHashDomadorTray=<sha256>" "/DHashAdminScript=<sha256>" `
  installer\domador.iss
```

Flags útiles de `build.ps1`:

| Flag | Efecto |
|---|---|
| `-SkipBuild` | usa `target\release` existente |
| `-SkipInstaller` | sólo staging + hashes, sin ISCC |
| `-OutputDir <ruta>` | cambia `dist` (no toca el repo) |
| `-Sign` | firma los .exe antes de hashear y el setup al final |
| `-CertThumbprint <hex>` | certificado a usar (o `DOMADOR_CERT_THUMBPRINT`) |
| `-ReleaseBaseUrl <url>` | genera `dist\update.json` para el updater opt-in |
| `-Version <x.y.z>` | override de la versión de `Cargo.toml` |

Artefactos en `dist\`: los tres `.exe`, `Domador-<ver>-x64-setup.exe`,
`SHA256SUMS.txt`, `update.json` (si se pidió) y `stage\` (entrada de ISCC, no se
publica).

## Verificación de hashes

Tres capas, con límites explícitos:

1. **Build**: `build.ps1` calcula SHA-256 de cada artefacto y escribe
   `dist\SHA256SUMS.txt` (formato `sha256sum`: `<hex>  <archivo>`).
2. **Instalación**: ISCC recibe los hashes por `/D`. Al terminar la copia,
   `[Code]` recalcula SHA-256 de `domador.exe`, `domador-ui.exe`,
   `domador-tray.exe` e `install-admin.ps1` y muestra un error si no coinciden.
   Detecta corrupción o reemplazo del payload después de compilar el setup.
3. **Usuario/CI**: `verify-hashes.ps1` contra `dist\` o contra la carpeta
   instalada.

```powershell
powershell -NoProfile -File installer\verify-hashes.ps1 -Path dist
powershell -NoProfile -File installer\verify-hashes.ps1 -Path "$env:LOCALAPPDATA\Programs\Domador"
```

Un hash correcto prueba **integridad de transporte**, no autoría: quien pueda
reemplazar el setup puede reemplazar también los hashes publicados. La única
prueba de origen es Authenticode (ver checklist de firma).

## Tarea programada elevada (opcional)

- Checkbox `startup_admin`, desmarcado por defecto. Si se marca, al final de la
  instalación se ejecuta `install-admin.ps1 -Start` con `ShellExec('runas', ...)`:
  **un solo UAC** y la bandeja elevada arranca de inmediato.
- El script es idempotente: si la tarea ya apunta al mismo `domador-tray.exe` con
  `RunLevel Highest`, no la recrea; si está desactualizada, la reemplaza.
- Cancelar el UAC no aborta la instalación; queda el acceso
  `Inicio > Domador > Activar arranque como administrador` (`activate-admin.cmd`).
- La tarea se llama `Domador`, corre `domador-tray.exe` con `RunLevel Highest`
  en `AtLogOn`, y `install-admin.ps1` retira el `.lnk` de Startup para no
  arrancar dos veces.
- `startup_tray` (Startup sin admin) y `startup_admin` son excluyentes en la
  práctica: si se marcan ambos, el instalador avisa y no crea el icono de
  Startup (`AllowStartupIcon`).

## Desinstalación

Qué hace:

- Mata `domador-tray.exe`, `domador-ui.exe` y `domador.exe` (`[UninstallRun]`).
- Borra sólo lo que instaló: `{app}`, accesos del Menú Inicio, icono de
  escritorio y `.lnk` de Startup si existía.
- Si detecta la tarea `Domador`, pregunta si eliminarla (UAC). Si el usuario
  dice No, la tarea queda huérfana y se documenta el comando manual.
- **No toca** `%LOCALAPPDATA%\Domador`: config, perfiles y reglas sobreviven.
  Al final muestra la ruta para que el usuario decida.

Comando manual si quedó la tarea:

```powershell
schtasks /Query /TN "Domador"
schtasks /Delete /TN "Domador" /F   # en una consola elevada
```

Modo silencioso: `Domador-<ver>-x64-setup.exe /SILENT /SP- /NORESTART`.
En silencio no se piden tareas opcionales (no hay UAC) y el desinstalador no
pregunta por la tarea.

## Actualización opt-in

No hay chequeo en segundo plano ni telemetría: la app sigue sin abrir
conexiones salientes. La actualización se dispara a mano desde
`Inicio > Domador > Buscar actualizaciones` (o `check-update.ps1`).

`update.json` (lo genera `build.ps1 -ReleaseBaseUrl`):

```json
{
  "app": "domador",
  "channel": "stable",
  "version": "0.7.0",
  "released": "2026-11-13",
  "min_os": "10.0.17763",
  "notes_url": "https://github.com/jjmilano/domador/releases/tag/v0.7.0",
  "publisher": "",
  "assets": [
    {
      "name": "Domador-0.7.0-x64-setup.exe",
      "url": "https://github.com/jjmilano/domador/releases/download/v0.7.0/Domador-0.7.0-x64-setup.exe",
      "sha256": "<64 hex>",
      "size": 5029746,
      "kind": "installer"
    }
  ]
}
```

Flujo de `check-update.ps1`:

1. Lee `update.json` por HTTPS (o `-ManifestPath` local para QA).
2. Compara versión; si está al día, sale 0.
3. Exige URL HTTPS (o archivo local sólo con `-ManifestPath`).
4. Descarga a `%TEMP%` y valida tamaño + SHA-256; si falla, borra y aborta.
5. Si hay `publisher` o `-ExpectedPublisher`, exige Authenticode válido y
   editor coincidente (`-RequireSignature` fuerza la exigencia).
6. Pide confirmación (`-Yes` la omite) y corre el setup con
   `/SILENT /SP- /NORESTART /CLOSEAPPLICATIONS`.

`-DryRun` descarga y verifica sin ejecutar. Rollback: reinstalar el setup de la
versión anterior; la config se conserva. No hay downgrade automático.

Pendiente para integrarlo a la app: un setting “buscar actualizaciones” que
llame a este script. Requiere cambio de source y no se hizo acá.

## Firma de código — checklist

Sin certificado hoy, los binarios salen **unsigned** y SmartScreen muestra
“Editor desconocido”. Los hashes no reemplazan la firma.

- [ ] Elegir certificado: OV/EV Authenticode o Azure Trusted Signing (CI).
- [ ] Guardar la clave en HSM/token o secreto de CI; nunca en el repo.
- [ ] Firmar en este orden: `domador.exe`, `domador-ui.exe`,
      `domador-tray.exe` → calcular hashes → compilar setup → firmar setup.
      `build.ps1 -Sign` hace exactamente ese orden.
- [ ] Usar SHA-256 y timestamp RFC3161:
      `signtool sign /sha1 <thumb> /fd sha256 /tr http://timestamp.digicert.com /td sha256 <archivo>`
- [ ] Verificar: `Get-AuthenticodeSignature` con `Status=Valid`, cadena OK y
      `TimeStamperCertificate` presente.
- [ ] `verify-hashes.ps1 -RequireSignature -ExpectedPublisher "<editor>"`.
- [ ] Opcional: firmar también el desinstalador con `SignTool` +
      `SignedUninstaller=yes` en el `.iss` (no está activado porque sin cert
      rompería el build).
- [ ] SmartScreen: EV da reputación inmediata; OV la construye con descargas.
      No instruir a “ignorar” el aviso.
- [ ] Documentar thumbprint, emisor y vencimiento en las notas del release.
- [ ] Rotar el certificado antes de que venza; el timestamp mantiene válidas
      las firmas viejas.
- [ ] Los `.ps1` no están firmados: el instalador los ejecuta con
      `-ExecutionPolicy Bypass` sobre archivos locales verificados por hash.
      Es riesgo residual conocido.

## CI sugerido

El workflow actual solo compila `domador.exe`. Para el paquete hacen falta los
tres bins y el instalador; snippet para agregar (no aplicado):

```yaml
- name: Instalar Inno Setup
  run: winget install --id JRSoftware.InnoSetup -e --silent --accept-package-agreements --accept-source-agreements

- name: Empaquetar
  run: powershell -NoProfile -ExecutionPolicy Bypass -File installer/build.ps1 -SkipBuild

- name: Verificar hashes
  run: powershell -NoProfile -ExecutionPolicy Bypass -File installer/verify-hashes.ps1 -Path dist

- name: Subir artefactos
  uses: actions/upload-artifact@v4
  with:
    name: domador-windows-x64
    path: |
      dist/domador.exe
      dist/domador-ui.exe
      dist/domador-tray.exe
      dist/Domador-*-x64-setup.exe
      dist/SHA256SUMS.txt
      dist/update.json
```

En CI, `-Sign` requiere el certificado como secreto y `signtool` del Windows
SDK; si no hay cert, el job publica artefactos unsigned y debe decirlo.

## Matriz de pruebas antes de publicar

| Caso | Esperado |
|---|---|
| Win10 22H2 x64, usuario sin admin | instala sin UAC, abre UI |
| Win11 24H2, escala 125% y 150% | sin texto cortado, iconos OK |
| Instalar sin marcar tareas | sin Startup, sin tarea, sin UAC |
| Marcar `startup_tray` | `.lnk` en Startup, arranca bandeja al login |
| Marcar `startup_admin` + aceptar UAC | tarea `Domador` Highest, sin `.lnk` duplicado |
| Marcar `startup_admin` + cancelar UAC | instalación OK, aviso, acceso manual funciona |
| Upgrade sobre 0.6.x | config intacta, binarios reemplazados, sin doble entrada |
| Desinstalar | app y accesos fuera; `%LOCALAPPDATA%\Domador` intacto |
| Desinstalar con tarea | pregunta; con Sí la borra, con No queda y se avisa |
| `verify-hashes.ps1` sobre `dist` | 0 fallas |
| Simular corrupción de un `.exe` staged | el setup detecta hash distinto y avisa |
| `check-update.ps1 -DryRun` | descarga, valida SHA-256, no ejecuta |
| Path de usuario con espacios/acentos | instala y desinstala OK |
| Antivirus/EDR corporativo | sin bloqueo; si bloquea, reportar binario |

## Riesgos y limitaciones conocidas

- **Sin certificado**: SmartScreen y algunos EDR marcan el setup. Es el riesgo
  principal de adopción; el checklist de firma lo resuelve cuando haya cert.
- **Sólo per-user**: no hay instalación machine-wide, GPO ni MSI. Si se pide,
  se agrega después sin cambiar el layout.
- **Ruta fija**: necesaria por `install-admin.ps1` y `/api/admin-task`.
- **Hashes ≠ firma**: el chequeo post-instalación detecta corrupción del
  payload, no un setup malicioso que se haga pasar por el oficial.
- **Desinstalación con tarea elevada**: si el usuario rechaza el UAC, la tarea
  queda; se documenta el comando manual.
- **Bandeja elevada al desinstalar**: el desinstalador corre sin admin, así que
  no puede terminar un `domador-tray.exe` elevado. Si la tarea está activa, hay
  que cerrar Domador desde el menú de la bandeja antes de desinstalar; si no,
  Windows puede pedir reiniciar para borrar el .exe bloqueado.
- **Build no bit-reproducible**: timestamps PE y compresión de Inno cambian el
  hash entre máquinas. La reproducibilidad es por versión de toolchain y
  verificación de hashes publicados, no bit a bit.
- **`install-admin.ps1` es de backend**: el instalador lo copia tal cual. Si
  cambia la forma en que resuelve el ejecutable o el nombre de la tarea, hay que
  revisar `domador.iss` (nombre `Domador`, flag `-Start`).
- **Firma de scripts**: `activate-admin.cmd` y los `.ps1` no están firmados;
  el instalador los ejecuta con bypass y hashes.
- **`Get-AuthenticodeSignature` puede no estar disponible** en instalaciones de
  PowerShell con módulos rotos: `check-update.ps1` avisa y sigue sólo si no se
  exigió firma; `verify-hashes.ps1 -RequireSignature` falla en ese caso.
