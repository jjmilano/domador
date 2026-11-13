# Domador V2 — Roadmap priorizado

Objetivo: convertir a Domador en una cabina de control **steampunk** para Windows, no sólo en un procesador de prioridades. Tiene que verse bien, entenderse rápido, no romperse al cambiar de tamaño y resolver problemas reales de RAM/CPU sin placebo.

Este documento es el roadmap **de producto, release y benchmark**. El detalle de pantallas, tokens y flujos vive en `docs/UX-ROADMAP-V2.md`, que es de otro agente y **no se edita desde acá**. Cuando hay solapamiento, manda el de UX para la UI y este para el alcance/entrega.

## Cómo leer los estados

| Estado | Significado |
|---|---|
| **Hecho** | Está en el árbol y verificado por código, test o build. |
| **En curso** | Hay trabajo iniciado, pero no está cerrado ni verificado end-to-end. |
| **Pendiente** | No está implementado. No se documenta como si existiera. |
| **Diferido** | Se decidió no hacerlo en V2 salvo que cambie el contexto. |

Regla de oro: **ninguna métrica nueva entra al README sin un archivo de resultados, fecha, máquina y versión**. Si no hay medición, se escribe "pendiente", no un número estimado.

## Estado verificado — árbol v0.6.2 (2026-11-13)

| Área | Estado | Evidencia |
|---|---|---|
| Motor nativo Rust (Win32/NT) | Hecho | `src/win.rs`, `src/nt.rs`, `src/sys.rs` |
| UI nativa egui/eframe (`glow`, OpenGL) | Hecho, rediseño en curso | `src/bin/ui.rs`; `Cargo.toml` usa `eframe` con feature `glow` |
| Tres binarios | Hecho | `domador.exe`, `domador-ui.exe`, `domador-tray.exe` |
| Bandeja animada + menú admin | Hecho | `src/bin/tray.rs`, 8 frames embebidos, timer 140 ms |
| API local + CSRF + Host loopback + body 64 KiB | Hecho | `src/api.rs` |
| Servidor MCP por stdio | Hecho | `src/mcp.rs`; test de handshake |
| ProBalance con vía rápida y cooldown | Hecho (opt-in) | `src/engine.rs`; default `enabled=false` |
| Reglas por ejecutable y persistencia | Hecho | `src/model.rs`, `src/engine.rs`; test de persistencia |
| SmartTrim + `trim-idle`/`trim-all` | Hecho (opt-in) | `src/engine.rs`, endpoints `/api/trim*` |
| MemGuard | **Hecho en motor + API + tests** | `memguard_step`/`set_memguard` en `src/engine.rs`, `GET /api/mem`, `POST /api/memguard`, `POST /api/mem/relief`; prueba `memguard_acota_los_umbrales`; falta tarjeta en UI y tool MCP |
| Familias Chrome/WebView/Electron | **Hecho en motor + API, sin UI** | `family_of`, `families()`, `GET /api/families`, `POST /api/family`; `ui.rs` todavía no lo consume |
| Diagnóstico de memoria/GPU/colgados | Hecho | pestaña Salud, `src/gpu.rs`, `Runaway` |
| Perfiles de energía | Hecho (plan + boosts) | `set_profile` en `src/engine.rs` |
| Admin por UAC + script + endpoint | Hecho (manual) | `src/bin/tray.rs`, `src/bin/ui.rs`, `scripts/install-admin.ps1`, `/api/admin-task` |
| Icono steampunk | **Hecho y embebido en build local** | `assets/domador.ico` 8 resoluciones, 8 frames; `build.rs` lo incrustó en el build 15:40; falta revisión visual 16/32 px y captura |
| Instalador Inno + hashes + updater | **Hecho en fuente, sin artefacto** | `installer/domador.iss`, `build.ps1`, `check-update.ps1`, `verify-hashes.ps1`; no hay `dist/` |
| Rediseño de UI | **En curso** | `src/bin/ui.rs` modificado (layout de Salud/Ajustes); sin cierre ni captura |
| Captura del README | **Hecho** | `docs/screenshot.png` actualizada a v0.6.2 |
| Benchmark reproducible | **En curso** | `bench/bench.py` actualizado; falta re-medir y publicar UI/FPS |
| CI de tres binarios + assets + bench check | Hecho en este cambio | `.github/workflows/ci.yml` |

Tests actuales: **14 en verde, 1 ignorado** (`cargo test --all-targets`) — controles de proceso, reglas persistentes, API/auth/Host/body cap, MCP, snapshot NT, `powercfg`, contrato de `/api/state`, familias, topes de MemGuard, trim y robustez ante pedidos inválidos. El ignorado es un bug conocido: body `chunked` > 64 KiB devuelve 400 en vez de 413. No hay test automatizado de la UI, la bandeja ni el instalador.

## Principios

1. **Nativo de verdad**: motor, UI y bandeja en Rust; el navegador es sólo respaldo de la API, no la experiencia principal.
2. **Datos honestos**: cada acción muestra qué cambió y con qué costo; cada número tiene fuente.
3. **Seguro por defecto**: nada de matar apps automáticamente; lo destructivo se confirma. El watchdog viene en `warn` y MemGuard nunca mata.
4. **Estética con función**: el steampunk jerarquiza estado (latón/cobre para lo activo, no decoración porque sí).
5. **Medible y reversible**: tests, benchmark y una captura verificada por release; toda feature se puede desactivar.

## No-goals de V2

- No ser un "RAM cleaner" que libera todo y vende humo: el alivio es conservador y medible.
- No matar procesos por presión de memoria sin confirmación.
- No telemetría, cuentas, nube ni update check obligatorio.
- No soporte Linux/macOS ni ARM64 en V2.
- No reescribir el motor ni cambiar de toolkit de UI en V2; se pule egui.
- No tocar `docs/UX-ROADMAP-V2.md` desde este flujo.

## Prioridades

### P0 — cierre de V2 (bloquea release)

| # | Trabajo | Estado | Dueño sugerido | Criterio de aceptación |
|---|---|---|---|---|
| P0.1 | Cerrar rediseño de UI nativa | En curso | Agente de UI | Sin texto cortado a 1080p/1440p/125%; foco visible; contraste AA; captura nueva |
| P0.2 | Captura y assets del README | Pendiente | Docs | `docs/screenshot.png` de la UI nueva; tabla de assets sin archivos WIP |
| P0.3 | MemGuard: tarjeta en UI y tool MCP | Hecho (v0.7.0), falta medición antes/después | UI + MCP | Tarjeta en Salud con nivel, último alivio y "Aliviar ahora"; tool `mem_info`; medir antes/después |
| P0.4 | Build y prueba del instalador | Pendiente | Packaging | `installer/build.ps1` genera setup + hashes; instalación/desinstalación limpia en VM; tarea elevada opcional funciona |
| P0.5 | Smoke test de UI | Pendiente | QA | Arranque, tres tabs, cambio de perfil, alivio, cierre sin matar motor |
| P0.6 | Release gate ejecutado | Pendiente | Release | Checklist completo, hashes, tres `.exe` + setup, benchmark con alcance |
| P0.7 | Verificación visual del icono | En curso | Build | Los tres `.exe` ya lo incrustan; falta revisar 16/32 px, taskbar y bandeja |

### P1 — después del cierre, misma línea V2

| # | Trabajo | Estado | Criterio de aceptación |
|---|---|---|---|
| P1.1 | Puerto configurable para la UI | Hecho (v0.7.0, `DOMADOR_PORT`) | `domador-ui --port N` o `DOMADOR_PORT`; deja de estar hardcodeado 8787 |
| P1.2 | CPU sets | Pendiente | Aplicar CPU sets además de máscaras; fallback a afinidad si el SO no lo soporta |
| P1.3 | Firma de código | Pendiente | `build.ps1 -Sign` produce setup y `.exe` firmados; `verify-hashes -RequireSignature` pasa |
| P1.4 | Benchmark de UI nativa | Pendiente | RAM/CPU de `domador-ui.exe` medidos con `bench.py --ui`, publicados con fecha |
| P1.5 | Benchmark FPS | Pendiente | CapFrameX, mismo juego/escena, con y sin perfil Juego; metodología archivada |
| P1.6 | Historial de consumo | Pendiente | Serie por proceso con retención acotada; sin crecimiento infinito |
| P1.7 | UI de grupos Chrome/WebView/Electron | Hecho (v0.7.0, tarjeta Familias: Eco/Liberar/Restaurar) | Acción por familia (el motor ya agrega `families`), confirmación para matar |
| P1.8 | Gestión de inicio | Pendiente | Listar/deshabilitar startups con confirmación y reversión |
| P1.9 | Más tools MCP | En curso (`mem_info`, `trim_idle`, `families` hechos; falta `admin_status`) | `mem_info`, `trim_idle`, `admin_status`; errores JSON-RPC correctos |
| P1.10 | Actualización opt-in integrada | Pendiente | La UI puede lanzar `check-update.ps1`; nunca silencioso |
| P1.11 | Legion 5 Pro: modos térmicos, fans y límites de CPU | Pendiente | WMI `LENOVO_GAMEZONE_DATA`, `LENOVO_FAN_METHOD`, `LENOVO_CPU_METHOD`; admin, backup de curva stock y un solo dueño del EC. Detalle en `docs/LEGION-TUNING.md` |

### P2 — exploratorio

| # | Trabajo | Estado | Nota |
|---|---|---|---|
| P2.1 | Localización es/en | Pendiente | No bloquear V2 |
| P2.2 | Historial de 24 h con gráficos | Pendiente | Requiere diseño de retención |
| P2.3 | Perfil Focus/Suave | Pendiente | Prioridad foreground + EcoQoS de fondo, sin tocar visible |
| P2.4 | Detección de juegos por launcher | Pendiente | Steam/Epic/Battle.net; no romper reglas manuales |
| P2.5 | Más telemetría local | Diferido | Sólo si es local, opt-in y exportable; nunca nube |

## P0.3 — MemGuard: qué está y qué falta

**Implementado en el motor** (`memguard_step`, `MemGuard` en `src/engine.rs`):

- Lee commit usado/límite, RAM disponible y cache del snapshot.
- Umbrales: `warn_commit_pct` 88% y `warn_available_pct` 7%, con nivel `critical` a +7 puntos de commit o la mitad del disponible.
- Cooldown de avisos: 900 s (`warn_cooldown_s`).
- Alivio automático **opt-in** (`relief=false` por defecto), intervalo mínimo 900 s, sólo procesos de fondo vía `trim_background`.
- **Nunca termina procesos** y no toca la ventana en primer plano.
- Config persistida en la sección `memguard` del `config.json`; `set_memguard` valida rangos.

**Falta para considerarlo producto:**

- Tarjeta en **Salud** con nivel, commit/disponible, último alivio y botón "Aliviar ahora" (manual, con confirmación).
- Tool MCP `mem_info`/`memguard`.
- Tests de histéresis y de "no tocar visibles/no matar" (hoy hay test de topes y contrato).
- Medición antes/después en una carga real de commit; sin eso no se publica una métrica de "MB liberados".

**Riesgo:** convertir esto en el "RAM cleaner" que el proyecto critica. Mitigación: default `warn`, `relief=false`, copy explícito y métricas antes/después.

## P0.1 — UI nativa: criterios de cierre

El rediseño está en curso en `src/bin/ui.rs`. Este roadmap no impone tokens ni layouts (eso es `docs/UX-ROADMAP-V2.md`), sólo el contrato de entrega:

- Tres vistas: Procesos, Salud, Ajustes; sin duplicar "perfil activo".
- Tabla escaneable: columnas alineadas, orden, selección visible, acciones con label/tooltip.
- Salud: commit/límite, cache, pool, top de memoria privada, GPU sin cortes; MemGuard visible cuando P0.3 cierre.
- Ajustes: ProBalance, SmartTrim, MemGuard, admin y motor con toggles reales.
- Estados vacío/error/cargando; ventana maximizada y 125%.
- Cero emojis como iconos funcionales; iconos dibujados o texto.
- Captura final para README y verificación de que el motor no se apaga al cerrar la UI.

**Aceptación:** `cargo build --release --bins`, smoke test manual, captura revisada, sin texto cortado ni controles sin affordance.

## P0.4 — Instalador: fuente lista, artefacto pendiente

**Implementado en `installer/`:**

- `domador.iss`: setup **per-user** (`PrivilegesRequired=lowest`), x64, Windows 10+, instala los tres binarios + `install-admin.ps1` + `activate-admin.cmd` + `check-update.ps1` + `verify-hashes.ps1` + `SHA256SUMS.txt`.
- Tareas opcionales: acceso de escritorio, bandeja en Startup y **tarea elevada** (`startup_admin`); si elegís la tarea, no se crea el Startup duplicado.
- Verificación SHA-256 post-instalación con los hashes que pasa el build; desinstalación pregunta antes de borrar la tarea y **preserva** `%LOCALAPPDATA%\Domador` (config/reglas).
- `build.ps1`: compila con `--locked`, stage, firma opcional, hashes, ISCC, `update.json` opcional.
- `check-update.ps1`: chequeo manual, HTTPS, hash obligatorio, Authenticode opcional, instalación silenciosa per-user.
- `verify-hashes.ps1`: manifiesto SHA256SUMS + firma opcional.
- `scripts/install-admin.ps1`: idempotente, verifica `RunLevel HighestAvailable`, no arranca la tarea salvo `-Start`.

**Falta:**

- Compilar el setup en una máquina con Inno Setup 6.3+ y adjuntar hashes.
- Probar instalación, actualización, desinstalación y tarea elevada en VM limpia.
- Decidir firma de código (P1.3); sin certificado, documentar SmartScreen.
- Publicar `update.json` sólo si se decide habilitar el updater manual.

**Aceptación:** en una VM limpia, instalar → reiniciar sesión → Domador arranca elevado sin UAC si se eligió la tarea; desinstalar deja el sistema sin tarea ni Startup, y conserva la config.

## P0.7 — Icono steampunk: qué falta

**Estado:** los maestros están en `assets/` (león con antiparras de latón, engranaje, fondo carbón). `domador.ico` tiene 16/20/24/32/48/64/128/256 y los 8 frames de bandeja tienen 16/20/24/32. El build release local ya **incrustó** el icono en los tres `.exe` (verificado extrayendo el recurso del `.exe`).

**Falta:**

- Revisar legibilidad a 16/32 px en taskbar, Alt-Tab y bandeja; a 32 px el detalle fino se pierde y hay que decidir si la variante simplificada alcanza.
- Verificar que la bandeja anima a 140 ms sin CPU perceptible.
- Actualizar la captura y la tabla de assets con el estado final.

**Aceptación:** icono legible a 16 px, sin artefactos a 125/150%, tres `.exe` con el recurso nuevo y bandeja animando.

## P0.6 — Release gate

Se ejecuta a mano. CI construye y verifica, pero **no publica**.

### Pre-flight

- [ ] Versión en `Cargo.toml`/`Cargo.lock`; README y roadmap alineados.
- [ ] `cargo fmt --all -- --check`.
- [ ] `cargo clippy --all-targets -- -D warnings`.
- [ ] `cargo test --all-targets` (14 pasan; 1 ignorado conocido: chunked > 64 KiB).
- [ ] `cargo build --release --bins` → tres `.exe` con icono steampunk incrustado.
- [ ] `python bench/bench.py --check`.
- [ ] Si hay métricas nuevas: `python bench/bench.py` en la misma máquina; `results-rs.json` con fecha y versión.
- [ ] `powershell -File installer\build.ps1` → setup + `SHA256SUMS.txt`; `verify-hashes.ps1` pasa.
- [ ] Assets: `.ico` multirresolución, 8 frames, logos; sin capturas WIP.
- [ ] README: captura nueva, tabla de assets, benchmark con alcance y limitaciones.
- [ ] Roadmap: estados actualizados; pendientes siguen pendientes.

### Smoke test (VM Windows limpia)

- [ ] `domador --version`.
- [ ] `domador serve --port 8788` + `GET /api/health` + `GET /api/state`.
- [ ] POST sin `X-Domador` → 403; body > 64 KiB → 413.
- [ ] UI: tres tabs, perfil, alivio, cierre sin matar motor, reapertura.
- [ ] Bandeja: animación, abrir UI, reiniciar como admin.
- [ ] `domador stop` restaura el plan de energía.
- [ ] Instalador: instalar sin UAC, tarea elevada opcional, actualización, desinstalación limpia.

### Publicación

- [ ] `sha256sum` de los tres `.exe`, setup y `.zip`.
- [ ] Release con binarios, setup, LICENSE, README y notas honestas.
- [ ] Verificar descarga desde otro equipo; documentar SmartScreen sin firma.
- [ ] Issues para pendientes; sin promesas de fecha.

## P1.4/P1.5 — Benchmark: qué se puede afirmar

**Ya medido (motor v0.4.2, 2026-11-11, Ryzen 7 5800H / 31,9 GB / Windows 11):**

- RAM del motor 17,3 MB vs 20,0 MB de Lasso.
- CPU idle 0,12% vs 0,19%.
- 4 cambios de prioridad vs 8 en 42 s con 2 hogs.
- Primer throttle 8,8 s vs 0,1 s.
- Latencia p50/p95 2,37/25,42 ms (Lasso sin API documentada).
- Churn de Lasso: 35 bajadas en 30 min sobre 5 procesos.

**No medido / no publicable todavía:**

- RAM/CPU de `domador-ui.exe`.
- FPS en juegos.
- Vía rápida de ProBalance (25%/2 s) archivada en JSON.
- Estrés de 24 h, múltiples máquinas, ARM64.
- Comparación justa de "RAM app completa": la GUI de Lasso y la UI nativa no son el mismo tipo de proceso.
- Efecto real de MemGuard (MB liberados) sin degradar apps visibles.

**Requisitos para publicar un número nuevo:**

1. `bench/results-rs.json` con `generated`, versión, binarios y configuración.
2. Misma configuración de Lasso documentada en `bench/README.md`; si no se re-midió Lasso, decirlo.
3. Una sola variable por comparación; no mezclar UI con motor.
4. Fecha, máquina y versión al lado de la tabla.
5. Si el número no se puede reproducir, no va al README.

**Trabajo pendiente:**

- `bench.py --ui`: medir UI en 8787, RAM/CPU, con puerto libre o skip explícito.
- `bench.py --quick`: smoke para CI, sin publicar resultados.
- CapFrameX: mismo juego/escena, con y sin perfil Juego; guardar capturas y CSV.
- Documentar que la línea base de Lasso es un JSON cacheado, no algo que el script mide.

## Dependencias y orden

1. **Cierre de UI + captura + verificación del icono** (P0.1/P0.2/P0.7): desbloquea README y release.
2. **MemGuard producto** (P0.3): depende del motor (ya está) y de la UI de Salud.
3. **Instalador** (P0.4): depende de los binarios finales; si no se firma, se publica con hashes y aviso de SmartScreen.
4. **Tests/smoke/release gate** (P0.5/P0.6): al final, con todo integrado.
5. **Puerto configurable** (P1.1): antes de documentar `--port` como completo; hoy la UI usa 8787 fijo.
6. **Benchmark UI/FPS** (P1.4/P1.5): después del cierre visual; no bloquear el release por FPS.

## Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| El rediseño de UI no cierra a tiempo | Alto | Release con UI actual + captura honesta; no publicar mockups |
| `state_json`/`mem_report`/`families` ya cableados, pero la UI no los usa | Medio | Tarjeta de MemGuard y acciones por familia en UI, o documentar que son sólo API |
| MemGuard degrada apps visibles | Alto | Default warn; alivio sólo fondo; confirmación; medir antes/después |
| Tarea elevada bloqueada por política de grupo | Medio | Detectar y mostrar error; no prometer admin automático |
| El instalador no se prueba en VM | Alto | Gate P0.4; no publicar setup sin smoke de instalación/desinstalación |
| Benchmark mezcla versiones/máquinas | Alto | Gate de métricas: fecha/versión/máquina/JSON o no se publica |
| UI hardcodea 8787 y choca con otra instancia | Medio | P1.1; mientras tanto, documentar y detectar puerto ocupado |
| CI se vuelve lento/frágil | Bajo | Cache, pasos informativos para fmt/clippy si el árbol no está limpio, bench `--check` sin cargas |
| Otro agente toca los mismos archivos | Alto | Este flujo sólo edita README/bench/CI/roadmap; UI/icono son archivos disjuntos |

## Assets: estado y contrato

| Asset | Rol | Estado V2 |
|---|---|---|
| `assets/domador.ico` | Icono de los tres `.exe` + favicon API | **Steampunk integrado y embebido**; falta revisión 16 px |
| `assets/domador.png` | Maestro/preview 1024×1024 | Steampunk, mantenimiento |
| `assets/favicon.ico` | Favicon alternativo 16/32/48 | Nuevo, no embebido |
| `assets/icon-256.png` | Logo UI + `/icon-256.png` | Integrado |
| `assets/icon-64.png` | Header UI + `/icon-64.png` | Integrado |
| `assets/icon-32.png` | `/icon-32.png` | Integrado |
| `assets/tray/anim00–07.ico` | Animación de bandeja (8 frames, 16–32) | Integrado; animación 140 ms |
| `assets/tray/anim00–07.png` | Fuente de frames 256×256 | Mantenimiento |
| `docs/icon-master-512.png` | Master de revisión | Fuente, no se publica |
| `docs/icon-preview.png` | Preview de tamaños | Revisión |
| `docs/screenshot.png` | Captura README | **Desactualizada (v0.6.0)**; reemplazar al cerrar UI |

Contrato: todo asset que se embebe se declara en la tabla del README; todo asset que no se embebe se marca "mantenimiento"; ninguna captura WIP se publica.

## Definition of Done (V2)

Una tarea está hecha cuando:

- El código compila en release y los tests pasan.
- Tiene test o smoke test reproducible (o se explica por qué no).
- La UI muestra el estado/error, no falla en silencio.
- Está documentada en README o roadmap, con limitaciones.
- No agrega telemetría ni dependencias de runtime innecesarias.
- Se puede desactivar/revertir.
- Si toca métricas, tiene JSON con fecha/máquina/versión.

## Registro de cambios de este roadmap

- **2026-11-13 (segunda pasada):** se actualiza al estado real del árbol: assets steampunk integrados y embebidos, MemGuard implementado en motor + API + tests (falta UI/MCP), familias expuestas por API, instalador Inno completo en fuente pero sin artefacto. Se corrige el estado de UI "wgpu" (es `glow`/OpenGL), se aclara que los perfiles no prenden ProBalance y se mueve el foco de P0 a cerrar producto, no a inventar features.
