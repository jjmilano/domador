# QA v2 — Domador 0.6.2

Fecha: 2026-11-13 15:20–16:00 (AST). Commit base: `4c61555` **con cambios sin commitear** en
`src/api.rs`, `src/engine.rs`, `src/win.rs`, `src/bin/ui.rs`, `assets/`, `installer/`.
El árbol se estaba editando en paralelo mientras corría esta QA: los números de línea son los
del estado del archivo a las 15:52 y pueden haberse movido si el archivo cambió después.

## Qué se verificó

| Prueba | Comando | Resultado |
|---|---|---|
| Suite completa | `cargo test` | 14 pasan, 0 fallan, 1 ignorado (bug conocido). Ver nota de lock más abajo |
| Build release (todo) | `cargo build --release` | OK, `domador.exe` / `domador-ui.exe` / `domador-tray.exe` |
| API en vivo (release) | `domador.exe serve --port 8802/8810` con `DOMADOR_CONFIG` aislado | 40+ casos |
| Auth / Host / métodos | sin `X-Domador`, Host `evil.example`, `PUT`/`DELETE`/`OPTIONS` | 403 / 403 / 405 |
| Tope de cuerpo | `Content-Length` 70 KiB → 413; chunked 200 KiB → **400** (bug) |
| Snapshot | `/api/state` 543 procesos, 241 KB, `counts` consistentes | OK salvo payload |
| Trim | `/api/trim` sobre `hog` propio; `/api/trim-idle`; `/api/family chromium trim` | libera y reporta bytes |
| Admin | `GET /api/admin-task`, `POST` sin admin | 200 con estado real, 403 correcto |
| UI web | Chromium a 1400×900, 4 pestañas + modales | renderiza, sin errores JS |
| UI nativa | `domador-ui.exe` (egui) sobre motor 0.6.1 | renderiza y pollea; sin fuga de handles |
| Fugas | muestreo 60 s de handles/RSS de motor y UI | motor 240→234 handles, 14.5 MB; UI 374→371, 82→84.6 MB |

Tests propios: `tests/qa_contract.rs` (9 casos, uno `#[ignore]` que documenta el bug del chunked).
Todos levantan el servidor en **puerto 0** con config temporal: no dependen de un motor instalado
ni de 8787.

```
cargo test --test qa_contract              # 8 pasan, 1 ignorado
cargo test --test qa_contract -- --ignored # reproduce el bug del chunked (falla a propósito)
```

---

## Estado de los hallazgos (v0.7.0, rama `mejoras-v0.7`)

| # | Estado | Cómo |
|---|---|---|
| A1 | Resuelto | GPU y planes en hilo propio (`main.rs`, `Engine::use_external_probes`); `/api/state` ~4 ms sin picos |
| A2 | Resuelto | Mutex con nombre por config (`win::single_instance`); el segundo `run` abre la UI contra el motor existente |
| A3 | Resuelto | Lectura con tope `MAX_BODY + 1`; `body_cap_chunked_deberia_ser_413` ya no está ignorado |
| A4 | Resuelto | `gaming` exige `target`; `"auto"` usa la última app que no es Domador. Test en `smoke.rs` |
| A5 | Resuelto | `hist` con `skip_serializing`; poll de la UI a 1 s. ~240 KB → ~143 KB |
| A6 | Resuelto (API) | `trim_pids` fuera del lock en `trim-all`, `trim-idle` y `mem/relief`. El alivio automático de MemGuard sigue dentro del tick |
| A7 | Resuelto | `open_ui` pasa `DOMADOR_PORT`; la UI avisa si la versión del motor no coincide |
| A8 | Abierto | — |
| A9 | Resuelto | `cpus` con elementos no enteros → 400 "cpus debe ser una lista de enteros". Test en `smoke.rs` |
| A10 | Resuelto | `api_roundtrip_and_auth` borra su config |
| A11 | Parcial | Header PID alineado, flechas de orden dibujadas, acciones sólo en hover; contraste de microcopy sin medir |

## Hallazgos

### A1 · Alta · `/api/state` lanza procesos externos con el mutex del motor tomado → tirones de 100–250 ms en la UI

`snapshot()` consulta la GPU con `nvidia-smi` cada 5 s y reenumera los planes de energía con
PowerShell cada 60 s, **dentro del handler HTTP y mientras tiene el `Mutex<Engine>`**.

- **Reproducción**: pollear `/api/state` cada 700 ms (lo que hace la UI nativa) y mirar la latencia.
- **Evidencia** (build release, 543 procesos):
  ```
  poll /api/state cada 700ms (ms): 246  9  8  18  19  9  9  9  129  11  9  8
  bytes=241502
  ```
  Base 8–11 ms; los picos de 18–246 ms coinciden con el refresco de GPU/planes. El primer
  `/api/state` después de arrancar tarda 227–246 ms.
- **Archivo/línea**: `src/engine.rs:2240` (`snapshot`), `:2241` (`last_plan_scan`),
  `:2256` (`gpu_at.elapsed() > 5.0` → `gpu::query()`), `src/gpu.rs:12-19` (`nvidia-smi`),
  `src/api.rs:157` (`GET /api/state`).
- **Fix sugerido**: mover `gpu::query()` y `refresh_plans()` al hilo del tick (o a un hilo propio
  con resultado cacheado) y que `snapshot()` sólo lea el cache. Nunca spawnear procesos dentro
  del lock que comparten todos los endpoints.

### A2 · Alta · Sin instancia única: dos motores con el mismo config se pisan (lost update)

`api::serve` cae al puerto siguiente si 8787 está ocupado, así que un segundo `domador run`
arranca un **segundo motor** en vez de reusar el existente. Ambos escriben el mismo `config.json`
con su copia en memoria completa (`save_config`), y gana el último.

- **Reproducción** (dos instancias, mismo `DOMADOR_CONFIG`):
  ```
  POST :8810 /api/settings/trim      {"enabled":true,"threshold":70}
    archivo -> trim.enabled True  threshold 70.0
  POST :8811 /api/settings/foreground {"enabled":false}
    archivo -> trim.enabled False threshold 85.0   <-- se perdió el cambio de 8810
  ```
- **Evidencia**: verificado con el binario release de las 15:49; el archivo revierte
  `enabled` y `threshold` a los valores viejos de la otra instancia.
- **Archivo/línea**: `src/engine.rs:353` (`save`) y `:2636` (`save_config`, escritura de archivo
  completo); `src/api.rs:21-34` (fallback de puerto); `src/bin/ui.rs:28` (`PORT` fijo).
- **Fix sugerido**: lock de instancia única (mutex con nombre / archivo `.lock` en `%LOCALAPPDATA%`
  con PID) y, si ya hay motor, abrir la UI contra ese puerto. Si se permite más de una instancia,
  guardar con merge por clave leída del disco justo antes de escribir.

### A3 · Media · El tope de 64 KiB se elude con `Transfer-Encoding: chunked` (400 en vez de 413, cuerpo truncado)

El chequeo usa `request.body_length()`, que es `None` sin `Content-Length`. Después
`take(MAX_BODY).read_to_string()` corta el cuerpo a 64 KiB en silencio y el JSON parcial
devuelve “JSON inválido”.

- **Reproducción**: POST chunked de 200 KiB a `/api/memguard` con `X-Domador: 1`.
- **Evidencia**:
  ```
  [chunked 200KiB] HTTP/1.1 400 Bad Request  {"error":"JSON inválido","ok":false}
  [CL 70KiB]       HTTP/1.1 413              {"error":"cuerpo demasiado grande"}
  ```
  Test que lo fija: `tests/qa_contract.rs::body_cap_chunked_deberia_ser_413` (`#[ignore]`,
  falla hoy si se corre con `--ignored`).
- **Archivo/línea**: `src/api.rs:79` (`MAX_BODY`), `:255-271` (chequeo + `take`).
- **Fix sugerido**: leer hasta `MAX_BODY + 1` y, si el buffer llegó al tope o el JSON no cierra,
  responder 413 sin intentar parsear. Alternativa: rechazar `Transfer-Encoding` en POST.

### A4 · Media · `POST /api/profile {"name":"gaming"}` sin `target` agarra la ventana en foco y cambia el plan de energía global

La rama `gaming` hace `target.or_else(|| self.system.foreground_name)`, así que el error
“Elegí el ejecutable del juego” es inalcanzable con cualquier ventana abierta.

- **Reproducción**: `POST /api/profile {"name":"gaming"}` sin `target`, con una ventana en foco.
- **Evidencia**: responde `200` con `profile.active = "gaming"`, `plan = "Domador Full Power"`,
  `previous_plan = <plan anterior>`; cambia el plan de energía del sistema y sube a `high` la
  prioridad del proceso en foco. (En esta QA se revirtió de inmediato con `{"name":"balanced"}`;
  el motor del usuario volvió a aplicar su propio plan.)
- **Archivo/línea**: `src/engine.rs:1921` (rama `"gaming"`), `src/api.rs:476` (`/api/profile`).
- **Fix sugerido**: exigir `target` explícito cuando el pedido viene por API; si se quiere el
  fallback, que sea un flag (`"target":"auto"`) y que la UI lo muestre antes de aplicar.

### A5 · Media · `/api/state` pesa 240–263 KB por poll y el 25 % es `hist` por proceso, que ninguna UI usa

- **Evidencia**: 543 procesos, 241 502 bytes; `processes` = 267 KB del total, `hist` = 12 845
  valores (24 por proceso, `HIST_LEN`), `history` global 150 puntos (11.8 KB), `log` 12.9 KB.
  La UI nativa pollea cada 700 ms (≈ 345 KB/s de JSON) y la web cada 1.5 s. Además
  `state_json()` serializa el snapshot a `Value` y después `reply()` lo vuelve a serializar.
- **Archivo/línea**: `src/model.rs:295` (`ProcessRow.hist`), `src/sys.rs:18` (`HIST_LEN`),
  `src/engine.rs:2308` (`state_json`, doble serialización), `src/bin/ui.rs:208` (700 ms),
  `ui/app.js:129` (1500 ms).
- **Fix sugerido**: sacar `hist` del payload (o endpoint aparte `?fields=`), serializar `Snapshot`
  directo sin pasar por `Value`, y bajar el poll a 1–2 s en la UI nativa.

### A6 · Media · El `Mutex<Engine>` global serializa el API: un trim bloquea todo lo demás

Cualquier operación larga se ejecuta con el lock tomado, y el resto de los endpoints espera.

- **Evidencia** (release): `/api/trim-idle` 453 ms con `/api/state` en 280–328 ms en paralelo
  (normal 8–11 ms); `/api/mem/relief` 466 ms liberando 9 procesos; `/api/family chromium trim`
  287 ms.
- **Archivo/línea**: `src/api.rs:366-394` (trim/relief toman `lock(engine)`), `src/engine.rs:861`
  (`trim_all`), `:883` (`trim_background`).
- **Fix sugerido**: copiar el snapshot necesario y soltar el lock antes de tocar procesos, o mover
  trim/relief a un job en background con progreso (el patrón `AdminJob` de `api.rs:506` ya sirve).

### A7 · Baja · La UI nativa fija `127.0.0.1:8787` y no valida la versión del motor

En esta máquina el motor instalado es **0.6.1** y el repo es 0.6.2: los endpoints nuevos
(`/api/mem`, `/api/families`, `/api/memguard`) devuelven 404 y la UI no lo distingue de un fallo.

- **Evidencia**: `curl 127.0.0.1:8787/api/health` → `{"version":"0.6.1"}`; la UI muestra `v0.6.1`;
  `POST 8787 /api/memguard` → `404 {"error":"no encontrado"}`.
- **Archivo/línea**: `src/bin/ui.rs:28` (`const PORT: u16 = 8787`), `:139` (`base()`),
  `:2820` (texto “127.0.0.1:8787” hardcodeado); `src/main.rs:89-124` (`open_ui` no pasa el puerto).
- **Fix sugerido**: aceptar `--port`/`--url` (y que `open_ui` lo pase), y comparar
  `app.version` con `VERSION` mostrando un aviso claro (“motor viejo: reiniciá Domador”).

### A8 · Baja · `GET /api/admin-task` cuesta 2.2 s la primera vez

El handler llama a `admin_task_status()` (spawnea `schtasks`/PowerShell) con caché de 5 s.

- **Evidencia**: `GET /api/admin-task` → 2264 ms, 25 ms, 1 ms en llamadas seguidas.
  `POST` sin admin → `403` correcto; el alta real ahora es asíncrona con script embebido
  (`src/win.rs:640`, `src/api.rs:567`) y verificación posterior: bien.
- **Archivo/línea**: `src/api.rs:169` (GET), `:548` (`cached_admin_status`, TTL 5 s).
- **Fix sugerido**: TTL de 30–60 s y refresco en hilo aparte; que el GET devuelva sólo el cache
  y dispare la consulta si está viejo.

### A9 · Baja · `/api/affinity` confunde “tipo inválido” con “lista vacía”

`filter_map(|v| v.as_u64())` descarta los elementos no numéricos y después el motor responde
“Elegí al menos un núcleo”.

- **Evidencia**: `{"pid":4,"cpus":["0","1"]}` → `400 {"error":"Elegí al menos un núcleo"}`.
- **Archivo/línea**: `src/api.rs:324-335`, `src/engine.rs:734` (`set_affinity`).
- **Fix sugerido**: si algún elemento no es entero, 400 “cpus debe ser una lista de enteros”.

### A10 · Baja · `tests/smoke.rs` deja configs en `%TEMP%`

- **Evidencia**: 23 archivos `domador-test-api-*.json` acumulados en `%TEMP%` (uno por corrida);
  `api_roundtrip_and_auth` no borra el suyo (los otros tests sí).
- **Archivo/línea**: `tests/smoke.rs:90` (`api_roundtrip_and_auth`), sin `remove_file` al final.
- **Fix sugerido**: borrar el config en un `Drop`/al final del test. `tests/qa_contract.rs` ya lo hace.

### A11 · Baja · UI nativa: contraste bajo y columnas numéricas desalineadas

- **Evidencia** (captura 1456×933 de `domador-ui.exe`): footer `127.0.0.1:8787`, “ordenar por” y
  los placeholders quedan casi invisibles sobre el fondo; en la tabla los headers `PID/CPU/RAM`
  están alineados a la izquierda y los valores a la derecha; la columna `Acciones` mide 238 px
  fijos y deja un hueco muerto a la derecha.
- **Archivo/línea**: `src/bin/ui.rs:1783` (`Column::exact(238.0)`), `:2820` (footer),
  estilos en `C_MUTED`/`FAINT`.
- **Fix sugerido**: subir un escalón el color de la microcopy (≥ 4.5:1), alinear headers
  numéricos a la derecha y dimensionar `Acciones` al contenido.

### A12 · Informativo · Costo de fondo del motor

`app.tick_ms` con 543 procesos: **p50 26.6 ms, p95 57.8 ms** por tick de 1 s; RAM del motor
14.5 MB y handles estables (240→234 en 60 s). La RAM del conjunto la explica la UI: 82–90 MB la
nativa y bastante más la web con WebView2. No se vieron fugas de handles ni de memoria en 60 s
en ninguno de los dos procesos.

---

## Lo que se probó y salió bien

- Auth: 403 sin `X-Domador`, con valor distinto de `1`, y con `Host` no-loopback
  (`evil.example`); `Host: localhost:PUERTO` y `127.0.0.1:PUERTO` aceptados; header en minúsculas
  aceptado (comparación case-insensitive correcta).
- Semántica HTTP: 405 para `PUT`/`DELETE`/`OPTIONS` y para `POST` sobre endpoints de sólo lectura
  (`/api/health`, `/api/state`, `/api/mem`); 404 para endpoint inexistente en GET y POST.
- Validación: `Falta pid`, `Prioridad inválida`, afinidad fuera de rango (`Núcleos válidos: 0 a 15`),
  `Falta enabled`, `No hay cambios guardados para ese proceso`, `Perfil desconocido`, `Plan de
  energía desconocido`, `operación desconocida`, `familia desconocida`, clamps de `memguard`
  (99 / 0.5 / 60 / 86 400), `trim/min_mb/threshold` acotados.
- Nuevos endpoints: `GET /api/mem`, `GET /api/families`, `/api/mem/relief`, `/api/memguard`,
  `/api/family` (priority/eco/io/trim) responden con la forma esperada; `state_json` agrega
  `memguard`, `mem_pressure`, `families` y `processes[].family` sin romper las claves viejas
  (verificado en `tests/qa_contract.rs::snapshot_contract_y_counts_consistentes`).
- Trim: sobre un `hog` propio libera y reporta bytes (5.3 MB la primera vez, 0 la segunda);
  `/api/trim-idle` y `/api/family chromium trim` liberan memoria real y devuelven `count`.
- Admin: sin admin el POST da 403 con mensaje claro; el alta es asíncrona, con script embebido
  (`include_str!` de `scripts/install-admin.ps1`) y verificación por estado real de la tarea.
- UI web: renderiza las 4 pestañas y los modales de regla/ajustes sin errores JS; 521 procesos
  en tabla; el modal de regla crea todos sus campos.
- UI nativa: arranca, encuentra el motor, muestra versión del motor, 518 procesos, y no fuga
  handles (374→371 en 60 s).

## Nota de entorno: `cargo test`/`cargo build` fallan si hay una UI abierta

Con `target/debug/domador-ui.exe` (o `target/release/...`) corriendo, Cargo no puede reemplazar el
binario y corta con:

```
error: failed to remove file `...\target\debug\domador-ui.exe`
```

Para verificar el árbol actual se corrió la suite con `CARGO_TARGET_DIR` aparte
(`cargo test` → 14 pasan, 1 ignorado). Si es esperable que la UI esté abierta durante el
desarrollo, conviene documentarlo en CONTRIBUTING o usar un `target` alternativo.

## Efectos secundarios de esta QA (para transparencia)

- Se levantaron motores propios en **8801, 8802, 8803, 8810, 8811** con `DOMADOR_CONFIG` en
  `%TEMP%\qa-domador\`; todos se apagaron con `/api/shutdown` o `Stop-Process` sobre el PID propio.
  Al motor del usuario (8787, PID 27620) y su tray sólo se les hicieron GETs de lectura; siguen
  corriendo. La UI que el usuario tenía abierta (PID 35880) se cerró por su lado, no por esta QA.
- Un `POST /api/profile {"name":"gaming"}` (A4) cambió el plan de energía del sistema a
  “Domador Full Power” durante ~1 minuto; se revirtió a `balanced` y el motor del usuario volvió
  a aplicar su propio plan (`full_power`), que es el estado en el que estaba.
- Se ejecutaron `trim-idle` (2 veces), `mem/relief` (1) y `family chromium trim` (1): liberan
  working sets, no matan procesos.
- Durante la QA el árbol estaba siendo editado por otros procesos: dos `cargo build` fallaron
  por errores de compilación transitorios (`src/win.rs:694` `Option<i64>` vs `i32`,
  `src/api.rs` `script` fuera de scope, `src/bin/ui.rs` `Color32` como función). El build final
  de las 15:50 compila limpio; si vuelven a aparecer, son del árbol en movimiento, no de esta QA.

## Sugerencia de orden de arreglo

1. A1 (spawns bajo lock) y A6 (lock global) — son la causa de los tirones que se ven como “UI rota”.
2. A2 (instancia única) — corrupción silenciosa de configuración.
3. A3 (tope de cuerpo) — es un bypass del límite de seguridad.
4. A4 (gaming sin target) — cambia estado global del sistema sin confirmación.
5. A5 (payload) — la mejora de RAM/CPU más barata.
6. A7–A11 — pulido.
