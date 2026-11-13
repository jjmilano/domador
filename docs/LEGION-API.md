# API Legion de Domador (WMI nativo)

Implementación en `src/legion.rs`, rutas en `src/api.rs`. No usa PowerShell ni
`WinRing0`: habla con las clases del driver Lenovo en `root\WMI` a través de COM
(`wmi` 0.18, sin features por defecto).

## Arquitectura

- **Un hilo worker** (`legion-wmi`) es dueño de la conexión COM a `ROOT\WMI` y
  serializa todas las lecturas y escrituras. COM no es `Send`, y el EC tolera mal
  dos dueños a la vez: el worker evita ambos problemas.
- **Cache de 2 s** para el estado completo. Un `GET /api/legion` repetido dentro
  de la ventana cuesta ~2 ms en vez de ~150 ms de llamadas WMI.
- **Conexión por demanda**, con reintento cada 30 s si falló (por ejemplo, si el
  servicio WMI estaba arrancando).
- Las lecturas usan la **ruta de instancia** (`SELECT * FROM clase` → `__PATH`),
  que es lo que espera el proveedor Lenovo; si no hay instancia, cae al nombre de
  clase.
- Modelo y BIOS salen de `ROOT\CIMV2` (`Win32_ComputerSystem.Model`,
  `Win32_BIOS.SMBIOSBIOSVersion`) y se leen una sola vez.

## Permisos

| Situación | GET | POST |
|---|---|---|
| Admin | datos reales | aplica y verifica |
| Sin admin | **200** con `admin:false`, `available:true` y `error` explicando que Windows bloquea el EC (`0x80041003`) | **403** `{"ok":false,"error":"esta operación requiere admin: ..."}` |
| Sin clases Lenovo / sin WMI | **200** con `available:false` y `error` | **503** `{"ok":false,"available":false,"error":...}` |

`available` significa “la máquina expone las clases Legion”. Sin admin no se
puede leer nada del EC en esta BIOS (el proveedor responde `0x80041003 Access
denied` incluso a `SELECT`), así que `available:true` + `error` es el estado
“limitado”: el hardware está, el permiso no.

## GET /api/legion

Siempre `200` (salvo Host/encabezado inválidos, que son 403 de la API general).
Contrato completo:

```json
{
  "ok": true,
  "available": true,
  "admin": false,
  "model": "82JQ",
  "bios": "GKCN65WW",
  "thermal": { "id": 0, "mode": "unknown", "label": "Desconocido" },
  "modes": [
    { "id": 1, "mode": "quiet", "label": "Silencioso" },
    { "id": 2, "mode": "balance", "label": "Equilibrado" },
    { "id": 3, "mode": "performance", "label": "Rendimiento" },
    { "id": 4, "mode": "custom", "label": "Personalizado" }
  ],
  "fans": [
    { "id": 1, "rpm": null, "max": 0 },
    { "id": 2, "rpm": null, "max": 0 }
  ],
  "full_speed": false,
  "temps": { "cpu": null, "gpu": null },
  "cpu": { "long": null, "short": null },
  "fan_table": null,
  "fan_table_stock": null,
  "competitors": [
    { "name": "LenovoVantage-(GenericMessagingAddin)", "pid": 54732 },
    { "name": "LenovoVantageService", "pid": 5380 },
    { "name": "VantageBridgePlugin", "pid": 9360 }
  ],
  "chief": false,
  "power_led": {
    "readable": false,
    "lighting_id": 4,
    "state": null,
    "brightness": null,
    "expected": null,
    "expected_label": null,
    "follows_mode": null
  },
  "error": "sin admin Windows bloquea el acceso WMI al EC (0x80041003 Access denied): ..."
}
```

Reglas de tipos:

- `temps.cpu/gpu`: °C enteros, o `null` si el firmware no contestó.
- `fans[].rpm`: RPM o `null`; `fans[].max`: máximo informado por el firmware o
  `0` si no lo informa (sin admin no hay máximo).
- `cpu.long/short`: `null` si no se pudo leer, o
  `{"value":45,"min":35,"max":75,"step":5}` con los límites que informa el
  firmware. `step` puede ser `0` si el firmware no lo publica.
- `fan_table` / `fan_table_stock`: arreglo de 10 niveles `1..10`, o `null` si no
  hay lectura / no hay backup todavía. **Nunca** se inventa una tabla por
  defecto.
- `thermal.id` es el valor crudo de `GetThermalMode` (1..4, 224/255 en otros
  modelos); si no se pudo leer, `0` + `mode:"unknown"`.
- `chief`: último estado persistido en `legion.json`.
- `power_led`: estado del LED del botón de encendido (ver sección propia).
  `readable:false` si el firmware no dejó leerlo; en ese caso `state`,
  `brightness`, `expected` y `follows_mode` van en `null`.
- `competitors`: procesos conocidos que hoy están corriendo (nombre sin `.exe`,
  PID). Es informativo: no se mata nada en un GET.
- `error`: sólo aparece si algo falló; es un resumen, no un volcado de cada
  llamada.

## POST /api/legion/thermal

```json
{"mode": 3}
{"mode": "performance"}
```

Acepta `1..4` o `quiet|balance|performance|custom` (sin distinguir mayúsculas).
Guarda el **modo previo** en `legion.json` (sólo la primera vez, para que
`restore` vuelva al original y no al último estado intermedio), aplica
`SetSmartFanMode(Data)` y relee para verificar.

Respuesta: `{"ok":true,"thermal":{...},"previous":{...},"verified":true,"power_led":{...},"backup":"..."}`.
`verified` es `null` si no se pudo releer el modo, `false` si el firmware informa
otro modo (y entonces `warning` lo dice). El color del LED no se escribe: lo
maneja el EC junto con el modo; ver la sección siguiente.

## LED del botón de encendido (`power_led`)

El color lo pone el firmware/EC cuando cambia el modo térmico; Domador **no
escribe** colores ni brillo. Sólo lee el estado y lo cruza con el modo:

- Fuente: `LENOVO_LIGHTING_METHOD.Get_Lighting_Current_Status(Lighting_ID=4)`,
  salidas `Current_State_Type` (u8) y `Current_Brightness_Level` (u8).
- Color esperado por modo (dato de firmware, no invención de Domador):

| Modo | `expected` | `expected_label` |
|---|---|---|
| 1 Quiet | `blue` | azul |
| 2 Balance | `white` | blanco |
| 3 Performance | `red` | rojo |
| 4 Custom | `null` | `null` (lo decide el firmware) |

`follows_mode` se calcula **al cambiar de modo** (`apply_thermal`):

- `null`: no se pudo leer el LED, o el salto involucra `custom` (no hay color
  fijo con el que comparar), o todavía no hubo un cambio de modo en esta sesión.
- `true`: el `Current_State_Type` cambió junto con el modo.
- `false` + `warning`: el estado del LED quedó igual al pasar entre dos modos con
  color esperado distinto; probablemente Vantage/LLT lo están manejando o el EC
  no sincroniza. Se reporta como **warning**, la escritura del modo no se
  revierte.

En el GET, `power_led` refleja la última lectura (cache 2 s) y la última
verificación de esta sesión. El campo nunca hace fallar el request: si no se
puede leer, `readable:false`.

## POST /api/legion/fans

```json
{"full_speed": true}
{"table": [1,2,3,4,5,6,7,8,9,10]}
{"full_speed": false, "table": [2,2,3,4,5,6,7,8,9,10]}
```

`table` exige exactamente 10 enteros `1..10` (niveles del firmware, no RPM ni
porcentaje). Antes de escribir:

1. Lee la tabla actual con `Fan_Get_Table(FanID=1, SensorID=1)` (fallback:
   `LENOVO_FAN_TABLE_DATA.FanTable_Data`).
2. Si no hay tabla stock en el backup, la guarda. **Si no pudo leer una tabla
   real, rechaza la escritura con 503**: no se escribe a ciegas.
3. Guarda también el estado previo de `full_speed`.
4. `Fan_Set_FullSpeed(0)` + `Fan_Set_Table(<64 bytes>)`, o
   `Fan_Set_FullSpeed(1)` si se pidió full speed.
5. Relee la tabla y devuelve `verified`.

Formato de los 64 bytes de `FanTable`:

| Offset | Tipo | Valor |
|---|---|---|
| 0 | u8 | `FSTM = 1` |
| 1 | u8 | `FSID = 0` |
| 2..6 | u32 LE | `FSTL = 10` (cantidad de puntos) |
| 6..26 | 10 × u16 LE | niveles 1..10 |
| 26..64 | — | relleno en cero |

Respuesta: `{"ok":true,"full_speed":false,"fan_table":[...],"table_written":true,"verified":true,"backup":"..."}`.
Si la relectura no coincide, `ok` sigue `true` (la escritura se hizo) pero
`verified:false` y `error` avisa.

## POST /api/legion/cpu

```json
{"long": 45}
{"short": 65}
{"long": 45, "short": 65}
```

Valida contra `CPU_Get_*Term_PowerLimit`: rango `min..max` y paso `step` del
firmware; fuera de eso responde **400** con el rango exacto. Guarda el valor
previo (primera vez) y aplica `CPU_Set_*Term_PowerLimit(value)`.

Respuesta: `{"ok":true,"changed":["long"],"cpu":{"long":{...},"short":{...}},"backup":"..."}`.

## POST /api/legion/chief

```json
{"enabled": true}
```

Cierra **sólo** procesos cuyo nombre (sin `.exe`, en minúsculas) empieza con:

`LenovoVantage`, `LegionArena`, `VantageBridgePlugin`, `LegionFanControl`,
`LenovoLegionToolkit`, `UXTU`, `RyzenController`, `Ryzen Controller`.

Nunca toca `Domador*` (propio PID y prefijo, además), `LenovoHotkeys`, juegos,
navegadores ni Afterburner. Reporta:

```json
{"ok":true,"enabled":true,
 "killed":[{"name":"LenovoVantageService","pid":5380}],
 "failed":[{"name":"UXTU","pid":1234,"error":"Access is denied."}],
 "skipped":[{"name":"...","pid":0,"reason":"protegido"}]}
```

Sin competidores devuelve `killed/failed/skipped` vacíos y `ok:true`.
`enabled:false` sólo apaga el flag (no relanza nada). El flag se persiste en
`legion.json` y se refleja en `GET /api/legion`.

## POST /api/legion/restore

```json
{}
```

Aplica, en orden y sólo lo que exista en el backup: `Fan_Set_FullSpeed(0)` +
tabla stock, `full_speed` previo, modo térmico previo, límites CPU previos.
Devuelve `{"ok":<sin fallos>,"restored":true,"applied":["fan_table","thermal"],"failed":[...],"thermal":{...},"fan_table":[...]}`.
Sin backup: `{"ok":true,"restored":false,"message":"no hay backup en disco; nada que restaurar"}`.
El backup no se borra: restaurar dos veces es idempotente. No restaura `chief`.

## Backup: `legion.json`

Ruta: junto a `config.json`, o sea `%LOCALAPPDATA%\Domador\legion.json`
(override: `DOMADOR_LEGION_CONFIG`).

```json
{
  "saved_at": 1763049600.0,
  "restored_at": null,
  "thermal_mode": 2,
  "fan_table_stock": [1,2,3,4,5,6,7,8,9,10],
  "full_speed": false,
  "cpu_long": 45,
  "cpu_short": 65,
  "chief": false
}
```

Se escribe con `tmp` + rename (nunca queda a medias). Los campos “previos” se
capturan **la primera vez** y no se pisan en escrituras posteriores: el backup
representa el estado de fábrica/original, no el último cambio.

## GET /api/legion/history?minutes=60[&limit=N]

Histórico del EC y de la GPU. Un hilo propio (`legion-telemetry`) muestrea cada
**2 s** sin tomar el mutex del motor: Legion se lee por el worker WMI (lectura
mínima: `GetCPUTemp`, `GetGPUTemp`, `GetFan1/2Speed`, `GetThermalMode`,
`CPU_Get_LongTerm_PowerLimit`) y la GPU por `nvidia-smi`
(`temperature.gpu,power.draw,clocks.current.graphics`, refresco cada 5 s).

- Ring buffer en memoria de **24 h** (≈43 264 muestras) y append a
  `%LOCALAPPDATA%\Domador\telemetry.csv`, que se relee al arrancar.
- El sampler arranca con el primer GET de Legion (la pestaña de la UI lo hace)
  y sigue hasta que el proceso termina.
- `minutes` acepta `0 < minutes ≤ 1440` (default `60`). `limit` acepta
  `1..=43264` y se aplica **dentro** de la ventana; si se manda `limit` sin
  `minutes`, la ventana es todo el buffer (24 h).
- Siempre `200` con WMI caído o sin admin. Parámetros inválidos → `400`
  `{"ok":false,"error":"..."}`.

```json
{
  "ok": true,
  "available": true,
  "sampling": {
    "interval_s": 2,
    "running": true,
    "last_at": 1794606432.3,
    "last_error": null,
    "buffer_samples": 2400,
    "buffer_capacity": 43264,
    "retention_hours": 24.0,
    "samples_total": 2400,
    "csv": "C:\\Users\\me\\AppData\\Local\\Domador\\telemetry.csv",
    "csv_error": null,
    "admin": false
  },
  "window": { "minutes": 60, "limit": null, "from": 1794602832.7, "to": 1794606432.7 },
  "samples": [
    {
      "ts": 1794606432.3,
      "time": "2026-11-13T21:47:12Z",
      "cpu_temp": 74.0,
      "gpu_temp": 66.0,
      "fan1_rpm": 2400,
      "fan2_rpm": 2350,
      "gpu_power_w": 40.7,
      "gpu_clock_mhz": 1560,
      "thermal_mode": 2,
      "thermal": "balance",
      "mode": "balance",
      "cpu_limit_w": 45.0
    }
  ],
  "summary": {
    "count": 1800,
    "first_ts": 1794602832.7, "last_ts": 1794606432.3,
    "first_time": "2026-11-13T20:47:12Z", "last_time": "2026-11-13T21:47:12Z",
    "span_s": 3600.0,
    "interval_s": { "avg": 2.0, "min": 1.8, "max": 2.4 },
    "cpu_temp_c": { "min": 61.0, "max": 94.0, "avg": 78.3, "last": 74.0, "count": 1800 },
    "gpu_temp_c": { "min": 58.0, "max": 81.0, "avg": 68.1, "last": 66.0, "count": 1800 },
    "fan1_rpm":   { "min": 0, "max": 4200, "avg": 2310, "last": 2400, "count": 1800 },
    "fan2_rpm":   { "min": 0, "max": 4150, "avg": 2280, "last": 2350, "count": 1800 },
    "gpu_power_w": { "min": 18.2, "max": 115.4, "avg": 52.8, "last": 40.7, "count": 1800 },
    "gpu_clock_mhz": { "min": 210, "max": 1905, "avg": 1180, "last": 1560, "count": 1800 },
    "hot": {
      "threshold_c": 90.0,
      "cpu_samples": 42, "gpu_samples": 0, "any_samples": 42,
      "cpu_s": 84.0, "gpu_s": 0.0, "any_s": 84.0,
      "cpu_min": 1.4, "gpu_min": 0.0, "any_min": 1.4
    },
    "thermal_modes": { "balance": 1500, "performance": 300 },
    "fan_curve": [
      { "fan_rpm_avg": 1100, "samples": 300, "cpu_avg_c": 62.4, "cpu_max_c": 70.0 }
    ],
    "fan_effect": {
      "pairs": 1799,
      "corr_rpm_vs_dtemp_per_min": -0.42,
      "avg_dtemp_low_rpm": 0.18,
      "avg_dtemp_high_rpm": -0.31,
      "rpm_split": 2200
    }
  }
}
```

Reglas:

- `samples[].*_temp/rpm/power/clock`: número real o `null`. Un `null` significa
  que esa fuente no contestó; nunca se rellena con 0. Se descartan centinelas
  del firmware (`0 °C`, `> 150 °C`).
- `cpu_temp`/`fan*_rpm`/`thermal_mode`/`cpu_limit_w` salen del EC y **requieren
  admin**; sin elevación quedan en `null` (igual que `GET /api/legion`).
- `gpu_temp` usa el EC y, si no hay dato, `nvidia-smi`; `gpu_power_w` y
  `gpu_clock_mhz` siempre vienen de `nvidia-smi` (no requieren admin).
- `summary.*` ignora `null`: `count` es cuántas muestras tenían ese dato.
- `hot` es tiempo sobre 90 °C: cada muestra caliente suma el intervalo hasta la
  siguiente (tope 60 s, para no inflar huecos con el equipo dormido).
- `fan_curve` son 6 bandas de RPM (fan 1; si falta, fan 2) con la temperatura
  media de CPU de cada banda: muestra la curva observada, no una causal.
- `fan_effect` correlaciona RPM con la variación de temperatura de CPU hasta la
  muestra siguiente (grados/min). Negativa sugiere que más RPM acompaña mejores
  bajadas; la inercia térmica confunde, así que es una pista, no una prueba.
  Con menos de 20 pares los campos van en `null`.
- `available` significa "las clases Legion existen" (igual que `/api/legion`):
  sin admin es `true` + `error`. `sampling.last_error` repite el motivo. Antes
  de la primera muestra es `null` (no se afirma que WMI falte).
- Si todavía no hay muestras: `samples: []`, `summary.count: 0` y `note`
  explicando si fue falta de admin/WMI o de tiempo.

## GET /api/legion/export.csv[?minutes=60][&limit=N]

Descarga el CSV del buffer (todo, o la misma ventana que `history`).

- `Content-Type: text/csv; charset=utf-8`
- `Content-Disposition: attachment; filename="domador-telemetry.csv"`
- Sin parámetros exporta las 24 h completas; con `minutes`/`limit` filtra igual
  que `history`. Parámetros inválidos → `400` JSON.
- Columnas:
  `ts,time_utc,cpu_temp_c,gpu_temp_c,fan1_rpm,fan2_rpm,gpu_power_w,gpu_clock_mhz,thermal_mode,cpu_limit_w`

## CSV en disco y rotación

Ruta: `%LOCALAPPDATA%\Domador\telemetry.csv` (override:
`DOMADOR_TELEMETRY_CSV`). Se escribe con `append` + `flush` por muestra y se
relee al arrancar (sólo las últimas 24 h, deduplicando la última muestra).
Rotación acotada: al pasar **8 MiB** el archivo activo pasa a
`telemetry-YYYYMMDD-HHMMSS.csv` y se conservan **2** archivos viejos. Máximo en
disco ≈ 24 MB; el archivo nunca crece sin límite. Un error de disco no rompe la
telemetría en memoria: se reporta en `sampling.csv_error`.

## GET/POST /api/legion/adaptive

Lazo automático que evalúa **las mismas muestras** de la telemetría (cada 2 s).
Nunca cambia el modo térmico y nunca escribe si falta admin o WMI.

```json
POST /api/legion/adaptive
{
  "enabled": true,
  "target_c": 82,
  "hot_c": 85,
  "critical_c": 92,
  "hysteresis_c": 3,
  "dwell_s": 10,
  "cooldown_s": 30,
  "action": "fans"
}
```

| Campo | Default | Rango / valores |
|---|---|---|
| `enabled` | `false` | `true|false` |
| `target_c` | `82` | `40..100`, menor que `hot_c` |
| `hot_c` | `85` | `45..105` |
| `critical_c` | `92` | `hot_c..110` |
| `hysteresis_c` | `3` | `1..20` |
| `dwell_s` | `10` | `2..600` |
| `cooldown_s` | `30` | `5..3600` |
| `action` | `"fans"` | `"fans"` o `"fans_cpu"` |

Todos los campos son opcionales en el POST (se conserva lo no enviado). El
body puede traer además `{"restore": true}` para apagar y devolver full speed y
el límite de CPU a su valor previo (requiere admin: `403` sin él).

Lógica:

1. Temperatura = máximo entre CPU y GPU disponibles. Sin lectura no se decide.
2. Caliente (`≥ hot_c`) sostenido `dwell_s` y con `cooldown_s` cumplido:
   - nivel 0 → `Fan_Set_FullSpeed(true)` (guarda el full speed previo).
   - nivel 1 + `action:"fans_cpu"` + `≥ critical_c` → baja el límite largo de
     CPU **un paso** del firmware (`step`, nunca por debajo de `min`) y guarda
     el valor previo.
3. Frío: sale en `max(target_c, hot_c - hysteresis_c)` sostenido `dwell_s`;
   restaura CPU primero y los fans después, un paso por `cooldown_s`.
4. Un fallo de escritura también consume cooldown (no martilla el EC) y queda
   en el log. Los `null` de WMI no se interpretan como frío ni como calor.

Respuesta (`GET` y `POST` devuelven lo mismo):

```json
{
  "ok": true,
  "enabled": true,
  "supported": true,
  "admin": true,
  "available": true,
  "error": null,
  "state": "hot",
  "level": 1,
  "target_c": 82.0, "hot_c": 85.0, "critical_c": 92.0,
  "hysteresis_c": 3.0, "exit_c": 82.0,
  "dwell_s": 10.0, "cooldown_s": 30.0,
  "action": "fans",
  "last_action": "fans_on · 86.4 °C · hot",
  "last_action_at": 1794606900.0,
  "last_action_time": "2026-11-13T21:55:00Z",
  "last_action_detail": {
    "ts": 1794606900.0, "time": "2026-11-13T21:55:00Z", "temp_c": 86.4,
    "trigger": "hot", "action": "fans_on", "level": 1, "ok": true,
    "detail": "full speed activado"
  },
  "last_temp_c": 86.4,
  "busy": false,
  "writes": 1,
  "failures": 0,
  "next_allowed_in_s": 12.3,
  "baseline": { "full_speed": false, "cpu_long": null },
  "config": { "enabled": true, "target_c": 82.0, "hot_c": 85.0,
              "critical_c": 92.0, "hysteresis_c": 3.0, "exit_c": 82.0,
              "dwell_s": 10.0, "cooldown_s": 30.0, "action": "fans" },
  "config_path": "C:\\Users\\me\\AppData\\Local\\Domador\\adaptive.json",
  "log": [ { "ts": 1794606900.0, "temp_c": 86.4, "trigger": "hot",
             "action": "fans_on", "level": 1, "ok": true,
             "detail": "full speed activado" } ]
}
```

- `supported` = `admin && available`: sin admin el lazo se puede configurar y
  armar, pero queda inerte y `error` lo dice.
- `state`: `off` (apagado), `normal`, `hot`, `critical` (según última lectura).
- `last_action` es texto para la UI; `last_action_detail` y `log` (últimas 100
  acciones) traen temperatura, trigger, nivel y resultado. `last_action_at` es
  epoch en segundos.
- `baseline` recuerda lo previo (`full_speed`, `cpu_long`); se persiste junto a
  la config en `adaptive.json` (override: `DOMADOR_ADAPTIVE_CONFIG`) para poder
  restaurar tras un reinicio. El `log` es de la sesión.
- `action:"fans"` no toca la CPU; `action:"fans_cpu"` sólo baja el cap cuando ya
  está en `critical_c`, y como máximo un paso por acción.

## Clases y métodos usados

| Clase | Métodos |
|---|---|
| `LENOVO_GAMEZONE_DATA` | `GetThermalMode`, `GetSmartFanMode`, `SetSmartFanMode(Data)`, `GetFan1Speed`, `GetFan2Speed`, `GetCPUTemp`, `GetGPUTemp` |
| `LENOVO_FAN_METHOD` | `Fan_Get_FullSpeed`, `Fan_Set_FullSpeed(Status)`, `Fan_Get_MaxSpeed(FanID)`, `Fan_GetCurrentFanSpeed(FanID)`, `Fan_Get_Table(FanID,SensorID)`, `Fan_Set_Table(FanTable)` |
| `LENOVO_CPU_METHOD` | `CPU_Get/Set_LongTerm_PowerLimit`, `CPU_Get/Set_ShortTerm_PowerLimit` |
| `LENOVO_FAN_TABLE_DATA` | lectura de `FanTable_Data` y `CurrentFanMaxSpeed` como fallback |
| `LENOVO_LIGHTING_METHOD` | `Get_Lighting_Current_Status(Lighting_ID=4)` para el LED del botón |

Tipos del MOF (importante: el proveedor rechaza `I4` donde espera `UInt32`):

| Método | Parámetro | Tipo |
|---|---|---|
| `SetSmartFanMode` | `Data` | `UInt32` (se manda `UI4`) |
| `CPU_Set_*Term_PowerLimit` | `value` | `UInt32` (`UI4`) |
| `Fan_Set_FullSpeed` | `Status` | `Boolean` (`Variant::Bool`) |
| `Fan_Set_Table` | `FanTable` | `UInt8Array` de 64 bytes (`Variant::Array` de `UI1`) |
| `Fan_Get_Table` | `FanID`, `SensorID` | `UInt8` (`UI1`) |
| `Get_Lighting_Current_Status` | `Lighting_ID` | `UInt8` (`UI1`) |

`Fan_Get_Table` puede devolver la tabla en dos formas y ambas se aceptan: 10
niveles pelados (`FanTableSize = 10`) o el buffer de 64 bytes con
`FSTM/FSID/FSTL`. Los valores leídos **no se normalizan**: si el stock trae un
`0`, el backup guarda `0` y `restore` lo devuelve como `0`.

Detalle de implementación: `IWbemClassObject::GetMethod` devuelve `NULL` para
métodos **sin parámetros de entrada** (por eso `GetThermalMode` parecía “no
existir”). El módulo usa `get_method_in_out`, que sí distingue “no existe” de
“existe sin argumentos”.

## Verificación hecha en esta máquina

- `cargo check`, `cargo build`, `cargo test --lib legion`: 7 tests en verde
  (modos, tabla de 64 bytes, parseo de tabla pelada/buffer, prefijos de
  competidores, validación de límites, colores del LED, backup ida y vuelta).
- Telemetría y adaptativo: `cargo test --lib telemetry adaptive` → 13 tests en
  verde (ISO UTC, CSV ida/vuelta, ring 24 h/capacidad, resumen de temps/fans/
  tiempo sobre 90 °C, curva de fans, correlación, rotación CSV con 2 archivos,
  ventana `minutes`/`limit`, validación de umbrales, dwell/cooldown/histéresis,
  escalera fans→CPU, desescalado y bloqueo sin admin/WMI).
- Servidor temporal en `:8891` / `:8892` (sin admin, config y CSV en un
  directorio de prueba, motor real de `:8787` intacto):
  - `GET /api/legion/history?minutes=5` → `200`, muestras cada ~2 s
    (`interval_s.avg = 1.98`), `gpu_temp 65–66 °C`, `gpu_power_w 40.5–41 W`,
    `gpu_clock_mhz 1560` reales vía `nvidia-smi`; CPU/fans/thermal en `null`
    con `available:true` y `sampling.last_error` explicando `0x80041003`.
  - `GET /api/legion/export.csv` → `200 text/csv; charset=utf-8` +
    `Content-Disposition: attachment`, encabezado y una línea por muestra.
  - `minutes=0` / `minutes=2000` / `limit=0` → `400` con mensaje; `POST` a
    `history`/`export.csv` → `405`; `POST /api/legion/adaptive` con
    `action:"bogus"` → `400`; `restore:true` sin admin → `403`.
  - Reinicio del motor con el mismo `DOMADOR_TELEMETRY_CSV`: el historial se
    recargó desde el CSV (39 muestras, sin duplicar la última) y
    `adaptive.json` conservó `enabled:true`, `target_c` y `action`.
  - `GET/POST /api/legion/adaptive` → `200` siempre (salvo validación), con
    `supported:false`, `writes:0` y `error` "sin admin: no toco el EC" al
    armarse sin elevación; apagarlo no deja un warning falso si no había nada
    aplicado.
- **No verificado** (falta elevación): temperaturas/RPM reales del EC, las
  escrituras del lazo (`Fan_Set_FullSpeed`, `CPU_Set_LongTerm_PowerLimit`) y
  la restauración del baseline. La máquina de estado está testeada con relojes
  simulados.


```bash
curl -s http://127.0.0.1:8787/api/legion | jq
curl -s "http://127.0.0.1:8787/api/legion/history?minutes=60" | jq '.summary'
curl -s "http://127.0.0.1:8787/api/legion/history?limit=300" | jq '.samples | length'
curl -s -D - -o telemetry.csv http://127.0.0.1:8787/api/legion/export.csv
curl -s http://127.0.0.1:8787/api/legion/adaptive | jq
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"enabled":true,"target_c":82,"hot_c":85,"critical_c":92}' \
  http://127.0.0.1:8787/api/legion/adaptive | jq
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"restore":true}' http://127.0.0.1:8787/api/legion/adaptive | jq
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"mode":3}' http://127.0.0.1:8787/api/legion/thermal
curl -s -X POST -H "X-Domador: 1" -H "Content-Type: application/json" \
  -d '{"full_speed":true}' http://127.0.0.1:8787/api/legion/fans
curl -s -X POST -H "X-Domador: 1" -d '{}' http://127.0.0.1:8787/api/legion/restore
```

## Riesgos y límites

1. **Un solo dueño del EC**: si Vantage/LLT/LFC están corriendo, pueden pisar el
   modo o la tabla. El flujo previsto es `chief` primero y después escribir.
   Domador no enforcea “no escribir si hay competidores”: lo reporta.
2. **Sin watchdog**: si Domador muere después de escribir una tabla o un modo,
   nada restaura el stock automáticamente. El backup queda en disco para
   `/api/legion/restore` manual o para una herramienta externa.
3. **Escrituras no verificadas en hardware real** en esta pasada (falta
   elevación). El formato de 64 bytes está testeado contra la estructura que usa
   el driver (`FSTM/FSID/FSTL` + 10 u16) pero no confirmado por relectura.
4. **Tabla de fans = niveles 1..10**, no RPM. El máximo real lo informa
   `Fan_Get_MaxSpeed`/`LENOVO_FAN_TABLE_DATA`; sin admin queda en `0`.
5. **`0x8004102F`** es el “fallo genérico” del proveedor cuando el EC rechaza
   una llamada (típico sin admin o con otra app controlando el EC). Se reporta
   tal cual en el `error`, sin reintentos automáticos.
6. **Timeout de 20 s por trabajo**: si el EC se cuelga, la request devuelve
   `504` y el worker puede quedar ocupado hasta que WMI responda; las siguientes
   encoladas también vencen. No hay cancelación de COM.
7. **`chief` requiere admin** por la regla general de escrituras, aunque matar
   procesos del mismo usuario no lo necesite. Un Domador no elevado no puede
   usar `chief` (403).
8. **Restaurar no revive apps cerradas** ni deshace cambios de otras
   herramientas: sólo reaplica los valores guardados.
9. `available:true` sin admin puede confundir a un consumidor ingenuo: hay que
   mirar `admin` y `error`, no sólo `available`.
10. **`power_led.state` no es un color documentado**: el firmware expone un
    `Current_State_Type` numérico sin tabla pública de equivalencias. Por eso
    `follows_mode` se infiere comparando el estado antes/después de un cambio de
    modo, no leyendo el color. Si el EC cambiara sólo el brillo, o si otro
    firmware usara `state` con otra semántica, puede dar un falso warning. Ante
    la duda (`custom`, LED ilegible, sin cambio previo) queda `null`.
11. No se escribe `Set_Lighting_Current_Status`: el color es del EC. Si una app
    externa lo fija, Domador lo reporta pero no lo corrige.
12. **El sampler arranca en el primer GET de Legion**, no con el motor: si nadie
    abre la pestaña ni consulta `/api/legion`, no hay histórico de ese tramo.
    Una vez arrancado sigue hasta que el proceso termina.
13. **El EC es de un solo dueño**: el lazo adaptativo escribe por el mismo
    worker WMI y puede competir con Vantage/LLT. `supported` sólo dice que hay
    admin y clases; no verifica que otra app esté pisando el modo.
14. **Baseline de full speed tomado del cache de `status()` (2 s)**: si otra
    herramienta cambió el full speed en esa ventana, la restauración puede
    devolver un valor previo levemente viejo. El de CPU se lee en el momento de
    bajar el cap.
15. **El lazo no cambia el modo térmico** a propósito: sólo full speed y límite
    largo de CPU. Si el firmware ignora `Fan_Set_FullSpeed` en modo Silencioso,
    el log mostrará el error del EC pero no se hace fallback a otra táctica.
16. **Historial completo = respuesta grande**: 24 h son ~43 000 muestras
    (~8 MB de JSON). La UI pide `minutes=60`; para bajar todo conviene
    `/api/legion/export.csv`.
17. **CSV en `%LOCALAPPDATA%`**: si el disco está lleno o el archivo está
    bloqueado, las muestras siguen en memoria y el error queda en
    `sampling.csv_error`; no se reintenta reescribir el archivo entero.
18. **Rotación por tamaño**: el archivo activo puede tener hasta ~2 días de
    datos (8 MiB) antes de rotar; el ring en memoria sigue siendo de 24 h, así
    que tras un reinicio se recargan sólo las últimas 24 h.

