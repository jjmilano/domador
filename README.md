# Domador

Control de procesos y rendimiento para Windows, escrito en Rust. Una alternativa open source a Process Lasso, enfocada en una sola cosa: que la notebook rinda al mango cuando la necesitás — jugando o corriendo herramientas de IA — sin telemetría, sin cuentas y sin instalar medio sistema.

Motor nativo. UI nativa. Sin WebView en el camino principal. Sin llamadas a casa automáticas.

**Descargar:** [Domador v0.6.2](https://github.com/jjmilano/domador/releases/tag/v0.6.2) · Windows 10/11 x64.

![UI nativa de Domador](docs/screenshot.png)

> La captura es de **v0.6.0**. El rediseño de UI y los assets steampunk ya están en el árbol; falta la captura nueva y la revisión visual del icono recién incrustado. Ver [docs/ROADMAP-V2.md](docs/ROADMAP-V2.md).

## Por qué

Windows reparte CPU como puede: el antivirus escanea mientras jugás, el indexador aparece en el peor momento, el navegador con cuarenta pestañas se come un núcleo entero, y el plan de energía sigue en "Equilibrado" cuando estás por arrancar una partida. Process Lasso resuelve parte de eso, pero es cerrado, pesado y te pide confiar en lo que hace por atrás.

Domador hace lo mismo (y algunas cosas más) con el código a la vista: reglas por ejecutable, ProBalance con cooldown, perfiles de energía, SmartTrim y una API local para que lo maneje un agente de IA. El caso típico es una notebook de 8 núcleos híbridos y 16 GB donde el juego, el modelo local y el navegador se pelean por los mismos hilos.

## Estado

**v0.6.2**, usable y probado en Windows 11 x64. El motor Rust, la **UI nativa egui/eframe** (backend `glow`/OpenGL, sin WebView), la bandeja, la API HTTP y el servidor MCP están conectados. `cargo test --all-targets` deja **14 tests en verde y 1 ignorado** (bug conocido: body `chunked` > 64 KiB devuelve 400 en vez de 413) y cubre prioridades, afinidad, EcoQoS, trim, restauración, reglas persistentes, autenticación/Host/body cap de la API, handshake MCP, snapshot NT, `powercfg`, contrato de `/api/state`, familias y topes de MemGuard. El plan de energía se restaura en una salida ordenada (Ctrl+C, `domador stop` o Salir desde la bandeja); un crash no lo garantiza.

La app son **tres binarios chicos**:

| Binario | Rol | Tamaño release local* |
|---|---|---|
| `domador.exe` | Motor, CLI, API HTTP y MCP | 2,6 MB |
| `domador-ui.exe` | Ventana nativa egui | 5,5 MB |
| `domador-tray.exe` | Bandeja animada y menú | 0,6 MB |

\* Build release local del árbol v0.6.2 (2026-11-13), con el icono steampunk ya incrustado. Los tamaños cambian con el compilador y las features; no son una promesa de release.

La UI se puede cerrar sin apagar el motor. Si `domador-ui.exe` no está al lado de `domador.exe`, `domador` cae al navegador como respaldo — eso es compatibilidad, no la UI principal.

**Lo que falta y no se disimula:**

- **Icono steampunk:** integrado en `assets/` y **embebido en el build local** (león con antiparras, engranaje y latón; `.ico` de 8 resoluciones y 8 frames de bandeja). Falta la verificación visual fina a 16/32 px en taskbar y bandeja, y actualizar la captura.
- **MemGuard:** implementado en el motor y expuesto por API (`GET /api/mem`, `POST /api/memguard`, `POST /api/mem/relief`). Avisa por presión de commit (88%) o RAM disponible (7%), con cooldown de 900 s; el alivio automático de working sets de fondo es **opt-in** y nunca termina procesos. Tiene tests de topes. Falta la tarjeta en **Salud** y la tool MCP; mientras tanto se configura por API o en la sección `memguard` del `config.json`.
- **Instalador:** el fuente está (`installer/`, Inno Setup 6.3+, hashes SHA-256, tarea elevada opcional, updater manual). Falta compilarlo, firmarlo si hay certificado y probarlo en una VM limpia; no hay artefacto publicado.
- **CPU sets, historial de 24 h y grupos en la UI:** en el roadmap, no en el binario.

Nada de eso impide usar Domador hoy. Está detallado en el [roadmap V2](docs/ROADMAP-V2.md).

## Características

### ProBalance con cooldown

El corazón del proyecto, **apagado por defecto**: se activa desde Ajustes, por API o con una configuración propia. Cada tanto mira el consumo de CPU por proceso y, cuando uno se pasa de rosca, lo baja de prioridad en lugar de matarlo.

- Umbral configurable (`threshold`), tiempo que tiene que sostenerlo (`sustain_s`) y umbral de restauración (`restore_below`) con su propia espera (`restore_s`).
- **Vía rápida** (`fast_threshold`, `fast_sustain_s`): si un proceso se come 25% o más de la máquina durante 2 s, se regula sin esperar el sustain lento. Los picos moderados siguen la vía lenta para no flapear; el cooldown se aplica igual.
- Baja a `below_normal` o `idle`, a elección, y opcionalmente activa EcoQoS mientras está regulado.
- **Cooldown** (`cooldown_s`): después de restaurar un proceso no lo vuelve a tocar por un rato. Evita el ping-pong de prioridades que castiga el rendimiento más de lo que ayuda.
- **Protección de apps interactivas**: `skip_foreground` no toca la ventana en primer plano y `skip_interactive` saltea cualquier proceso con ventana visible. La lista `protect` es manual y tiene la última palabra.
- Todo lo que toca queda registrado y se puede restaurar a mano.

### Diagnóstico de memoria, GPU y procesos colgados

La pestaña **Salud** muestra lo que el Task Manager esconde: commit real contra el límite, pool paginado y no paginado del kernel, cache, y los procesos ordenados por **memoria privada** (no working set). Así aparecen los `hex.exe` que ocupan 1,5 GB con 27 MB de working set.

### Ficha de proceso: qué es cada nombre raro

En **Procesos**, clic en una fila (o en el nombre) abre la ficha del proceso. Muestra descripción en español para los componentes conocidos de Windows, fabricante, producto, versión, ruta, proceso padre, inicio, ventana, línea de comandos y estado de firma. Los nombres sin metadata muestran **“Sin descripción del fabricante”** con la ruta y el padre para poder identificarlos; la regla es ruta + firma + padre, no el nombre.

El dato sale de `GET /api/process?pid=N` bajo demanda, con cache corto por PID. Sin admin, Windows puede bloquear ruta, comando y firma de procesos del sistema (`svchost`, `lsass`, `dwm`, etc.); la ficha igual muestra nombre, descripción conocida y padre, y avisa **“Ficha limitada”**. Elevá Domador para completarla.

El **watchdog de colgados** detecta un proceso sin ventana que lleva N minutos por encima de un umbral de CPU y lo registra; opcionalmente lo termina. Viene activo en modo `warn` (6% de la máquina durante 5 minutos). Es lo que habría cazado el `find.exe` que estuvo 5 horas comiéndose un núcleo.

La tarjeta **GPU** consulta `nvidia-smi` (uso, VRAM, temperatura, potencia, P-state) y la acción de GPU de cada proceso escribe la preferencia de **GPU dedicada** en Windows para ese ejecutable. No agrega VRAM: sirve para que el juego no quede en la iGPU y para ver cuánta VRAM se están comiendo los WebViews.

### Reglas por ejecutable

Una regla matchea por ruta completa (`C:\Juegos\juego.exe`), por nombre (`chrome.exe`) o con comodines (`*steam*`), y se aplica al iniciar cada proceso y cuando la creás.

| Campo | Qué hace |
|---|---|
| `priority` | `idle`, `below_normal`, `normal`, `above_normal`, `high`, `realtime` |
| `affinity` | Máscara de núcleos, ej. `0-3,8`. Vacío = no cambiar |
| `eco` | Activa o desactiva EcoQoS (Efficiency Mode) |
| `io` | Prioridad de E/S: `very_low`, `low`, `normal`, `high` |
| `mem` | Prioridad de memoria: `very_low` … `normal` |
| `watchdog` | `keep_alive` (relanzar) y `max_instances` (limitar instancias) |

Las reglas se guardan en un JSON local y sobreviven al reinicio.

### Watchdog

- `keep_alive`: si el proceso se cierra, lo relanza. Necesita ruta completa, no alcanza con el nombre.
- `max_instances`: si hay más instancias que el límite, cierra las sobrantes (las más nuevas primero).

### SmartTrim y alivio de memoria — sin placebo

SmartTrim libera el working set cuando la RAM del sistema pasa un umbral (`85%` por defecto), cada N segundos (`300 s`) y salteando los procesos chicos (`min_mb`, 250 MB). Viene **apagado**. El alivio manual tiene dos variantes: `trim-idle` (sólo procesos de fondo, sin ventana y con CPU baja) y `trim-all` (todos los que superan `min_mb`).

No es un "liberador de RAM" mágico: bajar el working set de una app que estás usando la obliga a leer de disco y puede empeorar la experiencia. Por eso el modo automático es opt-in, el manual excluye procesos visibles en `trim-idle`, y todo queda en el log con los MB liberados.

**MemGuard** está implementado en el motor y expuesto por API: mide commit usado contra el límite y RAM disponible, marca `warn`/`critical` con cooldown y, si se habilita `relief`, alivia working sets **sólo de procesos de fondo** cada 15 minutos como mínimo. Nunca mata ni toca la ventana en primer plano. Se configura con `POST /api/memguard` o editando `memguard` en el `config.json`; falta la tarjeta en **Salud** y la tool MCP.

### Planes de energía y perfiles

Domador lista los planes de energía del sistema, permite cambiarlos y puede hacerlo solo: un proceso disparador (`trigger`, ej. `juego.exe`) hace que pase al plan de juego mientras está abierto y vuelva al plan base al cerrarse. Al salir de Domador se restaura el plan original.

Los perfiles cambian el **plan de energía** y aplican un **boost de prioridad** a los patrones correspondientes; no prenden ProBalance ni crean reglas por su cuenta. ProBalance y las reglas se configuran aparte, y así se muestra en la UI.

| Perfil | Qué hace |
|---|---|
| **Full Power** | Plan de máximo rendimiento y boost alto a juegos/herramientas de IA. Sólo con la notebook enchufada |
| **Juego** | Plan de juego/alto rendimiento y prioridad alta al ejecutable elegido; boost a herramientas de IA |
| **IA** | Prioridad `above_normal` a los patrones de IA (Claude, Codex, Hermes, Cursor, etc.) y los protege de ProBalance |
| **Equilibrado** | Vuelve al plan anterior y deshace los cambios del perfil |

### API HTTP local, servidor MCP y UI nativa

- `domador serve` levanta un servidor HTTP en `127.0.0.1` (puerto 8787 por defecto, `--port` para cambiarlo) que sirve la API JSON. La UI web vieja ya no se incluye; la interfaz es `domador-ui.exe`.
- `domador mcp` expone las mismas operaciones como servidor MCP por stdio, así un agente (Claude, Codex, Hermes) puede leer el estado, aplicar reglas y cambiar de perfil con herramientas normales. Es un proxy: **requiere que `domador serve` esté corriendo** en el mismo puerto (8787 por defecto).
- `domador-ui` abre la **UI nativa egui/eframe**: ventana propia, sin Edge, sin WebView2 y sin pestañas. Es la UI principal desde v0.6. Tiene tres vistas: **Procesos**, **Salud** y **Ajustes**.
- `domador-tray` deja un icono **animado** en la bandeja. Menú: Abrir Domador, Abrir carpeta, **Reiniciar como administrador**, Salir. Cerrar la UI no apaga el motor.
- La UI web HTML/JS sigue existiendo sólo como respaldo de la API (`http://127.0.0.1:8787/`). No es la experiencia principal y no se promociona como tal.

### Admin sin ceremonia (y con límites)

- La UI tiene **Activar admin** y la bandeja tiene **Reiniciar como administrador**: UAC una vez y la app vuelve elevada.
- `scripts/install-admin.ps1` crea la tarea programada `Domador` con `RunLevel Highest` (HighestAvailable), la verifica y retira el acceso duplicado de Startup. Es **idempotente**: si la tarea ya apunta al mismo `.exe`, no la recrea. Busca `domador-tray.exe` junto al script, en `scripts\` o en `%LOCALAPPDATA%\Programs\Domador`; por defecto **no arranca** la tarea (`-Start` es opt-in) para no duplicar el motor que ya está corriendo.
- `POST /api/admin-task` inicia la creación de la tarea en segundo plano (no bloquea el request) y busca `install-admin.ps1` al lado de `domador.exe`; `GET /api/admin-task` devuelve admin, estado de la tarea y job. Si distribuís el motor suelto, copiá el script junto a él.
- El instalador (`installer/`) puede registrar la tarea elevada como opción durante el setup, con UAC. **Todavía no hay artefacto compilado ni publicado**: el fuente existe y se prueba en la VM del release.

## Seguridad

Leé esto antes de apretar Full Power.

- **Full Power avisa si estás con batería, pero no lo bloquea.** La UI pide confirmación; por CLI/API se aplica igual. El perfil desactiva límites de energía y te la come en minutos: usalo enchufado.
- **Calienta y consume.** Es esperable. Si la notebook throttlea, el perfil no hace magia: sólo deja de frenar.
- **El plan de energía original se restaura en una salida ordenada**, también con Ctrl+C o `domador stop`. Si el proceso muere de golpe (crash, Task Manager), el plan queda como estaba en ese momento.
- **No requiere admin para lo básico**: prioridades, afinidad y EcoQoS sobre procesos propios funcionan sin elevar. Procesos del sistema y algunos servicios van a rechazar los cambios; la UI muestra `sin admin` cuando corre sin privilegios.
- **Sí requiere admin** para: prioridades sobre procesos de otros usuarios, `realtime`, crear la tarea programada y varias operaciones de plan de energía en máquinas con políticas de grupo.
- **La API escucha sólo en loopback** y valida el header `Host` (`127.0.0.1`/`localhost`) para cortar DNS rebinding. Los POST exigen `X-Domador: 1` y el body tiene un tope de **64 KiB**. Eso es un guard CSRF, **no autenticación**: cualquier proceso local puede llamarla. No la expongas a la red.
- **Sin telemetría.** No hay analytics, ni crash reporting, ni update check automático. El motor sólo habla con `127.0.0.1` y ejecuta herramientas locales (`powercfg`, `nvidia-smi`, PowerShell para la tarea admin). El único componente que sale a internet es `check-update.ps1`, y **sólo cuando lo ejecutás a mano**.

## Instalación

Requisitos: Windows 10 u 11 (x64), Rust 1.88 o posterior y un linker de Windows (MSVC Build Tools, o WinLibs si usás el toolchain GNU).

```bash
git clone https://github.com/jjmilano/domador.git
cd domador
cargo build --release --bins
```

Quedan tres ejecutables en `target/release/`: `domador.exe`, `domador-ui.exe` y `domador-tray.exe`. Copiá los tres a la misma carpeta (por ejemplo `%LOCALAPPDATA%\Programs\Domador`) y agregá esa carpeta al `PATH` si querés escribir `domador` desde cualquier lado.

Los iconos y frames de la bandeja se **incrustan en tiempo de compilación** (`build.rs` + `include_bytes!`): no hace falta distribuir `assets/` junto a los `.exe`. Las excepciones son `install-admin.ps1` y los scripts del instalador, que se buscan en disco.

### Instalador (fuente, todavía sin artefacto)

`installer/` contiene el setup per-user de Inno Setup 6.3+ y el pipeline de build:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1
```

El script compila los tres binarios, arma `dist\stage`, escribe `SHA256SUMS.txt`, compila `Domador-<version>-x64-setup.exe` y, si se pasa `-ReleaseBaseUrl`, genera `update.json`. No instala ni ejecuta nada. Opciones: `-SkipBuild`, `-SkipInstaller`, `-Sign` (requiere `signtool` y certificado), `-OutputDir`.

El setup es **per-user** (`PrivilegesRequired=lowest`): instalar y desinstalar no piden UAC. La tarea elevada es una opción del wizard y pide UAC una vez. También incluye `check-update.ps1` (chequeo manual, opt-in) y `verify-hashes.ps1`. **No hay release compilado con este instalador todavía**; el checklist de release exige probarlo en una VM limpia.

## Uso

```bash
domador                    # motor + UI nativa (si domador-ui.exe está al lado)
domador --no-open          # motor + API, sin abrir UI
domador serve              # sólo motor + API JSON
domador serve --port 9000
domador mcp                # servidor MCP por stdio (lo lanza el cliente, no vos)
domador status             # perfil, plan y top de procesos por consola
domador profile gaming juego.exe
domador power list         # planes disponibles
domador power full|restore
domador set --pid 4321 --priority high --affinity 0-3,8 --eco on
domador stop               # apaga el motor y restaura el plan
domador --version
```

`serve` no abre la UI nativa: deja la API JSON. La UI nativa habla con el puerto **8787 fijo**; si cambiás el puerto con `--port`, `domador-ui.exe` no va a encontrar el motor. Es una limitación conocida y está en el roadmap.

## Integración con IA

El servidor MCP habla JSON-RPC por stdio y expone herramientas para leer el estado, listar procesos, aplicar prioridad/afinidad/EcoQoS, administrar reglas y cambiar de perfil.

**Claude Desktop** (`%APPDATA%\Claude\claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "domador": {
      "command": "C:\\Tools\\domador\\domador.exe",
      "args": ["mcp"]
    }
  }
}
```

**Codex** (`~/.codex/config.toml`):

```toml
[mcp_servers.domador]
command = "C:\\Tools\\domador\\domador.exe"
args = ["mcp"]
```

**Hermes** (`~/.hermes/config.yaml`):

```yaml
mcp_servers:
  domador:
    command: "C:\\Tools\\domador\\domador.exe"
    args: ["mcp"]
```

Reiniciá el cliente después de agregarlo. Las herramientas se llaman `domador_status`, `domador_processes`, `domador_set_priority`, `domador_set_affinity`, `domador_set_eco`, `domador_profile`, `domador_add_rule`, `domador_restore`, `domador_power_plans` y `domador_log`.

El servidor MCP habla con la API en el puerto 8787 por defecto. Si levantás `serve --port 9000`, pasale el mismo a `mcp --port 9000` o definí `DOMADOR_PORT=9000` en el entorno del cliente.

### Endpoints HTTP

Todos son `POST` con body JSON salvo los `GET`. Respuesta: `{"ok": true}` o `{"ok": false, "error": "..."}`. Los POST requieren `X-Domador: 1`.

| Endpoint | Body | Qué hace |
|---|---|---|
| `GET /api/health` | — | Versión y estado mínimo del servidor |
| `GET /api/state` | — | Snapshot completo: sistema, memoria, GPU, procesos, familias, reglas, settings, MemGuard y contadores |
| `GET /api/mem` | — | Presión de memoria, umbrales de MemGuard, composición y top por memoria privada |
| `GET /api/families` | — | Agregado por familia (Chrome, WebView, Electron) |
| `GET /api/admin-task` | — | Estado de admin, tarea programada y job de creación |
| `GET /api/process?pid=1234` | — | Ficha bajo demanda: ruta, fabricante, padre, firma, comando y descripción conocida |
| `GET /api/legion` | — | Estado Legion por WMI (modo térmico, fans, temps, límites, tabla, LED del botón, competidores); siempre 200, ver `docs/LEGION-API.md` |
| `POST /api/legion/thermal` | `{"mode":3}` o `{"mode":"performance"}` | Cambia el modo térmico (1..4) |
| `POST /api/legion/fans` | `{"full_speed":true}` / `{"table":[1,2,...]}` | Full speed o curva de 10 niveles (1..10); exige backup stock |
| `POST /api/legion/cpu` | `{"long":45,"short":65}` | Límites de potencia CPU validados contra el firmware |
| `POST /api/legion/chief` | `{"enabled":true}` | Cierra Vantage/LegionArena/LLT/LFC/UXTU/RyzenController y reporta `killed`/`failed`/`skipped` |
| `POST /api/legion/restore` | `{}` | Restaura el backup de `legion.json` (tabla stock + modo previo) |
| `POST /api/meta` | `{"pid":1234}` | Detalle de un proceso (ruta, prioridad, afinidad, EcoQoS) |
| `POST /api/profile` | `{"name":"gaming","target":"juego.exe"}` | Cambia de perfil: `full_power`, `gaming`, `ai`, `balanced` |
| `POST /api/priority` | `{"pid":1234,"cls":"high"}` | Cambia la prioridad |
| `POST /api/affinity` | `{"pid":1234,"cpus":[0,1,2,3]}` | Fija afinidad de CPU |
| `POST /api/eco` | `{"pid":1234,"enabled":true}` | Activa/desactiva EcoQoS |
| `POST /api/io` | `{"pid":1234,"level":"low"}` | Prioridad de E/S |
| `POST /api/memory-priority` | `{"pid":1234,"level":"low"}` | Prioridad de memoria |
| `POST /api/plan` | `{"guid":"..."}` | Cambia el plan de energía |
| `POST /api/power` | `{"auto":true,"trigger":"juego.exe",...}` | Configura el cambio automático de plan |
| `POST /api/trim` | `{"pid":1234}` | Libera el working set de un proceso |
| `POST /api/trim-all` | `{}` | Libera el working set de todos los que superan `min_mb` |
| `POST /api/trim-idle` | `{}` | Alivio conservador: sólo procesos de fondo |
| `POST /api/mem/relief` | `{}` | Alivio manual de MemGuard (mismos criterios de fondo) |
| `POST /api/memguard` | `{"enabled":true,"warn_commit_pct":88,"warn_available_pct":7,"relief":false}` | Ajusta umbrales y alivio de MemGuard |
| `POST /api/family` | `{"family":"chrome","action":"trim"}` | Acción por familia: `priority`, `eco`, `io`, `mem`, `affinity`, `trim`, `restore` |
| `POST /api/restore` | `{"pid":1234}` | Restaura los valores originales de un proceso |
| `POST /api/restore-all` | `{}` | Restaura todos los cambios registrados |
| `POST /api/kill` | `{"pid":1234}` | Termina el proceso |
| `POST /api/gpu-preference` | `{"pid":1234,"mode":"high"}` | Fuerza GPU dedicada (`high`) o automática (`default`) |
| `POST /api/runaway` | `{"enabled":true,"cpu_pct":6,"minutes":5,"action":"warn"}` | Ajusta el watchdog de colgados (`warn`/`kill`) |
| `POST /api/rules` | `{"op":"create","rule":{...}}` | `create`, `update`, `delete`, `toggle`, `apply` |
| `POST /api/probalance` | `{"enabled":true,...}` | Ajusta ProBalance en caliente |
| `POST /api/settings/trim` | `{"enabled":true,"threshold":85,"min_mb":250,"interval_s":300}` | Ajusta SmartTrim |
| `POST /api/settings/foreground` | `{"enabled":true,"priority":"above_normal"}` | Ajusta el boost al proceso en primer plano |
| `POST /api/admin-task` | `{}` | Inicia la creación de la tarea elevada (requiere admin y el script al lado del exe); el estado se consulta con `GET /api/admin-task` |
| `POST /api/shutdown` | `{}` | Apaga el motor y restaura el plan |

```bash
curl -s http://127.0.0.1:8787/api/state | jq '.counts'
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"name":"gaming","target":"juego.exe"}' http://127.0.0.1:8787/api/profile
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"pid":4321,"cls":"high"}' http://127.0.0.1:8787/api/priority
```

## Benchmark

Medición real en esta notebook (Legion, Ryzen 7 5800H, 16 hilos lógicos, Windows 11), misma sesión y misma carga para los dos. Carga: 2 procesos `domador-hog` de 2 hilos con fases 12 s activo / 6 s idle / 12 s activo, lanzados en `above_normal` para que ambos motores vean lo mismo. Lasso corre con su configuración vigente (umbral 5% por proceso / 10% total, 9 s); Domador se configuró con umbral 5%, 9 s de sustain y 30 s de cooldown.

| Métrica | Process Lasso | Domador (motor) |
|---|---|---|
| RAM del motor | 20,0 MB | 17,3 MB |
| CPU del motor (idle) | 0,19% | 0,12% |
| Cambios de prioridad en 42 s (2 hogs) | 8 (4 por proceso) | 4 (2 por proceso) |
| Primer throttle | 0,1 s | 8,8 s |
| Latencia de acciones p50 / p95 | sin API ni CLI documentada | 2,37 / 25,42 ms |
| Distribución | instalador + servicio | 3 binarios v0.6.2: 2,6 + 5,5 + 0,6 MB |
| RAM de la UI | 64,8 MB (GUI nativa) | **no medido** en esta corrida |
| Churn real (30 min de su log) | 35 bajadas sobre 5 procesos, incluidos Chrome, WebView y ChatGPT | — |

> **Alcance de los números:** son del **motor v0.4.2**, medidos el 2026-11-11 con `bench/bench.py`; el sampler y ProBalance no cambiaron de fondo en v0.6.2, pero **no se re-midieron para v0.6.2**. La UI nativa es un proceso aparte y no forma parte de esa huella; su RAM todavía no está publicada. No se presentan como benchmark de la UI nueva.

**Lectura honesta:** Lasso gana en reacción inicial; Domador gana en huella (17,3 MB y 0,12% contra 20 MB y 0,19%), en estabilidad (la mitad de cambios de prioridad), en tamaño de distribución, en automatización (API/MCP) y en no necesitar servicio. La UI nativa desde v0.6 elimina la pestaña de Edge, pero eso es una mejora de arquitectura, no un número de FPS.

La vía rápida de ProBalance (25% durante 2 s) existe y está testeada en el motor, pero **no se midió en esta corrida**: el hog de 2 hilos de la prueba llega a 12,5% y usa la vía lenta. Falta archivarla en el JSON antes de citar un tiempo.

No hay FPS medidos todavía. El próximo benchmark serio es con CapFrameX: mismo juego, misma escena, con y sin perfil Juego.

**Reproducibilidad:**

```bash
cargo build --release --bins
python -m pip install -r bench/requirements.txt
python bench/bench.py --check     # valida binarios, hog, psutil y datos de Lasso
python bench/bench.py             # corre la medición (~2 min)
```

Los crudos quedan en `bench/results-rs.json`, el resumen en `bench/report.md` y la línea base de Lasso en `bench/lasso-results.json`. El script no maneja Lasso: esa línea base es un JSON cacheado de la medición original. Para volver a medir Lasso hay que hacerlo a mano con su configuración documentada en [`bench/README.md`](bench/README.md). La UI se mide sólo con `--ui` y requiere el puerto 8787 libre.

## Arquitectura

```
src/
├── main.rs     CLI: run, serve, mcp, status, profile, power, set, stop
├── lib.rs      Librería compartida por los binarios
├── model.rs    Config, reglas, perfiles, snapshots y tipos JSON
├── config.rs   Ruta y persistencia del config local
├── win.rs      Wrapper crudo de Win32 (OpenProcess, SetPriorityClass,
│               SetProcessAffinityMask, EcoQoS, EmptyWorkingSet, ventanas,
│               planes de energía, IsUserAnAdmin)
├── sys.rs      Sampler: un snapshot NT por tick + sysinfo sólo CPU/RAM global
├── nt.rs       NtQuerySystemInformation(ProcessInformation) y parser del buffer
├── gpu.rs      nvidia-smi y preferencia de GPU por ejecutable
├── engine.rs   Loop del motor: ProBalance, reglas, watchdogs, trim, perfiles
├── power.rs    Planes de energía, perfiles y restauración al salir
├── api.rs      Servidor HTTP local (tiny_http), auth CSRF y handlers JSON
├── mcp.rs      Servidor MCP por stdio sobre las mismas operaciones que la API
└── bin/
    ├── ui.rs     UI nativa egui/eframe (glow)
    ├── tray.rs   Bandeja animada (8 frames, Win32)
    └── hog.rs    Carga de prueba para benchmarks
├── docs/         Capturas, roadmap y empaquetado
└── tools/        Generador del icono
tests/          Tests de integración
bench/          Script, hog precompilado, baseline de Lasso y resultados
assets/         Iconos y frames de bandeja (se incrustan al compilar)
scripts/        install-admin.ps1
installer/      Inno Setup, build, hashes, updater y verificación
docs/           Captura, roadmap y packaging
```

El motor corre en un hilo propio y publica un snapshot que la API serializa; los handlers de escritura encolan acciones y esperan el resultado. `win.rs` es el único módulo con `unsafe`, y todo lo que abre un handle lo cierra con un guard. La UI nativa es un proceso aparte que habla con la API; se puede cerrar sin tocar el motor.

## Assets

Todos los assets gráficos se incrustan en los binarios (`build.rs` para el `.ico` de los `.exe`, `include_bytes!` para el logo de la UI y los frames de la bandeja). `assets/` no se distribuye junto al release.

| Archivo | Formato / tamaño | Uso | Estado |
|---|---|---|---|
| `assets/domador.ico` | `.ico`, 8 resoluciones (16/20/24/32/48/64/128/256) | Icono de los tres `.exe` vía `winres`; favicon de la API | **Steampunk integrado y embebido**; revisar 16 px |
| `assets/domador.png` | PNG 1024×1024 | Maestro/preview del icono; no se embebe | Steampunk, mantenimiento |
| `assets/favicon.ico` | `.ico`, 16/32/48 | Favicon alternativo para web/instalador; la API sigue sirviendo `domador.ico` | Nuevo, no embebido |
| `assets/icon-256.png` | PNG 256×256 | Logo grande de la UI (`include_bytes!`) y `/icon-256.png` | Integrado |
| `assets/icon-64.png` | PNG 64×64 | Logo del header de la UI y `/icon-64.png` | Integrado |
| `assets/icon-32.png` | PNG 32×32 | `/icon-32.png` para la UI web de respaldo | Integrado |
| `assets/tray/anim00–07.ico` | 8 `.ico` (16/20/24/32 cada uno) | Animación de bandeja a 140 ms; se copian a `%LOCALAPPDATA%\Domador\anim` | Steampunk integrado |
| `assets/tray/anim00–07.png` | 8 PNG 256×256 | Fuente/preview de los frames; no se embeben | Mantenimiento |
| `docs/icon-master-512.png` | PNG 512×512 | Master de revisión del icono | Fuente, no se publica |
| `docs/icon-preview.png` | PNG | Preview de tamaños/fondos | Revisión |
| `docs/screenshot.png` | PNG 1818×1172 | Captura del README | **Actualizada (v0.6.2)** |

Regenerar iconos: `python tools/make_icon.py` (requiere Pillow y numpy). El script ya genera el león steampunk con antiparras y engranaje; después de correrlo hay que **recompilar** para que `build.rs` y `include_bytes!` tomen los archivos nuevos.

## Checklist de release

Se corre a mano; CI no publica nada. Ningún paso se saltea porque "compila igual".

**Antes de etiquetar**

- [ ] `Cargo.toml` con la versión nueva y `Cargo.lock` actualizado; README y roadmap dicen la misma versión.
- [ ] `cargo fmt --all -- --check` sin diferencias.
- [ ] `cargo clippy --all-targets -- -D warnings` sin warnings.
- [ ] `cargo test --all-targets` en verde (14 pasan; 1 ignorado por bug conocido de body chunked).
- [ ] `cargo build --release --bins` genera los **tres** `.exe` y el icono steampunk está incrustado (Propiedades → Detalles; taskbar a 16 px).
- [ ] `python bench/bench.py --check` pasa; si se publican métricas nuevas, `python bench/bench.py` se corrió en la misma máquina y quedó `results-rs.json` con fecha.
- [ ] `powershell -File installer\build.ps1` genera el setup y `SHA256SUMS.txt` (requiere Inno Setup 6.3+; `-Sign` si hay certificado).
- [ ] Assets según la tabla: `domador.ico` multirresolución steampunk, 8 frames de bandeja, logos; sin capturas WIP sueltas en `assets/`.
- [ ] README: captura actual, tabla de assets, benchmark con alcance y limitaciones honestas.
- [ ] Roadmap: lo hecho marcado como hecho; lo pendiente sigue pendiente (tarjeta/MCP de MemGuard, captura, build/firma del instalador).

**Smoke test en Windows limpio o VM**

- [ ] `domador --version` devuelve la versión esperada.
- [ ] `domador serve --port 8788` responde `GET /api/health` y `GET /api/state`.
- [ ] Un POST sin `X-Domador` devuelve 403; un body > 64 KiB devuelve 413.
- [ ] `domador-ui.exe` abre, muestra Procesos/Salud/Ajustes, no corta texto a 1080p ni a 125%.
- [ ] `domador-tray.exe` anima, abre la UI y "Reiniciar como administrador" eleva.
- [ ] `domador stop` apaga el motor y restaura el plan de energía original.
- [ ] Cerrar la UI no apaga el motor; volver a abrirla se reconecta.

**Publicación (manual)**

- [ ] `sha256sum` de los tres `.exe` y del `.zip`; hashes en las notas del release.
- [ ] Release con los tres binarios, el setup (si se compiló), LICENSE, README y notas honestas (sin FPS inventados).
- [ ] Verificar la descarga desde otro equipo y que SmartScreen avise por falta de firma.
- [ ] Post-release: issues abiertos para lo que quedó pendiente; no prometer fechas.

## Roadmap

El detalle, prioridades y criterios de aceptación están en [`docs/ROADMAP-V2.md`](docs/ROADMAP-V2.md). Resumen:

- **P0:** cierre del rediseño de UI, captura nueva, tarjeta/tool MCP de MemGuard, build y prueba del instalador en VM, smoke test de UI.
- **P1:** puerto configurable para la UI, CPU sets, firma de código, historial de consumo, UI de grupos/startup, benchmark de UI y FPS.
- **P2:** más tools MCP, localización, detección de juegos por launcher.

## Limitaciones honestas

- **Windows 10/11 x64 solamente.** No hay build ARM64 ni Linux.
- **La UI nativa necesita una GPU/OpenGL funcional.** Si falla, queda la UI web de respaldo; no hay render por software garantizado.
- **La UI nativa usa el puerto 8787 fijo.** `--port` mueve la API y el MCP, no la UI.
- **ProBalance y SmartTrim vienen apagados.** Los perfiles no los activan; hay que prenderlos a conciencia.
- **MemGuard existe en motor y API, pero no en la UI.** Avisa y puede aliviar working sets de fondo; no mata. Se configura por `POST /api/memguard` o editando `memguard` en `config.json`; falta la tarjeta en Salud.
- **El watchdog puede matar** si se configura en `kill`; por defecto sólo avisa.
- **Sin firma de código y sin instalador publicado.** El fuente del setup está, pero no hay artefacto compilado/firmado; SmartScreen va a advertir.
- **El benchmark es de una sola máquina y una sola corrida.** No hay FPS ni pruebas de estrés de 24 h; la línea base de Lasso está cacheada.
- **La API no tiene autenticación real**, sólo loopback + header CSRF. No la expongas.
- **Bug conocido:** un body `chunked` mayor a 64 KiB devuelve 400 en vez de 413; hay un test ignorado que lo documenta.
- **La captura sigue siendo de v0.6.0** y los binarios ya publicados de v0.6.2 son anteriores al icono steampunk; el build local ya lo tiene embebido.
- **No hay telemetría, update check automático ni crash reporting.** El chequeo de updates es manual y opt-in (`check-update.ps1`); si algo falla, el log local es lo único que hay.

## Contribuir

Se aceptan PRs. Leé [CONTRIBUTING.md](CONTRIBUTING.md) antes de mandar uno: ahí está el flujo, el estilo y qué se espera de un cambio.

## Licencia

MIT. Ver [LICENSE](LICENSE).
