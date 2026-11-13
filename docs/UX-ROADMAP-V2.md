# Domador — Auditoría UX y plan de flujos (V2)

> Documento de diseño e interacción. **No implementa nada**: define qué tiene que pasar, cómo se ve, qué dice y cómo se verifica. El código se toca en otra rama/PR, contra este contrato.
>
> Base auditada: **v0.6.2** (`Cargo.toml`, HEAD `9e9cf07`), `src/bin/ui.rs` (1.315 líneas), `src/api.rs`, `src/engine.rs`, `src/model.rs`, `src/bin/tray.rs`, `docs/screenshot.png` (captura v0.6.0, 1818×1172) y `docs/ROADMAP-V2.md`.
> Hay un **rediseño sin commitear en curso** (`src/bin/ui.rs` pasó a ~2.940 líneas a las 15:30 del 13/11). Donde el WIP ya resuelve un punto, se marca **`[WIP]`**; donde el WIP arrastra el problema, se marca **`[WIP ✗]`**. Los números de línea del WIP pueden moverse: se citan símbolos, no líneas.

---

## 0. Cómo leer esto

| Si sos… | Empezá por |
|---|---|
| Quien decide | §1 Resumen, §14 Backlog |
| Quien diseña | §6 Principios, §7 Sistema visual, §9 Flujos |
| Quien implementa | §5 Auditoría, §9 Flujos, §12 Robustez, §14 Backlog |
| Quien prueba | §13 Criterios de aceptación, §15 Validación |

Convenciones: `F-xx` = hallazgo de auditoría; `UX-Px-yy` = ítem de backlog; `AC-xx` = criterio de aceptación. Los textos entre comillas son **copy final propuesto**, en español rioplatense.

---

## 1. Resumen ejecutivo

**El motor no es el problema.** La API ya expone 25 rutas (2 GET + 23 POST: prioridad, afinidad, EcoQoS, E/S, memoria, reglas con watchdog, perfiles, planes de energía, trim, kill, admin). La UI nativa usa **10**, y casi todas con un único parámetro fijo. La brecha no es de capacidad sino de **contrato de interacción**.

Tres causas raíz explican casi todo lo que el usuario reporta como “fea, se rompe, no sé qué toca”:

1. **La UI es un visor de snapshot, no un cuaderno de operaciones.** Muestra el estado, pero no lo que Domador *hizo*, *por qué* ni *cómo volver*. `log` (400 entradas) e `history` (150 puntos) viajan en cada respuesta y no tienen superficie. Las reglas existen, se aplican y pueden cerrar procesos, y no hay pantalla.
2. **El feedback es global y tardío.** Una sola barra inferior de 30 px comunica éxito, busy y error; en v0.6.2 el toast **no expira nunca** y tapa el estado busy. Además, varias llamadas HTTP corren en el hilo de UI (`refresh`, “Actualizar”, `relaunch_admin` con `sleep(800ms)`): la ventana se congela y el usuario lee “se rompió”.
3. **Supuestos fijos que se rompen en uso real.** Puerto 8787 hardcodeado mientras el motor puede bindear 8788+; `take(300)` sobre 502 procesos con el contador diciendo “502 activos”; header de 76 px y mínimo 1080×700 que no sobreviven a 125/150 % de escala; tres tabs sin lugar para reglas, actividad o emergencias.

**Lo que está bien y hay que conservar:** paleta cálida coherente; contraste de *texto* medido entre 6,8:1 y 16,3:1 (cumple AA/AAA); motor rápido; snapshot completo y honesto; el WIP ya introdujo tokens, toasts con expiración, `egui::Modal` para el kill, atajos y estados vacíos.

**La oportunidad “suave, rica y deliciosa” no es más animación.** Es latencia percibida (toda acción responde en <100 ms con estado local), continuidad (no perder el contexto al actuar) y confianza (saber qué se tocó y cómo volver). Un solo gesto bien resuelto —“Terminar…” con nombre, ruta, hora de inicio y consecuencia— vale más que veinte transiciones.

**Números que resumen la deuda:** 34 hallazgos (**19 P0**, 13 P1, 2 P2). La mayoría de los P0 son de **confianza** (acciones peligrosas, estados invisibles, números que mienten), no de estética.

---

## 2. Método y alcance

**Qué se leyó:** UI nativa completa, API y dispatch, modelo de datos, motor (perfiles, reglas, trim, runaway, watchdog), bandeja, README, roadmap vigente, captura publicada y capturas del WIP.

**Qué se midió de verdad:**

- Contraste WCAG de todos los pares texto/fondo y borde/superficie con los hex reales del código (fórmula de luminancia relativa, script reproducible).
- Tamaños tipográficos y de target declarados en `ui.rs` (`TextStyle`, `button_padding`, alto de fila).
- Estructura y costo del snapshot (campos por proceso, 24 muestras de CPU por proceso, 150 puntos de historial, 400 líneas de log).

**Qué NO se midió:** no se levantó el motor para no alterar la sesión del usuario (podría aplicar SmartTrim o reglas locales). El tamaño del snapshot es una **estimación estructural** (~330 KB con 500 procesos + ~44 KB de historial/log), marcada como tal. Las latencias son objetivos de diseño, no mediciones.

**Límite importante:** `src/bin/ui.rs` está siendo reescrito por otro agente mientras se escribe esto. La auditoría describe v0.6.2 y evalúa el WIP como estado parcial; el backlog está formulado por **resultado**, no por línea, para que sobreviva al merge.

**Relación con `docs/ROADMAP-V2.md`:** ese documento manda sobre alcance y release; éste manda sobre interacción, copy y criterios UX. Hay una tensión explícita: su criterio de cierre P0.1 fija **tres vistas** (Procesos, Salud, Ajustes), y acá se proponen **cinco** (se suman Reglas y Actividad). Recomendación: para cerrar V2, mantener tres vistas y colgar Reglas y Actividad como sub-secciones con segmented control dentro de Procesos y Salud; promoverlas a tabs en V2.1. Si se prefiere la propuesta de cinco, el cambio de criterio se hace en `ROADMAP-V2.md`, no acá. `UX-P0-07` incluye ambas variantes.

---

## 3. Personas

### P1 — Nico, 32, desarrollador de IA (persona primaria)

- **Equipo:** Legion, 32 GB, Windows 11. Corre 2–3 agentes (Hermes/Claude/Codex), Chrome con 40 pestañas, IDE, Docker, terminal.
- **Trabajo:** que el agente termine sin que la notebook se arrastre; que el navegador no se congele mientras compila; recuperar el estado “normal” cuando termina.
- **Sabe:** qué es una prioridad; no sabe (ni quiere) qué es “working set privado” ni “pool no paginado”.
- **Frustración actual:** “Subir” manda a *High* sin preguntar; no hay forma de dejar la prioridad fija entre sesiones desde la UI (las reglas no tienen pantalla); no sabe si el cambio sobrevivió al reinicio del proceso.
- **Criterio de éxito:** en ≤2 clics sube el runtime y lo deja persistente como regla, y puede ver en Actividad que se aplicó a los 4 procesos correctos.
- **Frase:** “No quiero un Task Manager con esteroides; quiero que mi agente corra y poder volver atrás.”

### P2 — Sofi, 27, juega y estudia (persona secundaria)

- **Equipo:** notebook con 16 GB, juega con Discord + Chrome + Spotify abiertos.
- **Trabajo:** “modo Juego” y que al salir todo vuelva como estaba.
- **Sabe:** que existe un plan de energía; no sabe qué es Full Power ni por qué hay dos badges de admin.
- **Frustración actual:** aprieta “Juego” en el escritorio y recibe un error técnico (“Elegí el ejecutable del juego”) sin ningún lugar donde elegirlo. Le da miedo “Fin”.
- **Criterio de éxito:** un clic, ver “Modo Juego activo · juego.exe · plan Alto rendimiento”, y al volver a Equilibrado, nada raro.
- **Frase:** “Decime qué va a pasar antes de que pase.”

### P3 — Oscar, 58, usuario intensivo no técnico (persona de accesibilidad cognitiva)

- **Equipo:** notebook de 16 GB, la usa para trabajo, banco, fotos, videollamadas.
- **Trabajo:** “se puso lenta” → quiere un veredicto y un botón seguro.
- **Sabe:** abrir el Administrador de tareas y cerrar cosas (a veces de más).
- **Frustración actual:** la pantalla Salud muestra “Commit”, “Pool paginado”, “Handles”, “Regulados” sin explicar; no hay veredicto (“¿estoy bien o mal?”). El único botón grande no dice cuánto va a liberar ni a qué procesos afecta.
- **Criterio de éxito:** en <10 s entiende si el problema es memoria, CPU o un proceso colgado, y puede resolverlo sin miedo.
- **Frase:** “¿Está bien o está mal? ¿Y qué toco?”

### P4 — Vero, 41, vista cansada, trabaja con teclado y escala 150 % (persona de accesibilidad)

- **Trabajo:** operar todo sin mouse, leer sin esfuerzo, con lector de pantalla ocasional (Narrator).
- **Frustración actual:** textos de 9–10 px; targets de 22–26 px; filas de tabla no focusables; `↺` sin nombre accesible; build sin AccessKit (`eframe` con `default-features = false` y sólo `glow` + `default_fonts`); sin atajos en v0.6.2.
- **Criterio de éxito:** completa “buscar → seleccionar → prioridad → confirmar” sólo con teclado, con foco visible y todo anunciado.
- **Frase:** “Si no puedo tabular, no existe.”

### P5 — Agente/automatización (persona técnica, no usuaria de la UI)

- Opera por MCP/CLI (`domador_profile`, `domador_add_rule`, etc.). No usa la ventana, pero **la ventana tiene que mostrar lo que el agente hizo**, con origen (“MCP”, “CLI”) y posibilidad de deshacer. Si la UI y el agente se pisan, gana la trazabilidad.

### Anti-persona (explícita)

El usuario que quiere un “limpiador de RAM” de un clic que mate todo y libere cache. **El producto no lo complace**: mata la confianza y el posicionamiento (“no vendemos placebo”, README). El flujo de memoria está diseñado para *explicar* por qué no.

---

## 4. Mapa de pantallas

### 4.1 Hoy (v0.6.2)

| Superficie | Contenido | Problema estructural |
|---|---|---|
| Barra superior | Marca, badge admin, CPU, RAM, versión | Badge y botón “Activar admin” compiten con la marca; métricas sin base ni tooltip |
| Barra de perfiles | 4 chips + `PERFIL` | Duplica “PERFIL ACTIVO” del sidebar; sin descripción, sin estado de aplicación, sin picker para Juego |
| Sidebar | Procesos / Salud / Ajustes + ESTADO | Contadores duplicados; “Reglas 0” es un dead end; no hay Actividad ni Reglas |
| Procesos | Toolbar + tabla 6 columnas + 4 acciones/fila | 1.200 widgets potenciales; acciones ambiguas; 300 filas silenciosas |
| Salud | Memoria, kernel, GPU, top privada | Sin veredicto; sin historial; sin acciones; jerga |
| Ajustes | ProBalanza, admin, motor | 4 de ~30 parámetros; toggle ambiguo; “Hacer permanente” sin confirmación |
| Modal kill | Nombre + PID | Sin ruta, sin hora de inicio, sin peligro visual, sin foco inicial |
| Barra inferior | Toast / busy / error | Un solo slot; toast permanente; error sin salida |
| Bandeja | Abrir / carpeta / admin / salir | Dos modelos de cierre (UI ≠ motor) sin comunicarlo |
| UI web | Respaldo completo con reglas y ajustes | Convive y genera dudas de “cuál es la buena” |

### 4.2 Propuesto

```
┌─ Domador ─────────────────────────────────────────────────────────────────┐
│ TOPBAR   marca · estado del motor · ADMIN · CPU · RAM · versión · ⚙       │
│ PROFILE  [Equilibrado] [Juego] [IA] [Full Power]   activo hace 4 min · ⓘ  │
├──────────┬────────────────────────────────────────────────────────────────┤
│ NAV      │ CONTENIDO                                                      │
│ Procesos │  toolbar: buscar · filtros · orden · densidad                  │
│ Salud    │  tabla / tarjetas / lista de reglas / timeline                 │
│ Reglas   │                                        ┌─ DRAWER ─────────┐   │
│ Actividad│                                        │ detalle proceso  │   │
│ Ajustes  │                                        │ acciones         │   │
│          │                                        └──────────────────┘   │
├──────────┴────────────────────────────────────────────────────────────────┤
│ STATUSBAR  ● motor OK · hace 0,7 s · última acción: "Regla aplicada…"  ×  │
└───────────────────────────────────────────────────────────────────────────┘
OVERLAYS: onboarding · confirmación · selector de juego · editor de regla ·
          motor caído · ayuda de atajos
```

**Regla de IA:** cinco secciones de primer nivel, una sola por trabajo. “Reglas” y “Actividad” ascienden porque son la memoria y la explicación del producto; sin ellas, todo lo automático es magia (o sospecha).

**Compatibilidad con el cierre de V2:** si `ROADMAP-V2.md` mantiene “tres vistas” como criterio, la versión de tres tabs usa un segmented control dentro de Procesos (`Procesos · Reglas`) y dentro de Salud (`Estado · Actividad`), con los mismos contenidos y atajos (`Ctrl+3` Reglas, `Ctrl+4` Actividad). La IA de cinco es la forma final; la de tres es el envoltorio mínimo para no bloquear el release.

**Mapa de estados transversales** (toda pantalla los contempla):

| Estado | Cuándo | Qué se ve |
|---|---|---|
| `loading` | Primer snapshot / tras reiniciar motor | Skeleton con la forma del contenido, no spinner infinito |
| `empty` | Sin datos legítimos (0 reglas, sin coincidencias) | Explicación + acción primaria |
| `offline` | Sin respuesta del motor | Banner persistente con Reintentar / Iniciar / Ver registro |
| `degraded` | Motor viejo o campos faltantes | Banner “Motor desactualizado” + Reiniciar motor |
| `pending` | Acción en curso | Control local en `busy` + barra fina de progreso; nunca bloquea el resto |
| `error` | Acción rechazada | Mensaje en el punto de la acción + salida concreta |
| `stale` | Snapshot >3 s | Punto ámbar en STATUSBAR + “datos de hace N s” |

---

## 5. Auditoría

Severidad: **P0** = rompe confianza, seguridad o uso diario; **P1** = fricción alta o función invisible; **P2** = pulido.

### 5.1 Arquitectura de información y estado

| ID | Sev | Evidencia | Qué pasa | Por qué importa | Fix |
|---|---|---|---|---|---|
| F-01 | P1 | `ui.rs` `profile_bar` + `side` (`PERFIL ACTIVO`) + chip activo | El perfil se muestra 3 veces; “Procesos 502” y “502 activos” 2 veces | El usuario no sabe cuál es la fuente de verdad; el ojo no encuentra el estado | `UX-P0-05`, `UX-P0-07` |
| F-02 | **P0** | `counts.rules` visible; `/api/rules` con `create/update/delete/toggle/apply`; no hay `Tab::Rules` | Se muestran reglas pero no se pueden ver, editar ni apagar | Función central invisible; un watchdog activo puede cerrar apps sin que el usuario sepa por qué | `UX-P0-08`, `UX-P1-04` |
| F-03 | **P0** | `snapshot.log` (400 entradas) viaja cada 700 ms; ninguna vista | Domador actúa solo (ProBalanza, reglas, trim, watchdog) y no hay historial visible | “No sé qué hizo” = desconfianza; imposible auditar un cambio | `UX-P1-01` |
| F-04 | P1 | `history` (150 pts) y `hist` por proceso (24 muestras) en el snapshot; sin gráfico | Datos pagados en cada poll, cero valor | Se desperdicia la mejor prueba de “esto sirve” | `UX-P1-05` |
| F-05 | P1 | `enum Tab { Processes, Health, Settings }` | No hay lugar para emergencias, reglas, actividad ni admin | Todo lo urgente cae en un toast de 30 px | `UX-P0-07` |

### 5.2 Confianza y acciones peligrosas

| ID | Sev | Evidencia | Qué pasa | Por qué importa | Fix |
|---|---|---|---|---|---|
| F-06 | **P0** | `small_button("Fin")`, `kill_dialog` | Acción irreversible con etiqueta eufemística, mismo estilo que “Subir”, sin copy de consecuencia | Un clic + Enter puede cerrar trabajo sin guardar | `UX-P0-04` `[WIP parcial]` (ya es `danger` y bloquea protegidos; falta copy, ruta y foco) |
| F-07 | **P0** | `/api/kill` sólo recibe `pid`; `engine.kill` | Si el PID se reutiliza entre abrir el diálogo y confirmar, se mata otro proceso | Es el bug clásico de Task Manager; acá es evitable y no se evita | `UX-P0-04` |
| F-08 | **P0** | `DEFAULT_PROTECT` (explorer, dwm, audiodg, lsass…) se usa en trim/ProBalance, **no** en `kill` | La UI puede terminar explorer/dwm/audiodg | Pantalla negra o audio roto = “Domador me rompió Windows” | `UX-P0-04` `[WIP parcial]` (deshabilita “Fin” si `protected`; el endpoint sigue sin validar) |
| F-09 | **P0** | `watchdog_step`: `max_instances` termina las instancias “más nuevas” sin confirmación | Una regla creada por API/MCP puede cerrar procesos sola | Peligro invisible y no reversible | `UX-P0-08`, `UX-P1-03` |
| F-10 | **P0** | `runaway.action = "kill"` (`Runaway::default` = `warn`) | Cambiar a `kill` es API-only y sin fricción; el motor mata y sólo loguea | El usuario puede convertir un aviso en un asesino automático sin leer nada | `UX-P1-03` |
| F-11 | **P0** | `set_profile("full_power")`: si `!ac_online()` sólo `log("warn")`; luego `boost_patterns(ai, "khigh")` | Con batería no confirma y sube **todos** los patrones de IA a High | Consumo y calor inesperados; cambio de alcance no declarado | `UX-P0-05` |
| F-12 | P1 | `set_profile("ai")` agrega patrones a `config.probalance.protect` y **persiste** | Efecto lateral permanente; volver a Equilibrado no lo revierte | El comportamiento futuro cambia sin rastro | `UX-P0-05` (con cambio de motor) |
| F-13 | **P0** | `settings` → `self.action("/api/admin-task", ...)` | Crea una tarea programada elevada con un clic, sin confirmación ni resultado visible | Acción de sistema con efecto duradero y sin recibo | `UX-P0-14` |
| F-14 | **P0** | `relaunch_admin`: `post("/api/shutdown")` + `sleep(800)` + `Start-Process -Verb RunAs` | Apaga el motor de todos los clientes; si se cancela el UAC, queda apagado | “Se rompió” literal: la UI queda sin datos y sin explicación | `UX-P0-14` |
| F-15 | P1 | `/api/trim-all`, `/api/restore-all` existen; la UI sólo expone `trim-idle` y `restore` por fila | Acciones masivas sin confirmación ni conteo cuando se agreguen | Riesgo de jank masivo o pérdida de prioridades | `UX-P0-04`, `UX-P0-09` |

### 5.3 Ambigüedad y copy

| ID | Sev | Evidencia | Qué pasa | Por qué importa | Fix |
|---|---|---|---|---|---|
| F-16 | **P0** | `"Subir"` → `cls:"high"` fijo; `"↺"` sin label | “Subir” no dice a qué; no hay opción intermedia ni undo visible; `↺` es un glifo sin nombre | El usuario no puede predecir ni corregir | `UX-P0-06` `[WIP ✗]` (ya dice “Restaurar”, pero sigue el salto fijo a High y 4 botones por fila) |
| F-17 | P1 | Toggle ProBalanza: botón cuyo texto es el **estado** (“Activado”) | No se distingue estado de acción; sin umbrales | No sabe si va a activar o desactivar | `UX-P0-09` |
| F-18 | P2 | `win::priority_label`: “Idle” en inglés, “Muy alto” en español | Idioma mezclado en la misma columna | Se lee como descuido | `UX-P0-06` |
| F-19 | P1 | `post()` devuelve `HTTP {code}: {body}`; `get_state` “motor no disponible: …” | Errores crudos y en inglés técnico | El usuario no sabe qué hacer | `UX-P0-03` |
| F-20 | P1 | “62 % RAM”, “Regulados 0”, “Commit 12,3/18 GB” | Sin base, sin unidad legible, sin definición | Números que no informan | `UX-P0-13` |
| F-21 | P1 | Barra “Sólo toca procesos sin ventana visible, con CPU baja y working set grande” | Describe el criterio pero no cuántos ni cuáles; `trim_background` usa 192 MB hardcodeado vs `min_mb` 250 | El usuario no puede prever el efecto | `UX-P0-13` |

### 5.4 Feedback y estados

| ID | Sev | Evidencia | Qué pasa | Por qué importa | Fix |
|---|---|---|---|---|---|
| F-22 | **P0** | `Shared.toast` seteado y nunca limpiado (HEAD); `toast` tiene prioridad sobre `busy`/`error` | Un “Listo” queda para siempre y tapa el estado de la próxima acción | Feedback que miente y bloquea información | `UX-P0-03` `[WIP]` |
| F-23 | **P0** | `refresh()` en `update` (si `state.is_none()`), botón “Actualizar”, `relaunch_admin` | Llamadas HTTP + `sleep` en el hilo de UI | Congelamiento visible; jank | `UX-P0-02` `[WIP parcial]` (`refresh_bg` y acciones ya van a un hilo; `relaunch_admin` sigue bloqueando 800 ms) |
| F-24 | **P0** | `const PORT: u16 = 8787` en `ui.rs` y `tray.rs`; `api::serve` prueba 8788..8797 | Si el puerto está ocupado, la UI habla con otra instancia o levanta un segundo motor | “A veces abre vacío” / dos motores | `UX-P0-01` |
| F-25 | P1 | `/api/health` existe y nadie lo usa | UI nueva + motor viejo = campos faltantes silenciosos | Pantallas vacías sin diagnóstico | `UX-P0-01` |
| F-26 | **P0** | `filtered.iter().take(300)` + `format!("{} activos", procs.len())` | Dice 502, muestra 300, sin avisar | Números que mienten | `UX-P0-10` `[WIP]` (virtualiza con `body.rows`) |
| F-27 | P1 | `Health` sin `state`: sólo “Esperando al motor…” | Sin acción, sin causa, sin reintento | Dead end | `UX-P0-03` `[WIP]` (error card con “Reintentar”; falta “Iniciar motor”) |

### 5.5 Accesibilidad y layout

| ID | Sev | Evidencia | Qué pasa | Por qué importa | Fix |
|---|---|---|---|---|---|
| F-28 | **P0** | `eframe` con `default-features=false`, features `["glow","default_fonts"]` | **AccessKit deshabilitado**: sin árbol de accesibilidad para Narrator | La app es inoperable con lector de pantalla | `UX-P0-11` |
| F-29 | **P0** | TextStyle: Heading 21, Body 13, Button 12,5, Small 11; labels 9–11; fila 26; `small_button` ~22–24 | Texto y targets por debajo de lo usable a 125/150 % | “Bajo contraste” percibido = tamaño + densidad real | `UX-P0-09`, `UX-P0-11` `[WIP ✗]` (subió a 34 px de fila y 30 px de botón, pero quedan captions de 9,5–10 px) |
| F-30 | **P0** | Medición: texto 6,8–16,3:1 (**OK**); superficies 1,05–1,16:1; borde/card **1,47:1**; zebra **1,04:1** | El problema no es el color del texto: **no se distinguen las superficies ni los límites de los controles** (WCAG 1.4.11 pide 3:1) | Todo se ve “apretado” y los botones no tienen affordance | `UX-P0-09` `[WIP ✗]` (`BORDER` 1,49:1 y `FAINT` 4,14:1 a 9,5 px) |
| F-31 | P2 | `visuals.override_text_color = Some(C_TEXT)`; `C_RED` definido y nunca usado | Se aplastan estados (disabled/hover/link); el peligro no tiene color | Controles planos y acciones peligrosas camufladas | `UX-P0-09` |
| F-32 | **P0** | `with_min_inner_size([1080,700])`, header 76 px, sidebar 186, columnas mínimas 666 + márgenes | A 1366×768 con 125 % el contenido se recorta; el header no entra | “Se corta todo” en notebooks reales | `UX-P0-10` |
| F-33 | P1 | `eframe` sin feature `persistence`; `config.ui.sort` existe sin uso | Tamaño, posición, zoom, orden y tab se resetean | Sensación de app improvisada | `UX-P1-07` |
| F-34 | P1 | Cerrar UI no apaga motor; “Salir” de bandeja sí | Dos modelos de salida sin explicación | Usuario cree que apagó y sigue corriendo (o al revés) | `UX-P1-08` |

### 5.6 Detalle de los tres hallazgos visuales (con números)

**a) Contraste estructural, no de texto.** Medición sobre los hex de v0.6.2:

| Par | Ratio | Norma | Veredicto |
|---|---|---|---|
| `#faf0e2` texto / `#1a130f` panel | **16,28:1** | AA 4,5 | sobra |
| `#c6af9a` muted / panel | **8,74:1** | AA 4,5 | bien |
| `#f5b75c` acento / card | **9,58:1** | AA 4,5 | bien |
| `#8fd6a8` ok / card | **10,02:1** | AA 4,5 | bien |
| `#e98b6a` coral / card | **6,79:1** | AA 4,5 | bien |
| `#4b3424` borde / card | **1,47:1** | 1.4.11 → 3:1 | **falla** |
| `#1f1712` zebra / card | **1,04:1** | — | invisible |
| `#120d0a` fondo / panel | **1,05:1** | — | invisible |

Conclusión accionable: **no hay que “subir el contraste del texto”** (ya cumple). Hay que **separar superficies y bordes**: elevar el borde de controles a ≥3:1 y hacer la zebra perceptible (≥1,10:1 con tinte cálido, no negro puro). El WIP mejora el texto pero mantiene `BORDER 1,49:1` y `FAINT 4,14:1` usado a 9,5 px **`[WIP ✗]`**.

**b) Tipografía y densidad.** 9–10 px en labels de sección, versión, PID, prioridad y hints; 11 px en valores. A 125 % esto son ~11–12 px físicos: legible con esfuerzo, no “delicioso”. La tabla usa 26 px de alto con 4 botones: el ojo no descansa y el mouse falla.

**c) Jerarquía plana.** “Subir”, “Liberar”, `↺` y “Fin” tienen el mismo peso; el título de sección y el dato compiten; el badge ADMIN y el botón “Activar admin” están al mismo nivel que la marca. No hay un punto focal por pantalla.

---

## 6. Principios de interacción

1. **Una verdad por dato.** El perfil se muestra una vez (barra). Los contadores viven en el STATUSBAR, no en el sidebar. Si un número aparece dos veces, uno de los dos está mal.
2. **Confirmación proporcional al daño.** Reversible → se hace y se ofrece “Deshacer”. Irreversible o de sistema → confirmación con nombre, alcance y consecuencia. El default del diálogo nunca es la acción destructiva.
3. **Toda acción responde en <100 ms.** El control entra en `pending` local al instante; el resultado llega después. Nunca se bloquea la ventana esperando HTTP.
4. **Ningún dead end.** Todo error tiene salida: reintentar, elevar, elegir un objetivo, abrir la carpeta, ver el registro. Un mensaje sin botón es un bug.
5. **Lo automático se declara.** Si Domador actúa solo, aparece en Actividad con motivo, alcance y origen (UI / regla / MCP). Se puede desactivar desde ahí.
6. **Números honestos.** Unidad, base y hora: “1,2 GB liberados en 7 procesos · hace 2 min”, “commit 12,3 de 18 GB (68 %)”, “300 de 502 (filtro activo)”.
7. **Tres niveles de texto, no seis.** Título, cuerpo, caption. Si hace falta un cuarto, se resuelve con peso o color, no con otro tamaño.
8. **El teclado es un camino completo.** Todo se puede hacer sin mouse, con foco visible y orden lógico.
9. **El color señala excepción, no decora.** Verde/ámbar/rojo sólo para estado; el latón es marca y foco, no “ok”.
10. **La calma es un feature.** Sin parpadeos, sin badges que rebotan, sin contadores animados. El movimiento explica un cambio de estado; si no, no existe.

---

## 7. Sistema visual

### 7.1 Tokens (propuesta final)

Base cálida carbón; latón para marca/foco; cobre para advertencia; verde y rojo sólo estado. Los ratios están medidos contra `CARD`.

| Token | Hex | Uso | Ratio vs CARD |
|---|---|---|---|
| `bg` | `#14100D` | Fondo de ventana | 1,12 |
| `panel` | `#1C1510` | Sidebar, topbar, tabla | 1,07 |
| `card` | `#241B14` | Tarjetas, filas hover | — |
| `sunken` | `#0F0B08` | Inputs, pozos, segmentos | 1,16 |
| `border` | `#4A3626` | Separadores decorativos | 1,49 |
| `border-strong` | `#8A6240` | **Límite de controles interactivos** | **3,14** |
| `text` | `#F4E7D3` | Texto principal | 13,86 |
| `muted` | `#B9A48F` | Texto secundario | 7,06 |
| `faint` | `#9A8570` | Sólo caption ≥11,5 px | **4,80** (subir desde `#8F7A67`, que da 4,14) |
| `brass` | `#D7A24A` | Marca, foco, acción primaria | 7,37 |
| `brass-hi` | `#ECBE70` | Hover de primaria | 9,80 |
| `copper` | `#B9633D` | Advertencia (no texto chico) | 3,96 |
| `copper-hi` | `#E28D62` | Texto de advertencia | 6,62 |
| `ok` | `#86C291` | Éxito, admin | 8,16 |
| `warn` | `#E8B05C` | Atención | 8,69 |
| `danger` | `#DB6A56` | Peligro (texto/ícono) | 5,00 |
| `danger-fill` | `#8E3A30` | Fondo de botón destructivo | 2,25 (borde `#C4645A` = 4,28) |
| `zebra` | `#31251C` | Fila alterna | **1,14** (hoy 1,04) |

**Reglas de aplicación:** borde decorativo puede ser `border`; **todo control interactivo** (botón, input, segmento, switch) usa `border-strong` o un fill con ≥3:1 contra su vecino. Prohibido texto de menos de 11,5 px; prohibido `faint` para información necesaria.

### 7.2 Tipografía

| Rol | Tamaño / alto | Peso | Uso |
|---|---|---|---|
| Display | 22 / 28 | 600 | Título de pantalla (“Procesos”) |
| H2 | 17 / 22 | 600 | Título de tarjeta |
| Body | 14 / 20 | 400 | Texto, celdas, labels de campo |
| Body strong | 14 / 20 | 600 | Nombre de proceso, valor |
| Caption | 11,5 / 16 | 500 | Eyebrows en mayúsculas, hints, unidades |
| Mono | 12 / 17 | 400 | PID, GB, rutas, GUID |

Una sola familia proporcional (la del sistema si se puede; si no, la actual) + mono para números. **Cifras tabulares** en tablas (alinear a la derecha, ancho fijo) para que no bailen al actualizar.

### 7.3 Espaciado, targets y filas

- Escala: 4 / 8 / 12 / 16 / 24 / 32.
- Padding de tarjeta: 16. Separación entre tarjetas: 12. Margen de contenido: 20.
- Altura de control: **30 px** (mínimo clickeable 30×30; en barras densas 28 con separación ≥4).
- Altura de fila de tabla: **32 px** (hoy 26). Con 500 procesos, 32 sigue mostrando ~18 filas en 900 px: densidad suficiente.
- Iconos: 16×16 dibujados (trazo 1,5), nunca emoji. Un icono solo puede existir si tiene tooltip y nombre accesible; si es una acción de fila, va con label.

### 7.4 Botones y estados

| Variante | Fill | Texto | Borde | Uso |
|---|---|---|---|---|
| Primary | `brass` | `ink` (#241B14) | `brass-hi` | Acción principal de la pantalla (1 por vista) |
| Secondary | `card` | `text` | `border-strong` | Acciones normales |
| Ghost | transparente | `muted` → `text` hover | ninguno | Acciones terciarias, toolbar |
| Danger | `danger-fill` | `#FFE6E1` | `#C4645A` | Sólo destructivo irreversible |

Estados: `hover` aclara 6 %; `active` oscurece 4 % y baja 1 px; `focus` anillo 2 px `brass` + 1 px `ink` por dentro; `disabled` 40 % de opacidad **y** cursor prohibido + tooltip con el motivo (“Necesitás admin”). Nunca deshabilitar sin explicar.

### 7.5 Tabla de procesos

- Zebra visible (`#2A2018`), hover `#2F241B`, seleccionada `#3A2B1E` + barra izquierda de 2 px `brass`.
- Columna Proceso: nombre 14/600 + ruta corta 11,5 `faint` truncada con tooltip; avatar de 20 px con inicial y color estable por hash del nombre (no decorativo: ayuda a reconocer familias).
- CPU y RAM con sparkline de 24 puntos (ya viene en `hist`), 48×14 px, color por umbral.
- Prioridad como chip semántico: “Normal”, “Bajo”, “Alto”, “Muy alto”, “Tiempo real” + punto de color; **nunca** el valor crudo.
- Acciones: **una** acción primaria contextual (“Terminar…”) + menú `⋯` con el resto. Se elimina la fila de 4 botones `[WIP ✗]`.
- Encabezado ordenable con flecha y `aria-sort`; el orden se persiste.
- Pie de tabla: “Mostrando 60 de 502 · Virtualizado” o “300 de 502 (límite de seguridad) · Ver todos”.

### 7.6 Movimiento

Duraciones 100–160 ms, `ease-out`; sólo para: apertura de drawer, cambio de tab, aparición de toast, transición de estado de botón. Sin rebotes, sin parallax. Respetar “reducir movimiento” (Windows) desactivando fades y el pulso del logo de bandeja.

---

## 8. Navegación y atajos

### 8.1 Estructura

- **Sidebar** (184 px; 64 px colapsada por debajo de 1240 px): Procesos, Salud, Reglas, Actividad, Ajustes. Icono + label; en compacto sólo icono con tooltip. Estado activo: fondo `#3A2B1E`, texto `brass`, barra izquierda 2 px. Si se cierra V2 con tres vistas, Reglas y Actividad viven como segmented control dentro de Procesos y Salud (§4.2) y la sidebar sólo muestra tres ítems.
- **Topbar** (64 px): logo 32, “Domador”, punto de estado del motor con texto (“Motor OK”), badge admin (clickeable → Ajustes > Administrador), CPU y RAM con tooltip (“Promedio de 1 s · 16 hilos”), versión. Sin botón “Activar admin” en el header: una sola puerta.
- **STATUSBAR** (30 px, siempre visible): estado del motor, antigüedad del snapshot, última acción con hora, botón “Ver actividad”, `×` para descartar el mensaje. Reemplaza el slot único de toast/busy/error.
- **Drawer derecho** (380 px): detalle del proceso seleccionado; se cierra con Esc o click afuera; no tapa la tabla (la desplaza).
- **Overlays**: onboarding, confirmaciones, selector de juego, editor de regla, motor caído, ayuda. Todos con `egui::Modal` (disponible en 0.31.1), foco atrapado, Esc = cancelar.

### 8.2 Atajos

| Atajo | Acción | Contexto |
|---|---|---|
| `Ctrl+F` o `/` | Foco en buscar | Global |
| `Esc` | Cerrar modal / limpiar búsqueda / cerrar drawer | Global |
| `F5` o `Ctrl+R` | Refrescar snapshot | Global |
| `Ctrl+1…5` | Procesos, Salud, Reglas, Actividad, Ajustes | Global |
| `Ctrl+,` | Ajustes | Global |
| `?` / `F1` | Ayuda de atajos | Global |
| `↑` `↓` | Mover selección de fila | Procesos |
| `Home` / `End` | Primera / última fila | Procesos |
| `Enter` | Abrir drawer del seleccionado | Procesos |
| `Supr` | Abrir “Terminar…” del seleccionado (nunca ejecuta directo) | Procesos |
| `Ctrl+Shift+R` | Restaurar prioridad del seleccionado | Procesos |
| `Ctrl+L` | Liberar RAM del seleccionado | Procesos |
| `Ctrl+N` | Nueva regla | Reglas |
| `Ctrl+K` | Paleta de comandos (P1) | Global |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Zoom 80–200 % / reset | Global |
| `Tab` / `Shift+Tab` | Foco siguiente/anterior | Global |
| `Enter` / `Space` | Activar control enfocado | Global |
| `Alt+1…4` | Aplicar perfil (con confirmación si aplica) | Global (P2) |

Reglas de teclado: el foco **nunca** arranca en una acción destructiva; `Enter` en un diálogo destructivo no confirma (hay que tabular al botón rojo); el foco atrapado no escapa del modal; al cerrar, vuelve al control que lo abrió.

Implementación: `ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::F))` para no robar teclas a inputs; filas de tabla necesitan `Sense::click()` + foco propio (`Response::request_focus`) y un `Id` estable por PID+ct.

---

## 9. Flujos

Cada flujo define: disparador, pasos, estados, copy, endpoints y criterio. Los endpoints son los que ya existen salvo que se indique “API nueva”.

### 9.1 Primera ejecución (onboarding de 3 pasos)

**Disparador:** `config.ui.onboarded` ausente o `false` (API nueva: campo en `UiCfg`). No se muestra si el motor ya tiene reglas o perfiles usados.

**Paso 1 — Qué es esto**

```
┌──────────────────────────────────────────────────────────┐
│  DOMADOR                                        Saltar × │
│                                                          │
│  Tu máquina, bajo control.                               │
│                                                          │
│  ✓ Ordena la CPU cuando algo se come todo                │
│  ✓ Libera memoria de lo que no estás usando              │
│  ✓ Te avisa si un proceso quedó colgado                  │
│                                                          │
│  No cierra nada solo. Todo lo que hace queda en          │
│  Actividad, y podés deshacerlo.                          │
│                                                          │
│                               [ Entendido → ]             │
└──────────────────────────────────────────────────────────┘
```

**Paso 2 — Permisos** (sin jerga, con matriz de 3 filas):

> “Sin admin podés regular tus propios programas. Con admin, también los del sistema y el arranque automático.”
> `[ Seguir sin admin ]` (primaria, neutra) · `[ Reiniciar como administrador… ]` (secundaria)
> Nota: “Podés cambiarlo cuando quieras en Ajustes → Administrador.”

**Paso 3 — Cómo arrancar**

- Tarjetas de perfil con radio (no chips): Equilibrado (recomendado, preseleccionado), IA (si detecta ≥1 patrón de `AI_PATTERNS` corriendo: “Detectamos Claude y Hermes”), Juego, Full Power (con aviso si está con batería).
- Checkbox “Arrancar con Windows” **desmarcado** por defecto, con subtítulo “Crea una tarea elevada; te va a pedir permiso una vez.”
- `[ Empezar ]` → aplica perfil, guarda `onboarded=true`, toast: “Listo. Domador está mirando 502 procesos y no va a cerrar nada sin que se lo pidas.”

**Estados:** `loading` si el primer snapshot tarda (skeleton + “Estamos leyendo el sistema…”); `offline` si no hay motor (no se muestra el wizard: se muestra el flujo de motor caído, §9.6).
**Salir:** “Saltar” marca `onboarded=true` y deja un acceso “Volver a ver la introducción” en Ajustes > Acerca de.
**Criterio:** 3 pantallas, ≤45 s, 0 términos técnicos sin explicar, no requiere admin, se puede saltear y reabrir.

### 9.2 Perfiles

**Disparador:** click en un segmento de la barra de perfiles o `Alt+1…4` (P2).

**Comportamiento por perfil:**

| Perfil | Qué hace de verdad | Qué tiene que decir la UI |
|---|---|---|
| Equilibrado | Vuelve al plan anterior y **deshace** las prioridades que aplicó el perfil | “Vuelve al plan anterior y deshace los cambios del perfil. Tus reglas quedan.” |
| Juego | Requiere un ejecutable objetivo; sube su prioridad, baja la del resto (ProBalance), cambia plan | “Elegí el juego. Mientras corra, Domador lo prioriza y baja el fondo.” |
| IA | Sube prioridad de los patrones de IA y los protege de ProBalance | “Prioriza Hermes, Claude, Codex… y los protege de ProBalance mientras dure el perfil.” |
| Full Power | Plan de máximo rendimiento + todo patrón de IA a High | “Sube todo al máximo y calienta/consume más. Sólo enchufado.” |

**Pasos:**

1. Click → el segmento entra en `pending` (“Aplicando…”, spinner de 12 px) y **todos** los segmentos se deshabilitan.
2. Si el perfil es **Juego** y no hay ventana en primer plano reconocible → se abre el **selector de juego** (§9.2.1). Nunca se envía sin `target`.
3. Si es **Full Power** y `power.ac_line == false` → confirmación: “Estás con batería. Full Power sube el plan de energía al máximo y puede consumir 20–30 % más. ¿Seguir?” `[ Mejor no ]` (foco) / `[ Activar igual ]`.
4. Si es **IA** → confirmación informativa con una sola casilla: “También va a proteger estas apps de ProBalance mientras el perfil esté activo.” (Y, si el motor deja de persistir `protect`, desaparece el problema F-12.)
5. Al terminar: toast “Full Power activo · plan Alto rendimiento” + línea bajo la barra: “Activo hace 4 min · 6 procesos ajustados · Ver qué cambió”.
6. Error: se mantiene el perfil anterior, el segmento vuelve, y el mensaje aparece **debajo de la barra**: “No pude cambiar el plan de energía: Windows lo bloqueó. Probá reiniciar como administrador.” + botón.

**9.2.1 Selector de juego**

- Lista de procesos con ventana visible y uso de GPU > 0 (ordenados por CPU), con icono, nombre y ruta.
- Campo “Buscar ejecutable” + botón “Elegir archivo…” (explorador de Windows).
- Recordatorio: “Domador lo va a reconocer por nombre. Si hay varias instancias, prioriza todas.”
- `[ Cancelar ]` / `[ Usar este ]` (deshabilitado hasta elegir).
- Si el usuario cierra, el perfil **no** cambia y no hay error: se vuelve al estado anterior (esto elimina el dead end actual).

**Criterio:** aplicar un perfil siempre termina en uno de tres estados visibles (activo, cancelado, error con salida); nunca queda un chip “medio aplicado”; Juego es resoluble en ≤2 pasos desde cualquier estado.

### 9.3 Administrador

**Estados:** `sin admin` (ámbar), `admin` (verde), `admin permanente` (verde + “arranca con Windows”).

**Puerta única:** el badge del header lleva a Ajustes > Administrador. No hay botón de elevación en el header ni duplicado en Ajustes `[WIP ✗]`.

**Flujo elevar:**

1. `[ Reiniciar como administrador… ]` → modal:
   > **Reiniciar con permisos de administrador**
   > Domador se cierra y vuelve a abrir con permisos elevados. El motor se detiene unos segundos; las reglas, perfiles y ajustes quedan guardados.
   > Windows te va a pedir confirmación.
   > `[ Cancelar ]` (foco) · `[ Reiniciar ]`
2. Acepta UAC → la app vuelve, badge verde, toast “Ahora con admin · podés regular procesos del sistema”.
3. Cancela UAC o falla → **el motor se vuelve a levantar sin admin automáticamente** y aparece: “No se otorgaron permisos. Seguís sin admin y el motor está corriendo de nuevo.” + `[ Reintentar ]`.
4. Si el motor no vuelve en 5 s → estado `offline` con `[ Iniciar motor ]`.

**Matriz de capacidades** (visible en la tarjeta, no en un tooltip):

| Acción | Sin admin | Con admin |
|---|---|---|
| Prioridad/afinidad/EcoQoS de tus programas | Sí | Sí |
| Procesos del sistema y de otros usuarios | No | Sí |
| Plan de energía | Limitado | Sí |
| Tarea de arranque | No | Sí |
| Watchdog/reglas | Sí | Sí |

**Permanente:**

- Sólo habilitado con admin. Modal con 3 bullets: crea la tarea `Domador` con `RunLevel Highest`, arranca con Windows, pide UAC una vez.
- Al confirmar: estado `pending` “Creando tarea…”; resultado verificado: “Tarea creada y verificada · arranca con Windows” + “Ver detalle” (stdout del script, colapsado) + `[ Quitar tarea ]` (P1).
- Error: “No pude crear la tarea: falta `install-admin.ps1` en la carpeta de instalación.” + `[ Abrir carpeta ]`.
- Nunca mostrar el stdout crudo de PowerShell como mensaje principal.

**Criterio:** una sola puerta; cancelar UAC nunca deja el motor apagado; “permanente” tiene confirmación, verificación y reversión.

### 9.4 Memoria y Salud

**Objetivo:** que Oscar entienda en <10 s si hay presión real y qué puede hacer sin riesgo.

**Cabecera de Salud:**

```
Salud
Memoria, GPU y lo que el Administrador de tareas no muestra.
┌──────────────────────────────────────────────────────────────┐
│ ● Presión normal        Disponible 11,8 GB · Commit 68 %     │
│ El sistema no está paginando. La caché está trabajando.      │
└──────────────────────────────────────────────────────────────┘
```

**Veredicto** (derivado, no un dato crudo):

| Estado | Regla | Copy |
|---|---|---|
| Normal | disponible ≥15 % y commit <80 % | “El sistema no está paginando. La caché está trabajando.” |
| Alta | disponible <15 % o commit ≥80 % | “Queda poca memoria disponible. Windows puede empezar a usar el disco.” |
| Crítica | disponible <8 % o commit ≥90 % o hard faults en aumento | “Estás al límite. Cerrá lo que no uses o liberá memoria de fondo.” |

**Tarjetas:** Memoria (disponible, commit con barra, pagefile, hard faults/s), Kernel y cache (paginado, no paginado, standby, handles), GPU (uso, VRAM, temp; si no hay GPU, tarjeta “Sin telemetría de GPU” con explicación, no un hueco), Historial 5 min (CPU y RAM, dos líneas, hover con valor), Top por memoria privada (top 10, con acciones por fila: “Liberar RAM”, “Regla”, “Detalle”).

**Alivio de memoria (trim-idle):**

1. Botón primario “Liberar memoria de fondo” + subtítulo visible: “Hasta 10 procesos sin ventana, con CPU baja y más de 192 MB. No cierra nada.”
2. `[ Ver cuáles ]` abre un popover con la lista (nombre + MB) — requiere que el snapshot exponga `visible` por proceso (**API nueva**) o un `dry_run` en `/api/trim-idle`.
3. Click → botón `pending` “Liberando…”; la tabla y el resto siguen operativos.
4. Resultado **persistente 30 s** en una tarjeta bajo el botón: “Liberaste 1,2 GB en 7 procesos · hace un momento · Ver detalle”. Si `freed == 0`: “No había nada para liberar. Windows ya estaba usando esa memoria.” (nunca un “Listo” seco).
5. Nota fija: “Esto no es un limpiador de RAM: Windows vuelve a llenar la caché cuando la necesita.”

**Acciones masivas:** “Liberar RAM de todo…” (`trim-all`) y “Restaurar todo” (`restore-all`) sólo en el menú `⋯`, con confirmación que nombre el alcance (“48 procesos grandes, incluidos los que tienen ventana”) y un checkbox “Incluir procesos con ventana” desmarcado.

**Criterio:** todo número tiene unidad y base; el alivio declara alcance antes y resultado después; ninguna acción de memoria cierra procesos; el veredicto cambia con los mismos umbrales que el motor.

### 9.5 Reglas

**Por qué es P0:** las reglas son el corazón del producto (persistencia, watchdog) y hoy no tienen superficie. Además, una regla con `max_instances` **cierra procesos** (F-09).

**Lista:**

```
Reglas                                    [+ Nueva regla]  [Aplicar todas]
┌──────────────────────────────────────────────────────────────────────┐
│ chrome.exe                                              ● Activa  ⋯  │
│ Todas las instancias, desde cualquier carpeta                        │
│ [Prioridad: Bajo] [EcoQoS: Sí] [Afinidad: 0-7]        Matchea 6 ahora │
├──────────────────────────────────────────────────────────────────────┤
│ C:\Juegos\juego.exe                                     ○ Pausada  ⋯  │
│ [Prioridad: Alta] [E/S: Alta]                        Matchea 0 ahora  │
└──────────────────────────────────────────────────────────────────────┘
```

- Orden: activas primero, luego por cantidad de matches.
- Chip de watchdog en **rojo** si `max_instances > 0`: “Cierra instancias extra”.
- `⋯`: Editar, Duplicar, Aplicar ahora, Eliminar.
- Empty state: “Todavía no hay reglas. Creá una desde un proceso: elegí uno en Procesos y usá «Crear regla».” + `[ Ir a Procesos ]`.

**Crear desde un proceso** (drawer → `[ Crear regla ]`):

- Patrón prellenado con la **ruta completa** si se pudo resolver; si no, el nombre, con aviso: “No pude leer la ruta. Con el nombre, la regla aplica a todas las instancias.”
- Campos: Prioridad (—“no cambiar”—, Bajo, Normal, Alto, Muy alto, Tiempo real), Afinidad (“0-3,8” con ayuda y botón “Todos”), EcoQoS (no cambiar / activar / desactivar), E/S, Memoria, Nota.
- Watchdog: “Relanzar si se cierra” (requiere ruta; si el patrón es sólo nombre, deshabilitado con motivo) y “Máximo de instancias”.
- **Preview en vivo**: “Matchea 3 procesos ahora: chrome.exe (1234), chrome.exe (5678), chrome.exe (9012)”. Se calcula contra el snapshot actual (o endpoint `preview`, API nueva).
- **Aviso de peligro** si `max_instances > 0`: “Esto cierra automáticamente las instancias extra (las más nuevas primero). No se puede deshacer.” + casilla “Entiendo que puede cerrar programas” obligatoria para guardar.
- **Aviso** si prioridad = Tiempo real: “Tiempo real puede colgar el sistema si el proceso se traba. Usalo sólo si sabés lo que hacés.” + confirmación extra.
- **Bloqueo** de patrones peligrosos: `*`, `*.exe`, rutas de sistema (`C:\Windows\*`) con watchdog → “Ese patrón puede cerrar procesos de Windows. Elegí algo más específico.”
- `[ Cancelar ]` / `[ Crear regla ]` (deshabilitado hasta que el patrón sea válido; el motivo se muestra inline, no en un toast).

**Acciones sobre una regla existente:**

- Toggle activa/pausada: instantáneo + toast “Regla pausada · Deshacer”.
- Aplicar ahora: “Aplicada a 5 procesos · 1 no se pudo (permisos)” con `[ Ver detalle ]`.
- Eliminar: modal “¿Eliminar la regla de chrome.exe? Deja de aplicarse a los 6 procesos que matchea. Lo ya aplicado no se revierte hasta que uses Restaurar.” `[ Cancelar ]` / `[ Eliminar ]` (danger).

**Criterio:** toda regla muestra a cuántos procesos afecta **antes** de guardar; ninguna regla que cierre procesos se guarda sin acknowledgement; crear desde un proceso toma ≤3 campos obligatorios (patrón + una acción).

### 9.6 Colgados, emergencias y motor caído

**9.6.1 Procesos colgados (`runaways`)**

- Banner en Procesos cuando hay ≥1: “2 procesos colgados · 12 min con CPU alta” + `[ Revisar ]`.
- Panel (tab Salud o drawer): filas con nombre, PID, minutos, CPU y acciones: `[ Terminar… ]`, `[ Bajar prioridad ]`, `[ Crear regla ]`, `[ Ignorar 1 h ]`.
- “Ignorar” es local a la sesión y no cambia el veredicto del motor (requiere `ignored` en la UI, no en el motor).
- Explicación: “Domador llama colgado a un proceso sin ventana que lleva 5 min por encima del 6 % de CPU. No lo cierra: te avisa.”

**9.6.2 Watchdog automático**

- En Ajustes > Avanzado: “Qué hacer con un colgado”: `Avisar` (default) / `Cerrar el proceso`.
- Cambiar a “Cerrar” abre confirmación con la lista de exclusiones (`DEFAULT_PROTECT` + procesos con ventana) y un checkbox “Entiendo que puede cerrar programas sin preguntar”. Sin ese checkbox, el radio vuelve a “Avisar”.
- Debajo, siempre visible: “Nunca toca procesos con ventana, del sistema ni Domador.”

**9.6.3 Terminar un proceso**

```
┌─ Terminar proceso ────────────────────────────────┐
│  ⚠  chrome.exe                                    │
│     PID 1234 · iniciado hoy 14:02                 │
│     C:\Program Files\Google\Chrome\chrome.exe     │
│                                                   │
│  Se cierra ahora. Perdés las pestañas y lo que    │
│  no esté guardado. Otras instancias de Chrome     │
│  pueden quedar inestables.                        │
│                                                   │
│  [ Cancelar ]                     [ Terminar ]    │
└───────────────────────────────────────────────────┘
```

- Datos: nombre, PID, hora de inicio, ruta (requiere `/api/meta` o campos en `ProcessRow`).
- `Cancelar` con foco inicial; `Terminar` en `danger`; `Esc` cancela; `Enter` **no** confirma.
- Al confirmar se envía `pid` **y** `ct` (**API nueva**); si el motor detecta que el proceso cambió: “Ese proceso ya no es el mismo (PID reutilizado). Actualicé la lista.” y no mata nada.
- Si el proceso está en `DEFAULT_PROTECT` o `pid ≤ 4`: no se abre el modal; se muestra “Domador no termina procesos del sistema (explorer.exe). Si algo anda mal, reiniciá Windows.” (override avanzado sólo en P2, con confirmación escribiendo el nombre).
- Resultado: toast “chrome.exe terminado · Deshacer no disponible” (honesto: no hay undo) + entrada en Actividad.

**9.6.4 Motor caído**

- Banner global: “No puedo hablar con el motor” + causa + `[ Reintentar ]` `[ Iniciar motor ]` `[ Ver registro ]`.
- Reintento automático con backoff 1/2/5/10 s; el resto de la UI queda navegable con datos viejos marcados como “stale”.
- “Iniciar motor” ejecuta el binario con el puerto descubierto (§12) y espera health; si falla, muestra la ruta del ejecutable y el log.
- Nunca instruir “ejecutá `domador run --no-open` en una terminal” como único camino (WIP actual `[WIP ✗]`): es un dead end para P3.

**Criterio:** ninguna emergencia termina en un texto sin acción; toda acción destructiva declara nombre, alcance y consecuencia; el sistema operativo no se puede romper desde la UI.

### 9.7 Actividad

**Objetivo:** responder “¿qué hizo Domador?” sin abrir el JSON.

- Timeline agrupada por día, con filtros: Todo / Mis acciones / Automático (ProBalance, reglas, watchdog, trim) / Errores.
- Cada entrada: hora, icono de origen (UI / regla / MCP / CLI), acción, objeto (“chrome.exe · PID 1234”), resultado (“prioridad Bajo aplicada”), y `[ Deshacer ]` cuando es reversible (prioridad, afinidad, EcoQoS, regla pausada).
- “Deshacer” la **última** acción desde el STATUSBAR: “Regla aplicada a 6 procesos · Deshacer”.
- Exportar CSV/JSON (P2). Retención: 400 entradas (lo que ya guarda el motor); la UI no inventa más.
- Empty state: “Todavía no pasó nada. Cuando Domador ajuste algo, lo vas a ver acá.”

**Criterio:** toda acción de la UI y todo cambio automático del motor tiene una entrada con hora y origen; deshacer cubre al menos prioridad, afinidad, EcoQoS y toggle de regla.

### 9.8 Cierre, bandeja y ciclo de vida

- Cerrar la ventana (`×`) → **minimiza a bandeja** (default), con aviso la primera vez: “Domador sigue mirando el sistema desde la bandeja. Para cerrarlo del todo, usá Salir.”
- `Ctrl+Q` / menú → “Salir de Domador”: modal con dos opciones: “Cerrar sólo la ventana” / “Apagar el motor también”. Recordar la elección con “No volver a preguntar”.
- Bandeja: Abrir, Abrir carpeta, Estado (“Motor OK · 502 procesos”), Reiniciar como administrador, Salir. El icono refleja estado (normal / sin admin / error) además de la animación.
- Si el motor se apaga desde otro lado (CLI `stop`, MCP), la UI pasa a `offline` con el banner, no queda mostrando datos viejos como si fueran actuales.

**Criterio:** un solo modelo mental de “cerrar” vs “apagar”; el estado del motor es visible desde la bandeja y desde la ventana.

---

## 10. Copy deck (español rioplatense)

**Reglas de estilo:** voseo (“Elegí”, “Tenés”, “Podés”, “Fijate”, “Andá”); sin “por favor”; sin “OK”; sin “exitosamente”; unidades con coma decimal y espacio fino (“1,2 GB”, “12,3 de 18 GB”); mayúsculas sólo en eyebrows y nombres propios; los botones dicen el verbo exacto, no “Aceptar”.

### 10.1 Navegación y estado

| Clave | Actual | Propuesto |
|---|---|---|
| `nav.processes` | Procesos | Procesos |
| `nav.health` | Salud | Salud |
| `nav.rules` | — | Reglas |
| `nav.activity` | — | Actividad |
| `nav.settings` | Ajustes | Ajustes |
| `status.motor_ok` | `127.0.0.1:8787` (texto fijo) | “Motor OK · hace 0,7 s” |
| `status.motor_down` | “motor no disponible: …” | “No puedo hablar con el motor” |
| `status.stale` | — | “Datos de hace 12 s” |
| `admin.badge` | ADMIN / SIN ADMIN | “Admin” / “Sin admin” (con tooltip “Qué cambia”) |
| `metric.cpu` | “CPU 62 %” | “CPU 62 % · 16 hilos” (tooltip) |
| `metric.ram` | “RAM 62 %” | “RAM 62 % · 10,1 de 16 GB” |

### 10.2 Perfiles

| Clave | Actual | Propuesto |
|---|---|---|
| `profile.label` | PERFIL | “Modo de rendimiento” (caption) |
| `profile.balanced` | Equilibrado | “Equilibrado” |
| `profile.gaming` | Juego | “Juego” |
| `profile.ai` | IA | “IA” |
| `profile.full` | Full Power | “Full Power” |
| `profile.pending` | “Activando Full Power” | “Aplicando…” (en el chip) |
| `profile.active_since` | “PERFIL ACTIVO / FULL POWER” | “Full Power activo hace 4 min · 6 procesos ajustados” |
| `profile.gaming.need_target` | “Elegí el ejecutable del juego” (error) | “Elegí a qué juego darle prioridad” (título del selector) |
| `profile.full.battery` | (sólo log) | “Estás con batería. Full Power consume y calienta más. ¿Activarlo igual?” |
| `profile.balanced.help` | — | “Vuelve al plan anterior y deshace los cambios del perfil. Tus reglas quedan.” |

### 10.3 Procesos

| Clave | Actual | Propuesto |
|---|---|---|
| `proc.search` | “buscar proceso…” | “Buscar por nombre o ruta…  (Ctrl+F)” |
| `proc.count` | “502 activos” | “502 procesos · 61 con ventana” |
| `proc.sort` | “ordenar por” + combo | “Ordenar: CPU ↓ · RAM · Nombre” (segmento) |
| `proc.action.up` | “Subir” | “Prioridad ▾” (menú: Bajo, Normal, Alto, Muy alto, Tiempo real) |
| `proc.action.trim` | “Liberar” | “Liberar RAM” |
| `proc.action.restore` | “↺” | “Restaurar prioridad” |
| `proc.action.kill` | “Fin” | “Terminar…” |
| `proc.empty` | (tabla vacía) | “Ningún proceso coincide con «chrome». Probá con el nombre del ejecutable o limpiá el filtro.” |
| `proc.truncated` | (silencioso) | “Mostrando 300 de 502 · Ver todos” |
| `proc.sparkline` | — | “CPU últimos 24 s” (tooltip) |

### 10.4 Salud y memoria

| Clave | Actual | Propuesto |
|---|---|---|
| `health.subtitle` | “Memoria real, GPU y procesos que no ves en el Task Manager.” | “Memoria, GPU y lo que el Administrador de tareas no muestra.” |
| `health.verdict.normal` | — | “Presión normal · el sistema no está paginando” |
| `health.verdict.high` | — | “Presión alta · queda poca memoria disponible” |
| `health.verdict.critical` | — | “Al límite · Windows está usando el disco como memoria” |
| `health.commit` | “Commit 12,3 / 18,0 GB” | “Comprometida 12,3 de 18,0 GB (68 %)” |
| `health.pool` | “Pool paginado / no paginado” | “Memoria del kernel (paginada / no paginada)” |
| `health.trim.cta` | “Aliviar memoria de fondo” | “Liberar memoria de fondo” |
| `health.trim.help` | “Sólo toca procesos sin ventana visible…” | “Hasta 10 procesos sin ventana, con CPU baja y más de 192 MB. No cierra nada.” |
| `health.trim.result` | “Listo: 120 MB liberados” | “Liberaste 1,2 GB en 7 procesos · hace un momento · Ver detalle” |
| `health.trim.zero` | “Listo” | “No había nada para liberar: Windows ya estaba usando esa memoria.” |
| `health.cache.note` | “Sólo toca procesos… No mata nada.” | “La caché y el standby no son RAM perdida: Windows los reutiliza.” |
| `health.top_private` | “TOP POR MEMORIA PRIVADA” | “Los que más memoria privada usan” |

### 10.5 Reglas

| Clave | Propuesto |
|---|---|
| `rules.title` | “Reglas” |
| `rules.subtitle` | “Lo que Domador aplica solo, cada vez que arranca un proceso.” |
| `rules.empty` | “Todavía no hay reglas. Creá una desde un proceso: elegí uno en Procesos y usá «Crear regla».” |
| `rules.new` | “Nueva regla” |
| `rules.match_preview` | “Matchea 3 procesos ahora” |
| `rules.pattern.help` | “Ruta completa = sólo ese archivo. Nombre = todas las instancias, desde cualquier carpeta. Podés usar * como comodín.” |
| `rules.watchdog.warn` | “Esto cierra automáticamente las instancias extra (las más nuevas primero). No se puede deshacer.” |
| `rules.watchdog.ack` | “Entiendo que puede cerrar programas” |
| `rules.realtime.warn` | “Tiempo real puede colgar el sistema si el proceso se traba. Usalo sólo si sabés lo que hacés.” |
| `rules.delete` | “¿Eliminar la regla de chrome.exe? Deja de aplicarse a los 6 procesos que matchea. Lo ya aplicado no se revierte hasta que uses Restaurar.” |
| `rules.apply.result` | “Aplicada a 5 procesos · 1 no se pudo (permisos)” |

### 10.6 Admin y acciones del sistema

| Clave | Actual | Propuesto |
|---|---|---|
| `admin.elevate` | “Activar admin” / “Reiniciar como administrador” | “Reiniciar como administrador…” (único) |
| `admin.elevate.body` | (sin confirmación) | “Domador se cierra y vuelve a abrir con permisos elevados. El motor se detiene unos segundos; reglas, perfiles y ajustes quedan guardados.” |
| `admin.elevate.cancelled` | (queda apagado) | “No se otorgaron permisos. Seguís sin admin y el motor está corriendo de nuevo.” |
| `admin.permanent` | “Hacer permanente” | “Arrancar siempre como admin” |
| `admin.permanent.help` | “Crea la tarea de inicio elevada (UAC una vez)” | “Crea una tarea de Windows que arranca Domador con permisos, sin preguntarte cada vez.” |
| `admin.permanent.ok` | (sin recibo) | “Tarea creada y verificada · arranca con Windows” + “Ver detalle” |
| `admin.required` | “Algunos procesos del sistema van a rechazar cambios.” | “Necesitás admin para tocar procesos del sistema. Tus programas funcionan igual.” |

### 10.7 Errores y vacíos (plantillas)

| Situación | Copy |
|---|---|
| Motor caído | “No puedo hablar con el motor. ¿Lo arranco?” `[ Reintentar ] [ Iniciar motor ] [ Ver registro ]` |
| Permiso denegado | “Windows rechazó el cambio. Necesitás admin para procesos del sistema.” `[ Reiniciar como admin ]` |
| Proceso desapareció | “Ese proceso ya no existe. Actualicé la lista.” |
| PID reutilizado | “Ese proceso ya no es el mismo (PID reutilizado). No toqué nada.” |
| Regla duplicada | “Ya hay una regla para ese patrón. ¿Querés editarla?” `[ Editar ]` |
| Patrón inválido | “El patrón no puede quedar vacío ni ser sólo *.” |
| Sin coincidencias | “Ningún proceso coincide con «xyz». Probá con el nombre del ejecutable o limpiá el filtro.” |
| Sin reglas | “Todavía no hay reglas. Creá una desde un proceso.” |
| Sin GPU | “No encontré telemetría de GPU en este equipo. El resto de Salud funciona igual.” |
| Acción en curso | “Aplicando… podés seguir usando el resto.” |
| Acción lenta | “Está tardando más de lo normal. Sigue en curso; te aviso cuando termine.” |

---

## 11. Accesibilidad

**Objetivo:** WCAG 2.1 AA en lo aplicable a escritorio, y operación completa por teclado.

| Requisito | Estado actual | Acción |
|---|---|---|
| 1.4.3 Contraste de texto 4,5:1 | Cumple (6,8–16,3) | Mantener; corregir `faint` a ≥4,5 en 11,5 px |
| 1.4.11 Contraste de componentes 3:1 | **Falla** (bordes 1,47) | `border-strong #8A6240` en todo control |
| 1.4.4 Redimensionar texto 200 % | No | Zoom 80–200 % con `Ctrl+=/-/0`, persistido |
| 1.4.12 Espaciado de texto | Parcial | Altura de línea ≥1,35; no truncar sin tooltip |
| 2.1.1 Teclado | **Falla** (v0.6.2 sin atajos; filas no focusables) | §8.2 completo |
| 2.4.7 Foco visible | **Falla** | Anillo 2 px `brass` + 1 px `ink`, nunca sólo cambio de color |
| 2.5.5 Tamaño de objetivo | **Falla** (22–26 px) | Mínimo 30×30, separación ≥4 |
| 4.1.2 Nombre, rol, valor | **Falla** (AccessKit off) | Habilitar feature `accesskit` en `eframe`; labels en icon-only |
| 1.4.1 Uso del color | Parcial | Estado = color + texto/ícono (badge admin ya lo hace; CPU no) |
| 2.3.1 Sin destellos | Cumple | Mantener animación de bandeja lenta y opt-out |
| 2.2.2 Pausar movimiento | No | Respetar “reducir movimiento” de Windows |

**Cómo se implementa en egui 0.31:**

- `eframe = { version = "0.31", features = ["glow","default_fonts","accesskit"] }` (hoy falta).
- Botones icon-only: `response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Restaurar prioridad"))` o `ui.add(Button::image(...)).on_hover_text(...)` + `labelled_by`.
- Foco: `ui.memory_mut(|m| m.request_focus(id))`, `response.has_focus()` para dibujar el anillo; orden con `ui.next_auto_id()` estable.
- Tabla: cada fila con `Id::new(("row", pid, ct))`, `Sense::click()`, y navegación con flechas leyendo el índice seleccionado (no depender del foco interno de `TableBuilder`).
- Zoom: `ctx.set_zoom_factor(z)`; guardar en `config.ui.zoom`.
- Anuncios: `ctx.announce(...)` no existe en 0.31; usar `accesskit` + `ui.label` del STATUSBAR como región viva (región con `role=status`) o `response.widget_info` con el texto del toast.

**Matriz de prueba:** 1280×720 @100 %, 1366×768 @125 %, 1920×1080 @150 %, 2560×1440 @200 %, alto contraste de Windows, Narrator activo, sólo teclado.

---

## 12. Robustez y performance de UI

### 12.1 Descubrimiento de puerto e instancia

Problema: `api::serve` puede terminar en 8788–8797; `ui.rs` y `tray.rs` asumen 8787.

Propuesta:

1. El motor escribe `%LOCALAPPDATA%\Domador\engine.json` al bindear: `{ "pid": 1234, "port": 8788, "version": "0.6.3", "admin": false, "started": 1731600000 }` (API nueva, lado motor).
2. La UI lee ese archivo, hace `GET /api/health` con timeout 250 ms y valida `app == "domador"`.
3. Si no hay archivo o falla: escanea 8787→8797 con health (5 × 250 ms en paralelo).
4. Sólo si no hay ninguna instancia sana, lanza `domador.exe run --no-open` y espera hasta 6 s.
5. Si el health devuelve `version` distinta en major/minor: banner “Motor desactualizado (0.6.2) · Reiniciar motor” con acción.
6. Instancia única del motor: mutex nombrado (`Global\DomadorEngine`), no un simple “puerto libre”. La UI puede tener N ventanas; el motor una.

### 12.2 Hilos y latencia

- Ninguna llamada HTTP ni `sleep` en `update()`: ya hay `poll_thread`; extenderlo con un canal de comandos (`std::sync::mpsc`) y un trabajador de acciones.
- `Shared` guarda `state`, `pending: HashMap<Tab, Pending>`, `errors`, `toasts` (con `at`).
- Toda acción: `pending` local inmediato (el botón pasa a “Liberando…”), request en worker, resultado en el siguiente frame.
- Timeout de acción: 15 s → estado “Está tardando más de lo normal” con `[ Cancelar ]` (cancelar = dejar de esperar, no abortar el server).
- Poll: 700 ms visible, 2 s con ventana minimizada; pausar si el motor está caído (backoff).
- No re-ordenar ni re-formatear en cada frame: memoizar por `(state_ptr, sort, search)`; `TableBuilder` virtualiza filas visibles.

### 12.3 Snapshot

- Hoy: ~330 KB por snapshot con 500 procesos (estimación) + 44 KB de log/historial cada 700 ms.
- Propuesta: `/api/state?light=1` sin `log`, `history` ni `hist`; `/api/log?since=<ts>` y `/api/history?since=<ts>` por separado. La tabla usa `hist` sólo para filas visibles (o un endpoint `/api/process/<pid>/hist`).
- Contrato: la UI tolera campos faltantes (nunca `unwrap`); si falta un campo nuevo, muestra “—” y no rompe.

### 12.4 Presupuesto

| Métrica | Objetivo |
|---|---|
| Frame en Procesos con 500 filas | ≤8 ms (vsync 60 Hz) |
| Click → feedback visual | ≤100 ms |
| Click → resultado (acción local) | ≤400 ms p95 |
| RAM de la UI | ≤120 MB |
| CPU de la UI en reposo | ≤1 % de un núcleo |
| Arranque en frío (motor ya corriendo) | ≤800 ms hasta primer frame útil |
| Errores no controlados (panic) | 0 en 30 min de soak |

---

## 13. Criterios de aceptación

**Globales (gates de release):**

- **AC-01** Ninguna acción destructiva (terminar, `trim-all`, `restore-all`, regla con watchdog, watchdog en `kill`, tarea admin, apagar motor) se ejecuta sin confirmación que **nombre el objeto, el alcance y la consecuencia**. Verificación: checklist de 8 acciones + grep de llamadas directas a `/api/kill`, `/api/trim-all`, `/api/restore-all`, `/api/admin-task`.
- **AC-02** Cero llamadas de red en `eframe::App::update`. Verificación: revisión de código + prueba con motor colgado (la UI sigue respondiendo).
- **AC-03** Con el motor en 8790 (`serve --port 8790`), la UI lo descubre y muestra datos en ≤2 s. Con dos instancias, no se levanta un tercer motor.
- **AC-04** Todo par texto/fondo ≥4,5:1 y todo borde de control ≥3:1. Verificación: script de contraste sobre los tokens.
- **AC-05** Tipografía mínima: caption 11,5, cuerpo 14, tabla 13; targets ≥30 px. Verificación: captura a 100/125/150 %.
- **AC-06** Ningún texto cortado sin tooltip en Procesos, Salud, Reglas, Ajustes a 1366×768 @125 % y 1920×1080 @150 %.
- **AC-07** Todo feedback desaparece solo (≤8 s) o se cierra; busy ≤100 ms tras el click; error con al menos una acción.
- **AC-08** Las seis pantallas/estados (loading, empty, offline, pending, error, stale) están implementados y son alcanzables en una prueba manual.
- **AC-09** Cero dead ends: todo mensaje de error tiene botón; “Juego” siempre se puede completar o cancelar.
- **AC-10** Onboarding ≤3 pasos, saltable, reabrible, sin requerir admin.
- **AC-11** Operación completa por teclado de: buscar → seleccionar → cambiar prioridad → confirmar; foco visible en todos los controles; AccessKit activo y Narrator anuncia nombres.
- **AC-12** Reglas: el preview de coincidencias coincide con el snapshot; no se puede guardar watchdog sin acknowledgement; el patrón `*` con watchdog se bloquea.
- **AC-13** Colgados: cada fila ofrece al menos “Terminar…” y “Bajar prioridad”; resolver uno no requiere ir a otra pantalla.
- **AC-14** Números honestos: el contador de filas coincide con lo mostrado o declara el límite; el resultado de memoria muestra GB y cantidad de procesos.
- **AC-15** Soak 30 min con 500+ procesos: CPU UI ≤1 %, sin crecimiento de memoria >10 %, sin panics.

**Por flujo:**

| Flujo | Criterio |
|---|---|
| Primera ejecución | ≤45 s, 0 jerga, saltable, no requiere admin |
| Perfiles | Estado final siempre visible; Juego resoluble; Full Power con batería confirma; Equilibrado explica qué deshace |
| Admin | Cancelar UAC deja motor vivo; “permanente” verifica y se puede quitar |
| Memoria | Alcance antes, resultado después, 0 procesos cerrados |
| Reglas | ≤3 campos obligatorios; preview antes de guardar; borrar explica alcance |
| Emergencias | Kill con nombre/ruta/hora; protegidos bloqueados; PID reutilizado detectado |
| Actividad | Toda acción (UI/auto/MCP) aparece con hora y origen; undo en reversibles |

---

## 14. Backlog priorizado

Esfuerzo: **S** ≤1 día, **M** 2–4 días, **L** ≥1 semana. “Depende” indica cambios de motor/API sin los cuales el ítem no se puede cerrar.

### P0 — sin esto no se puede llamar “producto”

| ID | Título | Problema | Cambio | Archivos | Criterio | Esf |
|---|---|---|---|---|---|---|
| `UX-P0-01` | Conexión robusta | F-24, F-25 | `engine.json` con puerto/versión; health check; escaneo 8787–8797; mutex de instancia; banner de versión | `api.rs`, `engine.rs`, `ui.rs`, `tray.rs` | AC-03 | M |
| `UX-P0-02` | Cero I/O en el hilo de UI | F-23 | Canal de comandos + worker; `pending` por acción; timeouts con “sigue en curso” | `ui.rs` | AC-02 | M |
| `UX-P0-03` | Ciclo de feedback | F-19, F-22, F-27 | Toast 8 s con cierre, cola de 3, busy con spinner y cancelación, error con acción, STATUSBAR de 3 niveles | `ui.rs` | AC-07, AC-08 | S/M |
| `UX-P0-04` | Terminar seguro | F-06, F-07, F-08, F-15 | Modal con ruta/hora; `ct` en el request; bloqueo de protegidos; `Enter` no confirma; confirmaciones para masivas | `ui.rs`, `api.rs`, `engine.rs`, `model.rs` | AC-01, AC-13 | M |
| `UX-P0-05` | Perfiles completos | F-01, F-11, F-12 | Segmento único; picker de Juego; confirm de batería; efectos de IA declarados y de sesión; “activo hace X · Ver qué cambió” | `ui.rs`, `engine.rs` | Flujo §9.2 | M |
| `UX-P0-06` | Acciones de proceso claras | F-16, F-18 | “Prioridad ▾”, “Liberar RAM”, “Restaurar prioridad”, “Terminar…” + menú `⋯`; labels localizados | `ui.rs` | AC-05, AC-09 | S |
| `UX-P0-07` | IA de secciones | F-01, F-05 | Reglas y Actividad accesibles (tabs en V2.1 o sub-secciones en V2); sidebar colapsable; quitar duplicados; STATUSBAR | `ui.rs` | Navegación §8.1, cualquiera de las dos variantes | M |
| `UX-P0-08` | Reglas v1 | F-02, F-09 | Lista, toggle, aplicar, eliminar con confirmación, empty state, chip de watchdog en rojo | `ui.rs`, `api.rs` (`preview`) | AC-12 | M |
| `UX-P0-09` | Tokens, contraste y tipografía | F-30, F-31, F-17 | Paleta con `border-strong` 3:1, zebra visible, escala mínima, switch real para ProBalanza, `danger` usado | `ui.rs` | AC-04, AC-05 | S/M |
| `UX-P0-10` | Layout responsive y contadores honestos | F-26, F-32 | Header flexible, sidebar 64/184, columnas adaptativas, “300 de 502”, virtualización | `ui.rs` | AC-06, AC-14 | M |
| `UX-P0-11` | Accesibilidad base | F-28, F-29 | Feature `accesskit`, foco visible, targets 30 px, atajos §8.2, nombres en icon-only, zoom | `Cargo.toml`, `ui.rs` | AC-11 | M |
| `UX-P0-12` | Onboarding 3 pasos | F-05 | Wizard con flag `ui.onboarded`, perfil recomendado, arranque con Windows opcional | `ui.rs`, `model.rs` | AC-10 | M |
| `UX-P0-13` | Salud con veredicto y alivio honesto | F-20, F-21, F-27 | Semáforo con umbrales, tarjetas con unidades y base, preview + resultado persistente, “no es un limpiador” | `ui.rs` | Flujo §9.4 | M |
| `UX-P0-14` | Admin sin trampas | F-13, F-14 | Puerta única, confirmación, recuperación si se cancela UAC, permanente con verificación y reversión | `ui.rs`, `engine.rs` | Flujo §9.3 | M |

### P1 — lo que hace que se sienta “rica”

| ID | Título | Problema | Cambio | Archivos | Criterio | Esf |
|---|---|---|---|---|---|---|
| `UX-P1-01` | Actividad y deshacer | F-03 | Timeline con filtros y origen; undo de prioridad/afinidad/EcoQoS/toggle; “Ver actividad” en STATUSBAR | `ui.rs`, `model.rs` | Flujo §9.7 | M |
| `UX-P1-02` | Drawer de proceso | F-02, F-16 | Detalle con ruta, usuario, hilos, handles; afinidad, EcoQoS, E/S, memoria; “Crear regla” | `ui.rs` (`/api/meta`) | Drawer §8.1 | M |
| `UX-P1-03` | Watchdog y colgados configurables | F-09, F-10 | Ajustes de umbral/acción con ack; acciones por colgado; protecciones visibles | `ui.rs`, `engine.rs` | Flujo §9.6 | M |
| `UX-P1-04` | Reglas v2 | F-02 | Editor completo, preview en vivo, dry-run, duplicar, validaciones de peligro | `ui.rs`, `api.rs` | AC-12 | L |
| `UX-P1-05` | Historial y sparklines | F-04 | Gráfico 5 min en Salud; sparkline por fila; hover con valor | `ui.rs` | 500 filas ≤8 ms | M |
| `UX-P1-06` | Paleta de comandos | — | `Ctrl+K`: ir a tab, buscar proceso, aplicar perfil, crear regla | `ui.rs` | ≤3 teclas a cualquier acción | M |
| `UX-P1-07` | Persistencia de UI | F-33 | Feature `persistence` + `config.ui` (tab, sort, densidad, zoom, ventana) | `Cargo.toml`, `model.rs`, `ui.rs` | Reinicio conserva estado | S |
| `UX-P1-08` | Bandeja integrada | F-34 | Cerrar = minimizar; Salir con dos opciones; estado en el menú; icono por estado | `ui.rs`, `tray.rs` | Flujo §9.8 | M |
| `UX-P1-09` | Gestión de inicio | Roadmap 4.4 | Listar startups, deshabilitar con confirmación y undo | `ui.rs`, `engine.rs`, `win.rs` | 0 cambios sin confirmación | L |
| `UX-P1-10` | Export/backup de config | — | Exportar reglas y ajustes a JSON; importar con preview | `ui.rs`, `config.rs` | Restauración verificada | S/M |

### P2 — pulido y alcance

| ID | Título | Cambio | Esf |
|---|---|---|---|
| `UX-P2-01` | Selección múltiple y lote | Checkboxes + barra de acciones (prioridad, EcoQoS, liberar, restaurar) con confirmación | L |
| `UX-P2-02` | Densidad compacta/cómoda | Toggle 28/36 px, persistido | S |
| `UX-P2-03` | Alto contraste / tema del sistema | Paleta alternativa AA+ y seguimiento de Windows | M |
| `UX-P2-04` | Notificaciones de Windows | Colgado, MemGuard, regla aplicada (opt-in, agrupadas, sin spam) | M |
| `UX-P2-05` | Exportar actividad | CSV/JSON del timeline | S |
| `UX-P2-06` | Antes/después en UI | Comparar métricas de una acción (tiempo, GB, procesos) | M |
| `UX-P2-07` | i18n | Extraer strings, inglés, formato locale | L |
| `UX-P2-08` | Tour y tips contextuales | Tips de 1 línea la primera vez por pantalla, descartables | S |
| `UX-P2-09` | Por núcleo y mapa de calor | `percpu` en Salud, heatmap de 16 hilos | M |

### Secuencia recomendada

1. **Semana 1 (P0 estructural):** 01, 02, 03, 09, 10. Sin esto, todo lo demás se siente roto.
2. **Semana 2 (P0 de confianza):** 04, 05, 06, 07, 08, 13, 14.
3. **Semana 3 (P0 de entrada):** 11, 12 + cierre de AC-01..AC-15.
4. **P1 en dos tandas:** 01/02/07 (memoria y contexto) y 03/04/05 (poder y control).
5. **P2** sólo con P0/P1 cerrados y soak limpio.

### Dependencias de motor/API (bloquean P0)

| Necesidad | Para | Tipo |
|---|---|---|
| `engine.json` con puerto/versión/pid | `UX-P0-01` | Motor |
| `ProcessRow`: `visible`, `started_at` (o usar `/api/meta`) | `UX-P0-04`, `UX-P0-13` | Modelo |
| `/api/kill` acepta `ct` y devuelve `PROCESS_CHANGED` | `UX-P0-04` | API |
| `/api/trim-idle?dry_run=1` (lista de objetivos) | `UX-P0-13` | API |
| `/api/rules?op=preview` (matches) | `UX-P0-08`, `UX-P1-04` | API |
| `profile.protect` de sesión, no persistente | `UX-P0-05` | Motor |
| `config.ui`: `onboarded`, `zoom`, `density`, `sort_dir`, `window` | `UX-P0-12`, `UX-P1-07` | Modelo |
| `/api/state?light=1`, `/api/log?since`, `/api/history?since` | `UX-P1-01`, `UX-P1-05` | API |

---

## 15. Validación

**Guion de prueba (10 tareas, 5 personas):**

1. Abrí Domador por primera vez y decime qué hace sin leer el README.
2. Poné el agente de IA en prioridad alta y dejalo fijo para la próxima.
3. Activá modo Juego para un juego que no está abierto.
4. Volvé a Equilibrado y explicá qué cambió.
5. Mirá Salud y decime si la memoria está bajo presión.
6. Liberá memoria de fondo y decime cuánto y a qué afectó.
7. Creá una regla para Chrome que baje la prioridad y explicá a cuántos aplica.
8. Encontrá el proceso que se colgó y resolvelo.
9. Terminá un proceso y explicá qué riesgo corriste.
10. Elevá permisos, cancelá el UAC y verificá que Domador sigue funcionando.

**Métricas:**

| Métrica | Objetivo |
|---|---|
| Tiempo a primera acción útil | ≤15 s |
| Tareas completadas sin ayuda | ≥9/10 |
| Acciones con feedback <150 ms | 100 % |
| Dead ends encontrados | 0 |
| Errores de concepto (“¿esto mata?”) | 0 |
| SUS | ≥80 |
| Soak 30 min: CPU/RAM/panics | ≤1 % / ≤120 MB / 0 |

**Pruebas técnicas:** script de contraste (tokens), capturas a 4 escalas, `serve --port 8790` + UI, UAC cancelado, kill de un proceso dummy con PID reciclado simulado, regla con `max_instances=1` sobre `notepad.exe` (verificar ack y bloqueo), motor matado a la fuerza (banner + recuperación).

---

## 16. Riesgos y supuestos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| Otro agente reescribe `ui.rs` en paralelo | Conflictos y auditoría que envejece | Este doc es el contrato; el backlog va por resultado, no por línea; merge por ítem |
| Cambios de motor (puerto, `ct`, previews) no entran en el mismo release | P0 bloqueados | Ship UI-only primero (03, 06, 09, 10, 12); degradar con “—” cuando falte campo |
| AccessKit aumenta tamaño/tiempo de build | Fricción de release | Medir binario; es un feature opt-in del `Cargo.toml` |
| Virtualización de tabla en egui_extras 0.31 | Jank con 500 filas | Medir; si no alcanza, paginar a 100 con “Ver más” |
| Power plans son globales y pueden fallar por política de grupo | Errores confusos | Copy específico + estado del plan visible + fallback |
| “Deliciosa” deriva en animación decorativa | Más jank, menos confianza | Regla §7.6: movimiento sólo para explicar estado |
| Kill sigue siendo irreversible | Pérdida de datos | Confirmación + protegidos + `ct`; nunca undo falso |

**Supuestos:** el usuario usa Windows 11 con escalado 100–150 %; el motor corre en la misma máquina; no hay multiusuario; el 90 % de las sesiones tienen entre 200 y 600 procesos.

---

## 17. Fuera de alcance

- Iconografía, instalador, updater, firma, benchmarking y packaging (pertenecen a `docs/ROADMAP-V2.md`).
- Nuevas capacidades de motor (MemGuard, grupos/familias, historial persistente) más allá de los habilitadores listados.
- Rediseño de la UI web de respaldo: se mantiene como está y se marca como “respaldo” en la app.
- MCP/CLI: sólo se agrega el **origen** de la acción en Actividad.

---

## Anexo A — Cobertura API ↔ UI

| Endpoint | En UI v0.6.2 | Propuesto |
|---|---|---|
| `GET /api/state` | Sí (700 ms) | Sí, con `?light=1` |
| `GET /api/health` | **No** | Sí (descubrimiento/versión) |
| `/api/priority` | Sólo `high` (“Subir”) | Menú completo |
| `/api/affinity` | No | Drawer + reglas |
| `/api/eco` | No | Drawer + reglas |
| `/api/io` | No | Drawer + reglas |
| `/api/memory-priority` | No | Drawer + reglas |
| `/api/restore` | Sí (`↺` sin label) | “Restaurar prioridad” + actividad |
| `/api/restore-all` | No | Menú `⋯` con confirmación |
| `/api/trim` | Sí (“Liberar”) | “Liberar RAM” + resultado |
| `/api/trim-all` | No | Menú `⋯` con confirmación |
| `/api/trim-idle` | Sí | Con preview y resultado |
| `/api/kill` | Sí, sin `ct` ni protegidos | Seguro (§9.6.3) |
| `/api/meta` | No | Drawer |
| `/api/rules` (5 ops) | **No** | Reglas v1/v2 |
| `/api/probalance` | Sólo `enabled` | Ajustes con umbrales |
| `/api/settings/trim` | No | Ajustes > Memoria |
| `/api/settings/foreground` | No | Ajustes > Rendimiento |
| `/api/power` | No | Ajustes > Energía |
| `/api/plan` | No | Ajustes > Energía |
| `/api/profile` | Sí, sin `target` | Con picker y confirmaciones |
| `/api/admin-task` | Sí, sin confirmación | Con confirmación y verificación |
| `/api/shutdown` | Sólo en relaunch | Salir del todo |
| `/api/runaway` | No | Ajustes > Avanzado |
| `/api/gpu-preference` | No | Drawer (P2) |

## Anexo B — Strings actuales que hay que matar

| Actual | Problema | Reemplazo |
|---|---|---|
| “Subir” | No dice qué sube ni a cuánto | “Prioridad ▾” |
| “Liberar” | Objeto implícito | “Liberar RAM” |
| “Fin” | Eufemismo de una acción irreversible | “Terminar…” |
| “↺” | Glifo sin nombre accesible | “Restaurar prioridad” |
| “Hacer permanente” | ¿Qué es permanente? | “Arrancar siempre como admin” |
| “Activar admin” | Verbo de estado, no de acción | “Reiniciar como administrador…” |
| “Regulados” | Jerga interna | “Ajustados por ProBalance” |
| “colgado(s)” | Bien, pero sin acción | “2 procesos colgados · Revisar” |
| “Listo” | No informa | Resultado con número y alcance |
| “Esperando al motor…” | Dead end | Banner offline con acciones |
| “HTTP 400: {json}” | Error crudo | Copy de §10.7 |
| “Commit / Pool / Handles” | Jerga | “Memoria comprometida / Kernel / Recursos” |

## Anexo C — Checklist por pantalla (Definition of Done UX)

**Procesos:** búsqueda con `/`, filtros visibles, orden persistido, 500 filas sin jank, acciones con label + menú, selección con teclado, drawer con meta, contador honesto, empty/error/loading, colgados accionables.

**Salud:** veredicto, 6 métricas con unidad y base, historial, GPU o explicación, top privada accionable, alivio con preview y resultado, copy anti-placebo.

**Reglas:** lista con matches, toggle con undo, editor con preview, watchdog con ack, borrado con alcance, empty state, validaciones inline.

**Actividad:** timeline con origen y hora, filtros, undo, export (P2), empty state.

**Ajustes:** ProBalanza con umbrales, memoria con umbral, energía con plan y trigger, admin con matriz y reversión, motor con ruta/puerto/versión y “Abrir carpeta”, avanzado con watchdog, acerca de con build y licencia.

**Global:** foco visible, targets 30 px, sin texto <11,5, sin cortes a 125/150 %, toast ≤8 s, motor caído recuperable, un solo modelo de cierre.

---

*Fin. Este documento no implementa ni publica nada; es la referencia para los PR de UI y los cambios de API listados en §14.*
