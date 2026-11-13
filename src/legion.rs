//! Control nativo Lenovo Legion por WMI (`root\WMI`), sin PowerShell.
//!
//! Todo el acceso a WMI pasa por un único hilo worker: COM no es `Send` y el
//! EC de la notebook tolera mal dos dueños a la vez, así que las lecturas y
//! las escrituras se serializan en un solo hilo con su propia conexión. El
//! worker cachea el estado 2 s y guarda un backup en disco (`legion.json`)
//! antes de cada escritura, para poder volver al modo térmico y a la tabla de
//! fans de fábrica con `/api/legion/restore`.
//!
//! Reglas que este módulo hace cumplir:
//! - Las escrituras exigen admin (`403` claro si no).
//! - Si WMI no existe o no deja leer, la lectura devuelve `available:false`
//!   (o `available:true` + `error` si es sólo falta de permisos), nunca un
//!   `500` y nunca un panic.
//! - La tabla de fans no se escribe si no hay backup real de la tabla stock.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wmi::{IWbemClassWrapper, Variant, WMIConnection};

use crate::config;
use crate::nt;
use crate::win;

/// Namespace de las clases del driver Lenovo.
const NS_LEGION: &str = "ROOT\\WMI";
/// Namespace estándar, sólo para modelo y BIOS.
const NS_CIMV2: &str = "ROOT\\CIMV2";
const CLASS_GAMEZONE: &str = "LENOVO_GAMEZONE_DATA";
const CLASS_FAN: &str = "LENOVO_FAN_METHOD";
const CLASS_CPU: &str = "LENOVO_CPU_METHOD";
const CLASS_FAN_TABLE_DATA: &str = "LENOVO_FAN_TABLE_DATA";
/// Clase de iluminación; el LED del botón de encendido es el `Lighting_ID = 4`.
const CLASS_LIGHTING: &str = "LENOVO_LIGHTING_METHOD";
const POWER_LED_ID: u8 = 4;

/// Lecturas cacheadas: un GET cada 2 s no vuelve a golpear el EC.
const CACHE_TTL: Duration = Duration::from_secs(2);
/// Tope de espera por un trabajo del worker (el EC puede colgarse).
const JOB_TIMEOUT: Duration = Duration::from_secs(20);
/// Si la conexión WMI falló, se reintenta pasado este tiempo.
const CONN_RETRY: Duration = Duration::from_secs(30);
/// Cantidad de puntos de la curva de fans que entiende el firmware.
const FAN_TABLE_LEN: usize = 10;

/// Modos térmicos que acepta `SetSmartFanMode`.
const MODES: [(u8, &str, &str); 4] = [
    (1, "quiet", "Silencioso"),
    (2, "balance", "Equilibrado"),
    (3, "performance", "Rendimiento"),
    (4, "custom", "Personalizado"),
];

/// Procesos que pelean por el EC. Se comparan como prefijo, sin `.exe` y en
/// minúsculas. No incluye juegos, navegadores, Afterburner ni Lenovo Hotkeys.
const COMPETITOR_PREFIXES: &[&str] = &[
    "lenovovantage",
    "legionarena",
    "vantagebridgeplugin",
    "legionfancontrol",
    "lenovolegiontoolkit",
    "uxtu",
    "ryzencontroller",
    "ryzen controller",
];

/// Nunca se matan, aunque un prefijo los alcance por error.
const PROTECTED_PREFIXES: &[&str] = &["domador", "lenovohotkeys"];

// ------------------------------------------------------------------ errores

/// Error con su status HTTP. La API lo traduce sin inventar códigos.
#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

impl Error {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    fn denied() -> Self {
        Self::new(
            403,
            "esta operación requiere admin: cerrá Domador y volvé a abrirlo elevado",
        )
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(503, message)
    }

    fn wmi(message: impl Into<String>) -> Self {
        Self::new(502, message)
    }
}

// ------------------------------------------------------------- API pública

/// Estado completo, sólo lectura. Nunca falla: si el worker o WMI no están,
/// devuelve el mismo contrato con `available:false` y `error`.
pub fn status() -> Value {
    match send(JobReq::Status) {
        Ok(value) => value,
        Err(err) => json!({
            "ok": true,
            "available": false,
            "admin": win::is_admin(),
            "model": Value::Null,
            "bios": Value::Null,
            "thermal": mode_obj(0),
            "modes": modes_json(),
            "fans": [],
            "full_speed": false,
            "temps": {"cpu": Value::Null, "gpu": Value::Null},
            "cpu": {"long": Value::Null, "short": Value::Null},
            "fan_table": Value::Null,
            "fan_table_stock": Value::Null,
            "competitors": [],
            "chief": false,
            "power_led": power_led_json(0, None, None, None),
            "error": err.message,
        }),
    }
}

/// Lectura mínima para la telemetría (cada 2 s): temperaturas, RPM de los dos
/// fans, modo térmico y límite largo de CPU. No toca el cache de `status()` ni
/// enumera procesos, y nunca falla: sin WMI devuelve `null` en cada campo.
pub fn sample() -> Value {
    match send(JobReq::Sample) {
        Ok(value) => value,
        Err(err) => json!({
            "ok": true,
            "available": false,
            "admin": win::is_admin(),
            "temps": {"cpu": Value::Null, "gpu": Value::Null},
            "fans": [
                {"id": 1, "rpm": Value::Null},
                {"id": 2, "rpm": Value::Null},
            ],
            "thermal": {"id": 0},
            "cpu": {"long": Value::Null},
            "error": err.message,
        }),
    }
}

/// Nombre estable del modo térmico (`quiet|balance|performance|custom`) o
/// `unknown`. Lo usan la telemetría y el control adaptativo.
pub fn thermal_key(id: u8) -> String {
    MODES
        .iter()
        .find(|(mode, _, _)| *mode == id)
        .map(|(_, key, _)| (*key).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// POST /api/legion/thermal {mode: 1..4 | "quiet"|"balance"|"performance"|"custom"}
pub fn set_thermal(body: &Value) -> Result<Value, Error> {
    let id = parse_mode(body.get("mode"))
        .ok_or_else(|| Error::bad("mode inválido: usá 1..4 o quiet|balance|performance|custom"))?;
    send(JobReq::Thermal { id })
}

/// POST /api/legion/fans {full_speed?: bool, table?: [10 enteros 1..10]}
pub fn set_fans(body: &Value) -> Result<Value, Error> {
    let full_speed = match body.get("full_speed") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(Error::bad("full_speed debe ser booleano")),
    };
    let table = match body.get("table") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            if items.len() != FAN_TABLE_LEN {
                return Err(Error::bad(format!(
                    "table debe tener {FAN_TABLE_LEN} valores (niveles 1..10)"
                )));
            }
            let mut levels = Vec::with_capacity(FAN_TABLE_LEN);
            for (index, item) in items.iter().enumerate() {
                let value = item
                    .as_i64()
                    .ok_or_else(|| Error::bad(format!("table[{index}] no es un entero")))?;
                if !(1..=10).contains(&value) {
                    return Err(Error::bad(format!(
                        "table[{index}] = {value}: los niveles van de 1 a 10"
                    )));
                }
                levels.push(value as u16);
            }
            Some(levels)
        }
        Some(_) => return Err(Error::bad("table debe ser un arreglo de 10 enteros")),
    };
    if full_speed.is_none() && table.is_none() {
        return Err(Error::bad("falta full_speed o table"));
    }
    send(JobReq::Fans { full_speed, table })
}

/// POST /api/legion/cpu {long?: W, short?: W}
pub fn set_cpu(body: &Value) -> Result<Value, Error> {
    let long = optional_int(body, "long")?;
    let short = optional_int(body, "short")?;
    if long.is_none() && short.is_none() {
        return Err(Error::bad("falta long o short"));
    }
    for (key, value) in [("long", long), ("short", short)] {
        if let Some(value) = value {
            if !(1..=500).contains(&value) {
                return Err(Error::bad(format!(
                    "{key} debe ser un valor en watts entre 1 y 500"
                )));
            }
        }
    }
    send(JobReq::Cpu { long, short })
}

/// POST /api/legion/chief {enabled: bool}
pub fn set_chief(body: &Value) -> Result<Value, Error> {
    let enabled = body
        .get("enabled")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| Error::bad("falta enabled (true|false)"))?;
    send(JobReq::Chief { enabled })
}

/// POST /api/legion/restore {}
pub fn restore() -> Result<Value, Error> {
    send(JobReq::Restore)
}

fn optional_int(body: &Value, key: &str) -> Result<Option<i64>, Error> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| Error::bad(format!("{key} debe ser un entero en watts"))),
    }
}

fn parse_mode(value: Option<&Value>) -> Option<u8> {
    match value? {
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| MODES.iter().any(|(id, _, _)| id == value)),
        Value::String(text) => {
            let wanted = text.trim().to_ascii_lowercase();
            MODES
                .iter()
                .find(|(_, key, _)| *key == wanted)
                .map(|(id, _, _)| *id)
        }
        _ => None,
    }
}

// ------------------------------------------------------------- worker WMI

#[derive(Clone)]
enum JobReq {
    Status,
    Sample,
    Thermal {
        id: u8,
    },
    Fans {
        full_speed: Option<bool>,
        table: Option<Vec<u16>>,
    },
    Cpu {
        long: Option<i64>,
        short: Option<i64>,
    },
    Chief {
        enabled: bool,
    },
    Restore,
}

struct Job {
    req: JobReq,
    reply: Sender<Result<Value, Error>>,
}

static CHANNEL: OnceLock<Mutex<Option<Sender<Job>>>> = OnceLock::new();

fn channel() -> Result<Sender<Job>, Error> {
    let cell = CHANNEL.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap_or_else(|poison| poison.into_inner());
    if guard.is_none() {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("legion-wmi".to_string())
            .spawn(move || worker(rx))
            .map_err(|err| Error::unavailable(format!("no pude crear el hilo WMI: {err}")))?;
        *guard = Some(tx);
    }
    Ok(guard.as_ref().expect("canal recién creado").clone())
}

fn reset_channel() {
    if let Some(cell) = CHANNEL.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

fn send(req: JobReq) -> Result<Value, Error> {
    let mut last = String::from("sin respuesta");
    for _ in 0..2 {
        let tx = channel()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        match tx.send(Job {
            req: req.clone(),
            reply: reply_tx,
        }) {
            Ok(()) => match reply_rx.recv_timeout(JOB_TIMEOUT) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(Error::new(
                        504,
                        "WMI no respondió a tiempo; el EC puede estar ocupado por otra app",
                    ))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    last = "el worker WMI terminó inesperadamente".to_string();
                    reset_channel();
                }
            },
            Err(_) => {
                last = "el worker WMI ya no acepta trabajos".to_string();
                reset_channel();
            }
        }
    }
    Err(Error::unavailable(format!("WMI no disponible: {last}")))
}

fn worker(rx: Receiver<Job>) {
    let mut ctx = Ctx::new();
    for job in rx {
        let result = match job.req {
            JobReq::Status => Ok(ctx.status()),
            JobReq::Sample => Ok(ctx.sample()),
            JobReq::Thermal { id } => ctx.apply_thermal(id),
            JobReq::Fans { full_speed, table } => ctx.apply_fans(full_speed, table),
            JobReq::Cpu { long, short } => ctx.apply_cpu(long, short),
            JobReq::Chief { enabled } => ctx.apply_chief(enabled),
            JobReq::Restore => ctx.apply_restore(),
        };
        let _ = job.reply.send(result);
    }
}

// ----------------------------------------------------------------- contexto

struct Ctx {
    conn: Option<WMIConnection>,
    /// Error de conexión + cuándo se intentó, para reintentar sin martillar.
    conn_error: Option<(String, Instant)>,
    cim: Option<WMIConnection>,
    paths: HashMap<String, String>,
    cache: Option<(Instant, Value)>,
    snapshot: nt::ProcessSnapshot,
    model: Option<String>,
    bios: Option<String>,
    stock: Option<Vec<u16>>,
    chief: bool,
    /// Resultado de la última verificación LED↔modo; `None` = sin datos.
    led_follows: Option<bool>,
    led_warning: Option<String>,
}

impl Ctx {
    fn new() -> Self {
        let backup = load_backup();
        Self {
            conn: None,
            conn_error: None,
            cim: None,
            paths: HashMap::new(),
            cache: None,
            snapshot: nt::ProcessSnapshot::new(),
            model: None,
            bios: None,
            stock: backup
                .as_ref()
                .and_then(|backup| backup.fan_table_stock.clone()),
            chief: backup.and_then(|backup| backup.chief).unwrap_or(false),
            led_follows: None,
            led_warning: None,
        }
    }

    fn conn(&mut self) -> Result<&WMIConnection, String> {
        if self.conn.is_none() {
            if let Some((message, at)) = &self.conn_error {
                if at.elapsed() < CONN_RETRY {
                    return Err(message.clone());
                }
            }
            match WMIConnection::with_namespace_path(NS_LEGION) {
                Ok(conn) => {
                    self.conn = Some(conn);
                    self.conn_error = None;
                }
                Err(err) => {
                    let message = format!("WMI {NS_LEGION}: {err}");
                    self.conn_error = Some((message.clone(), Instant::now()));
                    return Err(message);
                }
            }
        }
        Ok(self.conn.as_ref().expect("conexión recién abierta"))
    }

    /// `__Path` de la instancia de la clase. Las clases Lenovo son dinámicas:
    /// el proveedor espera la ruta de instancia, no el nombre de la clase.
    fn instance_path(&mut self, class: &str) -> Result<String, String> {
        if let Some(path) = self.paths.get(class) {
            return Ok(path.clone());
        }
        let path = {
            let conn = self.conn()?;
            let query = format!("SELECT * FROM {class}");
            let mut enumerator = conn.exec_query(query).map_err(|err| err.to_string())?;
            match enumerator.next() {
                Some(Ok(object)) => object.path().ok(),
                Some(Err(err)) => return Err(err.to_string()),
                None => None,
            }
            .unwrap_or_else(|| class.to_string())
        };
        self.paths.insert(class.to_string(), path.clone());
        Ok(path)
    }

    fn call(
        &mut self,
        class: &str,
        method: &str,
        params: &[(&str, Variant)],
    ) -> Result<Option<IWbemClassWrapper>, String> {
        let path = self.instance_path(class)?;
        let conn = self.conn()?;
        let class_object = conn
            .get_object(class)
            .map_err(|err| format!("{class}: {err}"))?;
        // `get_method` devuelve `None` para métodos sin parámetros de entrada,
        // así que la firma in/out es la única forma de distinguir "no existe"
        // de "existe y no lleva argumentos".
        let (input_signature, _output) = class_object
            .get_method_in_out(method)
            .map_err(|err| format!("{class}.{method}: {err}"))?;
        let instance = match input_signature {
            Some(signature) => {
                let instance = signature
                    .spawn_instance()
                    .map_err(|err| format!("{class}.{method}: {err}"))?;
                for (name, value) in params {
                    instance
                        .put_property(*name, value.clone())
                        .map_err(|err| format!("{class}.{method}.{name}: {err}"))?;
                }
                Some(instance)
            }
            None => {
                if !params.is_empty() {
                    return Err(format!("{class}.{method}: el método no acepta parámetros"));
                }
                None
            }
        };
        let output = conn
            .exec_method(&path, method, instance.as_ref())
            .map_err(|err| format!("{class}.{method}: {err}"))?;
        Ok(output)
    }

    fn call_prop(
        &mut self,
        class: &str,
        method: &str,
        params: &[(&str, Variant)],
        property: &str,
    ) -> Result<Variant, String> {
        let output = self
            .call(class, method, params)?
            .ok_or_else(|| format!("{class}.{method}: sin salida"))?;
        output
            .get_property(property)
            .map_err(|err| format!("{class}.{method}.{property}: {err}"))
    }

    fn call_int(
        &mut self,
        class: &str,
        method: &str,
        params: &[(&str, Variant)],
        property: &str,
    ) -> Result<i64, String> {
        let value = self.call_prop(class, method, params, property)?;
        as_i64(&value).ok_or_else(|| format!("{class}.{method}.{property}: valor no numérico"))
    }

    // --------------------------------------------------------- lecturas

    fn status(&mut self) -> Value {
        if let Some((at, value)) = &self.cache {
            if at.elapsed() < CACHE_TTL {
                return value.clone();
            }
        }
        let value = self.build_status();
        self.cache = Some((Instant::now(), value.clone()));
        value
    }

    /// Lectura mínima y sin cache para telemetría/control adaptativo. Nunca
    /// falla: agrega `error` y deja en `null` lo que no pudo leer.
    fn sample(&mut self) -> Value {
        let admin = win::is_admin();
        let mut notes: Vec<String> = Vec::new();
        let mut available = false;
        match self.instance_path(CLASS_GAMEZONE) {
            Ok(_) => available = true,
            Err(err) => {
                if is_denied(&err) {
                    available = true;
                }
                notes.push(err);
            }
        }

        let cpu_temp = self
            .call_int(CLASS_GAMEZONE, "GetCPUTemp", &[], "Data")
            .map_err(|err| notes.push(err))
            .ok();
        let gpu_temp = self
            .call_int(CLASS_GAMEZONE, "GetGPUTemp", &[], "Data")
            .map_err(|err| notes.push(err))
            .ok();
        let mut fans = Vec::with_capacity(2);
        for fan_id in [1u8, 2u8] {
            let rpm = self
                .read_fan_rpm(fan_id)
                .map_err(|err| notes.push(err))
                .ok();
            fans.push(json!({"id": fan_id, "rpm": rpm}));
        }
        let thermal_id = self.read_thermal().map_err(|err| notes.push(err)).ok();
        let cpu_long = self
            .read_cpu_limit(true)
            .map_err(|err| notes.push(err))
            .ok();

        let denied = notes.iter().any(|note| is_denied(note));
        let error = if denied {
            Some(
                "sin admin Windows bloquea el acceso WMI al EC (0x80041003 Access denied): \
                 modos, fans, temperaturas y límites quedan sin dato hasta elevar Domador"
                    .to_string(),
            )
        } else {
            dedup_notes(notes)
        };

        let mut payload = json!({
            "ok": true,
            "ts": now_epoch(),
            "available": available,
            "admin": admin,
            "temps": {"cpu": cpu_temp, "gpu": gpu_temp},
            "fans": fans,
            "thermal": {"id": thermal_id.unwrap_or(0)},
            "cpu": {"long": cpu_long},
        });
        if let Some(error) = error {
            if let Some(object) = payload.as_object_mut() {
                object.insert("error".to_string(), Value::String(error));
            }
        }
        payload
    }

    fn build_status(&mut self) -> Value {
        let admin = win::is_admin();
        let mut notes: Vec<String> = Vec::new();
        let mut available = false;

        match self.instance_path(CLASS_GAMEZONE) {
            Ok(_) => available = true,
            Err(err) => {
                // Sin admin, Windows contesta "Access to a CIM resource...".
                // La clase existe: no es que falte WMI, es que no nos deja.
                if is_denied(&err) {
                    available = true;
                }
                notes.push(err);
            }
        }

        let thermal_id = match self.read_thermal() {
            Ok(id) => id,
            Err(err) => {
                notes.push(err);
                0
            }
        };

        let mut fans = Vec::new();
        for fan_id in [1u8, 2u8] {
            let max = self.fan_max(fan_id).unwrap_or(0);
            match self.read_fan_rpm(fan_id) {
                Ok(rpm) => fans.push(json!({"id": fan_id, "rpm": rpm, "max": max})),
                Err(err) => {
                    notes.push(err);
                    fans.push(json!({"id": fan_id, "rpm": Value::Null, "max": max}));
                }
            }
        }

        let full_speed = self.read_full_speed().unwrap_or(false);

        let cpu_temp = self
            .call_int(CLASS_GAMEZONE, "GetCPUTemp", &[], "Data")
            .map_err(|err| notes.push(err))
            .ok();
        let gpu_temp = self
            .call_int(CLASS_GAMEZONE, "GetGPUTemp", &[], "Data")
            .map_err(|err| notes.push(err))
            .ok();

        let cpu_long = self
            .read_cpu_limit(true)
            .map_err(|err| notes.push(err))
            .ok();
        let cpu_short = self
            .read_cpu_limit(false)
            .map_err(|err| notes.push(err))
            .ok();

        let fan_table = match self.read_fan_table() {
            Ok(table) => {
                if self.stock.is_none() {
                    self.stock = Some(table.clone());
                }
                Some(table)
            }
            Err(err) => {
                notes.push(err);
                None
            }
        };

        let fan_table_json = fan_table
            .as_ref()
            .map(|table| json!(table))
            .unwrap_or(Value::Null);
        let stock_json = self
            .stock
            .as_ref()
            .map(|table| json!(table))
            .unwrap_or(Value::Null);

        let competitors = self.competitors();

        let led = self.read_power_led().ok();
        let (model, bios) = self.identity();
        let denied = notes.iter().any(|note| is_denied(note));
        let error = if denied {
            Some(
                "sin admin Windows bloquea el acceso WMI al EC (0x80041003 Access denied): \
                 modo térmico, fans, temperaturas y límites quedan sin dato hasta elevar Domador"
                    .to_string(),
            )
        } else {
            dedup_notes(notes)
        };

        let mut payload = json!({
            "ok": true,
            "available": available,
            "admin": admin,
            "model": model,
            "bios": bios,
            "thermal": mode_obj(thermal_id),
            "modes": modes_json(),
            "fans": fans,
            "full_speed": full_speed,
            "temps": {"cpu": cpu_temp, "gpu": gpu_temp},
            "cpu": {"long": cpu_long, "short": cpu_short},
            "fan_table": fan_table_json,
            "fan_table_stock": stock_json,
            "competitors": competitors,
            "chief": self.chief,
            "power_led": power_led_json(
                thermal_id,
                led.as_ref(),
                self.led_follows,
                self.led_warning.as_deref(),
            ),
        });
        if let Some(error) = error {
            if let Some(object) = payload.as_object_mut() {
                object.insert("error".to_string(), Value::String(error));
            }
        }
        payload
    }

    fn read_thermal(&mut self) -> Result<u8, String> {
        let mut first = None;
        for method in ["GetThermalMode", "GetSmartFanMode"] {
            match self.call_int(CLASS_GAMEZONE, method, &[], "Data") {
                Ok(value) => return Ok(normalize_mode(value)),
                Err(err) => {
                    if first.is_none() {
                        first = Some(err);
                    }
                }
            }
        }
        Err(first.unwrap_or_else(|| "modo térmico desconocido".to_string()))
    }

    fn read_fan_rpm(&mut self, fan_id: u8) -> Result<i64, String> {
        let method = if fan_id == 1 {
            "GetFan1Speed"
        } else {
            "GetFan2Speed"
        };
        let first = match self.call_int(CLASS_GAMEZONE, method, &[], "Data") {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };
        let params = [("FanID", Variant::UI1(fan_id))];
        self.call_int(
            CLASS_FAN,
            "Fan_GetCurrentFanSpeed",
            &params,
            "CurrentFanSpeed",
        )
        .map_err(|err| format!("{first}; {err}"))
    }

    fn fan_max(&mut self, fan_id: u8) -> Result<i64, String> {
        let params = [("FanID", Variant::UI1(fan_id))];
        if let Ok(value) = self.call_int(CLASS_FAN, "Fan_Get_MaxSpeed", &params, "MaxFanSpeed") {
            return Ok(value);
        }
        let conn = self.conn()?;
        let rows: Vec<HashMap<String, Variant>> = conn
            .raw_query(format!(
                "SELECT CurrentFanMaxSpeed FROM {CLASS_FAN_TABLE_DATA} WHERE Fan_Id = {fan_id}"
            ))
            .map_err(|err| err.to_string())?;
        let best = rows
            .iter()
            .filter_map(|row| row.get("CurrentFanMaxSpeed").and_then(as_i64))
            .max()
            .unwrap_or(0);
        if best > 0 {
            Ok(best)
        } else {
            Err(format!(
                "fan {fan_id}: el firmware no informa velocidad máxima"
            ))
        }
    }

    fn read_full_speed(&mut self) -> Result<bool, String> {
        let value = self.call_prop(CLASS_FAN, "Fan_Get_FullSpeed", &[], "Status")?;
        as_bool(&value).ok_or_else(|| "Fan_Get_FullSpeed: estado no booleano".to_string())
    }

    fn read_fan_table(&mut self) -> Result<Vec<u16>, String> {
        let params = [("FanID", Variant::UI1(1)), ("SensorID", Variant::UI1(1))];
        match self.call(CLASS_FAN, "Fan_Get_Table", &params) {
            Ok(Some(output)) => {
                let value = output
                    .get_property("FanTable")
                    .map_err(|err| format!("Fan_Get_Table.FanTable: {err}"))?;
                let size = output
                    .get_property("FanTableSize")
                    .ok()
                    .and_then(|value| as_i64(&value))
                    .unwrap_or(0)
                    .max(0) as usize;
                let raw = variant_u16_array(&value)
                    .ok_or_else(|| "Fan_Get_Table: FanTable no es un arreglo".to_string())?;
                parse_fan_table(&raw, size)
                    .ok_or_else(|| "Fan_Get_Table: la tabla no tiene 10 puntos".to_string())
            }
            Ok(None) => Err("Fan_Get_Table: sin salida".to_string()),
            Err(err) => {
                // Fallback: la tabla que publica el driver como dato estático.
                let conn = self.conn()?;
                let rows: Vec<HashMap<String, Variant>> = conn
                    .raw_query(format!(
                        "SELECT FanTable_Data FROM {CLASS_FAN_TABLE_DATA} WHERE Fan_Id = 1"
                    ))
                    .map_err(|_| err.clone())?;
                for row in rows {
                    if let Some(levels) = row.get("FanTable_Data").and_then(variant_u16_array) {
                        if let Some(table) = parse_fan_table(&levels, 0) {
                            return Ok(table);
                        }
                    }
                }
                Err(err)
            }
        }
    }

    /// Estado del LED del botón de encendido (`Lighting_ID = 4`):
    /// `(Current_State_Type, Current_Brightness_Level)`. Sólo lectura: el color
    /// lo pone el EC cuando cambia el modo térmico; acá no se escribe nada.
    fn read_power_led(&mut self) -> Result<(i64, i64), String> {
        let params = [("Lighting_ID", Variant::UI1(POWER_LED_ID))];
        let output = self
            .call(CLASS_LIGHTING, "Get_Lighting_Current_Status", &params)?
            .ok_or_else(|| "Get_Lighting_Current_Status: sin salida".to_string())?;
        let state = out_int(&output, "Current_State_Type")?;
        let brightness = out_int(&output, "Current_Brightness_Level")?;
        Ok((state, brightness))
    }

    fn read_cpu_limit(&mut self, long_term: bool) -> Result<Value, String> {
        let method = if long_term {
            "CPU_Get_LongTerm_PowerLimit"
        } else {
            "CPU_Get_ShortTerm_PowerLimit"
        };
        let prefix = if long_term { "LongTerm" } else { "ShortTerm" };
        let output = self
            .call(CLASS_CPU, method, &[])?
            .ok_or_else(|| format!("{CLASS_CPU}.{method}: sin salida"))?;
        let value = out_int(&output, &format!("Current{prefix}_PowerLimit"))?;
        let min = out_int(&output, &format!("Min{prefix}_PowerLimit"))?;
        let max = out_int(&output, &format!("Max{prefix}_PowerLimit"))?;
        let step = out_int(&output, "step").unwrap_or(0).max(0);
        Ok(json!({"value": value, "min": min, "max": max, "step": step}))
    }

    fn identity(&mut self) -> (Option<String>, Option<String>) {
        if self.model.is_none() {
            self.model = self.cim_string("Win32_ComputerSystem", "Model");
        }
        if self.bios.is_none() {
            self.bios = self.cim_string("Win32_BIOS", "SMBIOSBIOSVersion");
        }
        (self.model.clone(), self.bios.clone())
    }

    fn cim_string(&mut self, class: &str, field: &str) -> Option<String> {
        if self.cim.is_none() {
            self.cim = WMIConnection::with_namespace_path(NS_CIMV2).ok();
        }
        let conn = self.cim.as_ref()?;
        let rows: Vec<HashMap<String, Variant>> = conn
            .raw_query(format!("SELECT {field} FROM {class}"))
            .ok()?;
        rows.into_iter().find_map(|row| {
            row.get(field).and_then(|value| match value {
                Variant::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
                _ => None,
            })
        })
    }

    fn competitors(&mut self) -> Vec<Value> {
        let _ = self.snapshot.refresh();
        let me = std::process::id();
        let mut found: Vec<(String, u32)> = Vec::new();
        for entry in self.snapshot.entries() {
            if entry.pid <= 4 || entry.pid == me {
                continue;
            }
            let name = clean_name(&entry.name);
            if name.is_empty() || is_protected(&name) || !is_competitor(&name) {
                continue;
            }
            found.push((name, entry.pid));
        }
        found.sort_by(|a, b| {
            a.0.to_lowercase()
                .cmp(&b.0.to_lowercase())
                .then(a.1.cmp(&b.1))
        });
        found
            .into_iter()
            .map(|(name, pid)| json!({"name": name, "pid": pid}))
            .collect()
    }

    // --------------------------------------------------------- escrituras

    fn apply_thermal(&mut self, id: u8) -> Result<Value, Error> {
        require_admin()?;
        if !MODES.iter().any(|(mode, _, _)| *mode == id) {
            return Err(Error::bad("modo térmico fuera de 1..4"));
        }
        let previous = self.read_thermal().ok();
        let led_before = self.read_power_led().ok();
        let mut backup = load_backup().unwrap_or_default();
        if backup.thermal_mode.is_none() {
            if let Some(previous) = previous {
                backup.thermal_mode = Some(previous);
            }
        }
        if backup.fan_table_stock.is_none() {
            if let Some(stock) = self.stock.clone() {
                backup.fan_table_stock = Some(stock);
            }
        }
        backup.saved_at = now_epoch();
        save_backup(&backup)?;

        // `Data` es UInt32 en el MOF: con I4 el proveedor contesta type mismatch.
        self.call(
            CLASS_GAMEZONE,
            "SetSmartFanMode",
            &[("Data", Variant::UI4(id as u32))],
        )
        .map_err(|err| Error::wmi(format!("SetSmartFanMode: {err}")))?;

        // Verificación: el firmware tiene la última palabra.
        let read_back = self.read_thermal();
        let verified = read_back.as_ref().ok().map(|current| *current == id);
        let current = read_back.unwrap_or(id);
        let led_after = self.read_power_led().ok();

        let mut warning: Option<String> = None;
        if verified == Some(false) {
            warning = Some(format!(
                "el modo no quedó: pedí {id} y el firmware informa {current}"
            ));
        }

        // El color del LED lo maneja el EC al cambiar el modo. Acá sólo se
        // comprueba que el estado del LED se haya movido cuando el color
        // esperado cambia; nunca se escribe un color.
        self.led_follows = match (led_before, led_after) {
            (Some((before, _)), Some((after, _))) => {
                let from_color = previous.and_then(led_color);
                let to_color = led_color(id);
                match (from_color, to_color) {
                    (Some((from, _)), Some((to, _))) if from != to => {
                        if before == after {
                            let message = format!(
                                "el LED del botón no cambió de estado ({before}) al pasar de \
                                 {from} a {to}; puede estar controlado por Vantage/LLT o \
                                 desincronizado del modo térmico"
                            );
                            warning = Some(match warning {
                                Some(prev) => format!("{prev}; {message}"),
                                None => message,
                            });
                            Some(false)
                        } else {
                            Some(true)
                        }
                    }
                    // Custom (o un modo sin color esperado): decide el firmware,
                    // no hay con qué comparar.
                    _ => None,
                }
            }
            _ => None,
        };
        if self.led_follows.is_none()
            && led_before.is_none()
            && led_after.is_none()
            && warning.is_none()
            && win::is_admin()
        {
            warning = Some(
                "no pude leer el estado del LED del botón (Get_Lighting_Current_Status)"
                    .to_string(),
            );
        }
        self.led_warning = warning.clone();

        self.cache = None;
        let mut payload = json!({
            "ok": true,
            "thermal": mode_obj(current),
            "previous": previous.map(mode_obj),
            "verified": verified,
            "power_led": power_led_json(current, led_after.as_ref(), self.led_follows, warning.as_deref()),
            "backup": backup_path().to_string_lossy(),
        });
        if let Some(warning) = warning {
            if let Some(object) = payload.as_object_mut() {
                object.insert("warning".to_string(), Value::String(warning));
            }
        }
        Ok(payload)
    }

    fn apply_fans(
        &mut self,
        full_speed: Option<bool>,
        table: Option<Vec<u16>>,
    ) -> Result<Value, Error> {
        require_admin()?;

        let current_table = self.read_fan_table().ok();
        let current_full = self.read_full_speed().ok();

        let mut backup = load_backup().unwrap_or_default();
        if backup.fan_table_stock.is_none() {
            // Nunca se escribe una tabla sin stock real de fábrica.
            let stock = current_table
                .clone()
                .or_else(|| self.stock.clone())
                .ok_or_else(|| {
                    Error::unavailable(
                        "no pude leer la tabla de fans actual: no escribo sin backup stock",
                    )
                })?;
            backup.fan_table_stock = Some(stock);
        }
        if backup.full_speed.is_none() {
            backup.full_speed = current_full;
        }
        backup.saved_at = now_epoch();
        save_backup(&backup)?;
        self.stock = backup.fan_table_stock.clone();

        let mut wrote_table = false;
        if let Some(ref levels) = table {
            let levels: Vec<u16> = levels.iter().map(|level| (*level).clamp(1, 10)).collect();
            // La tabla sólo se acepta en modo "no full speed".
            self.call(
                CLASS_FAN,
                "Fan_Set_FullSpeed",
                &[("Status", Variant::Bool(false))],
            )
            .map_err(|err| Error::wmi(format!("Fan_Set_FullSpeed(false): {err}")))?;
            let bytes = fan_table_bytes(&levels);
            let array = Variant::Array(bytes.into_iter().map(Variant::UI1).collect());
            self.call(CLASS_FAN, "Fan_Set_Table", &[("FanTable", array)])
                .map_err(|err| Error::wmi(format!("Fan_Set_Table: {err}")))?;
            wrote_table = true;
        }

        if let Some(enabled) = full_speed {
            self.call(
                CLASS_FAN,
                "Fan_Set_FullSpeed",
                &[("Status", Variant::Bool(enabled))],
            )
            .map_err(|err| Error::wmi(format!("Fan_Set_FullSpeed({enabled}): {err}")))?;
        }

        let read_back = self.read_fan_table().ok();
        let verified = match (&table, &read_back) {
            (Some(wanted), Some(current)) => {
                let wanted: Vec<u16> = wanted.iter().map(|level| (*level).clamp(1, 10)).collect();
                *current == wanted
            }
            (Some(_), None) => false,
            (None, _) => true,
        };
        let full = self
            .read_full_speed()
            .unwrap_or_else(|_| full_speed.unwrap_or(false));
        self.cache = None;

        let mut payload = json!({
            "ok": true,
            "full_speed": full,
            "fan_table": read_back.map(|table| json!(table)).unwrap_or(Value::Null),
            "table_written": wrote_table,
            "verified": verified,
            "backup": backup_path().to_string_lossy(),
        });
        if wrote_table && !verified {
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "error".to_string(),
                    Value::String(
                        "la tabla no quedó como se pidió; revisá /api/legion".to_string(),
                    ),
                );
            }
        }
        Ok(payload)
    }

    fn apply_cpu(&mut self, long: Option<i64>, short: Option<i64>) -> Result<Value, Error> {
        require_admin()?;

        let mut backup = load_backup().unwrap_or_default();
        let mut changed = Vec::new();

        if let Some(value) = long {
            let current = self.read_cpu_limit(true).ok();
            if let Some(current) = &current {
                validate_limit(value, current).map_err(Error::bad)?;
            }
            if backup.cpu_long.is_none() {
                backup.cpu_long = current
                    .as_ref()
                    .and_then(|limit| limit.get("value"))
                    .and_then(Value::as_i64);
            }
            backup.saved_at = now_epoch();
            save_backup(&backup)?;
            self.call(
                CLASS_CPU,
                "CPU_Set_LongTerm_PowerLimit",
                &[("value", Variant::UI4(value as u32))],
            )
            .map_err(|err| Error::wmi(format!("CPU_Set_LongTerm_PowerLimit: {err}")))?;
            changed.push("long");
        }

        if let Some(value) = short {
            let current = self.read_cpu_limit(false).ok();
            if let Some(current) = &current {
                validate_limit(value, current).map_err(Error::bad)?;
            }
            if backup.cpu_short.is_none() {
                backup.cpu_short = current
                    .as_ref()
                    .and_then(|limit| limit.get("value"))
                    .and_then(Value::as_i64);
            }
            backup.saved_at = now_epoch();
            save_backup(&backup)?;
            self.call(
                CLASS_CPU,
                "CPU_Set_ShortTerm_PowerLimit",
                &[("value", Variant::UI4(value as u32))],
            )
            .map_err(|err| Error::wmi(format!("CPU_Set_ShortTerm_PowerLimit: {err}")))?;
            changed.push("short");
        }

        self.cache = None;
        Ok(json!({
            "ok": true,
            "changed": changed,
            "cpu": {
                "long": self.read_cpu_limit(true).ok(),
                "short": self.read_cpu_limit(false).ok(),
            },
            "backup": backup_path().to_string_lossy(),
        }))
    }

    fn apply_chief(&mut self, enabled: bool) -> Result<Value, Error> {
        require_admin()?;

        let mut backup = load_backup().unwrap_or_default();
        backup.chief = Some(enabled);
        backup.saved_at = now_epoch();
        save_backup(&backup)?;
        self.chief = enabled;

        let mut killed: Vec<Value> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        let mut skipped: Vec<Value> = Vec::new();

        if enabled {
            let _ = self.snapshot.refresh();
            let me = std::process::id();
            for entry in self.snapshot.entries() {
                if entry.pid <= 4 || entry.pid == me {
                    continue;
                }
                let name = clean_name(&entry.name);
                if name.is_empty() || !is_competitor(&name) {
                    continue;
                }
                if is_protected(&name) {
                    skipped.push(json!({"name": name, "pid": entry.pid, "reason": "protegido"}));
                    continue;
                }
                match win::terminate_process(entry.pid) {
                    Ok(()) => killed.push(json!({"name": name, "pid": entry.pid})),
                    Err(err) => failed.push(json!({"name": name, "pid": entry.pid, "error": err})),
                }
            }
        }

        self.cache = None;
        Ok(json!({
            "ok": true,
            "enabled": enabled,
            "killed": killed,
            "failed": failed,
            "skipped": skipped,
        }))
    }

    fn apply_restore(&mut self) -> Result<Value, Error> {
        require_admin()?;

        let backup = match load_backup() {
            Some(backup) => backup,
            None => {
                return Ok(json!({
                    "ok": true,
                    "restored": false,
                    "applied": [],
                    "failed": [],
                    "message": "no hay backup en disco; nada que restaurar",
                }))
            }
        };

        let mut applied: Vec<&str> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();

        if let Some(table) = backup.fan_table_stock.clone() {
            if table.len() != FAN_TABLE_LEN {
                failed.push(json!({
                    "what": "fan_table",
                    "error": format!("backup con {} puntos en vez de {FAN_TABLE_LEN}", table.len()),
                }));
            } else {
                if let Err(err) = self.call(
                    CLASS_FAN,
                    "Fan_Set_FullSpeed",
                    &[("Status", Variant::Bool(false))],
                ) {
                    failed.push(json!({"what": "full_speed_off", "error": err}));
                }
                let bytes = fan_table_bytes(&table);
                let array = Variant::Array(bytes.into_iter().map(Variant::UI1).collect());
                match self.call(CLASS_FAN, "Fan_Set_Table", &[("FanTable", array)]) {
                    Ok(_) => applied.push("fan_table"),
                    Err(err) => failed.push(json!({"what": "fan_table", "error": err})),
                }
            }
        }

        if let Some(enabled) = backup.full_speed {
            match self.call(
                CLASS_FAN,
                "Fan_Set_FullSpeed",
                &[("Status", Variant::Bool(enabled))],
            ) {
                Ok(_) => applied.push("full_speed"),
                Err(err) => failed.push(json!({"what": "full_speed", "error": err})),
            }
        }

        if let Some(mode) = backup.thermal_mode {
            match self.call(
                CLASS_GAMEZONE,
                "SetSmartFanMode",
                &[("Data", Variant::UI4(mode as u32))],
            ) {
                Ok(_) => applied.push("thermal"),
                Err(err) => failed.push(json!({"what": "thermal", "error": err})),
            }
        }

        if let Some(value) = backup.cpu_long.filter(|value| *value > 0) {
            match self.call(
                CLASS_CPU,
                "CPU_Set_LongTerm_PowerLimit",
                &[("value", Variant::UI4(value as u32))],
            ) {
                Ok(_) => applied.push("cpu_long"),
                Err(err) => failed.push(json!({"what": "cpu_long", "error": err})),
            }
        }

        if let Some(value) = backup.cpu_short.filter(|value| *value > 0) {
            match self.call(
                CLASS_CPU,
                "CPU_Set_ShortTerm_PowerLimit",
                &[("value", Variant::UI4(value as u32))],
            ) {
                Ok(_) => applied.push("cpu_short"),
                Err(err) => failed.push(json!({"what": "cpu_short", "error": err})),
            }
        }

        self.cache = None;
        Ok(json!({
            "ok": failed.is_empty(),
            "restored": true,
            "applied": applied,
            "failed": failed,
            "thermal": self.read_thermal().ok().map(mode_obj),
            "fan_table": self.read_fan_table().ok(),
        }))
    }
}

fn require_admin() -> Result<(), Error> {
    if win::is_admin() {
        Ok(())
    } else {
        Err(Error::denied())
    }
}

fn validate_limit(value: i64, limit: &Value) -> Result<(), String> {
    let min = limit.get("min").and_then(Value::as_i64).unwrap_or(0);
    let max = limit.get("max").and_then(Value::as_i64).unwrap_or(0);
    let step = limit.get("step").and_then(Value::as_i64).unwrap_or(0);
    if max > 0 && (value < min || value > max) {
        return Err(format!(
            "valor {value} W fuera del rango del firmware ({min}..{max} W)"
        ));
    }
    if step > 0 && (value - min) % step != 0 {
        return Err(format!(
            "valor {value} W no cae en un paso de {step} W desde {min} W"
        ));
    }
    Ok(())
}

// ------------------------------------------------------------ backup disco

#[derive(Serialize, Deserialize, Clone, Default)]
struct Backup {
    #[serde(default)]
    saved_at: f64,
    #[serde(default)]
    restored_at: Option<f64>,
    #[serde(default)]
    thermal_mode: Option<u8>,
    #[serde(default)]
    fan_table_stock: Option<Vec<u16>>,
    #[serde(default)]
    full_speed: Option<bool>,
    #[serde(default)]
    cpu_long: Option<i64>,
    #[serde(default)]
    cpu_short: Option<i64>,
    #[serde(default)]
    chief: Option<bool>,
}

/// `legion.json` junto a `config.json` (por defecto `%LOCALAPPDATA%\Domador`).
/// `DOMADOR_LEGION_CONFIG` permite apuntarlo a otro lado en pruebas.
fn backup_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOMADOR_LEGION_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    match config::default_path().parent() {
        Some(dir) => dir.join("legion.json"),
        None => PathBuf::from("legion.json"),
    }
}

fn load_backup() -> Option<Backup> {
    let text = fs::read_to_string(backup_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_backup(backup: &Backup) -> Result<(), Error> {
    let path = backup_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::wmi(format!("no pude crear {parent:?}: {err}")))?;
    }
    let text = serde_json::to_string_pretty(backup)
        .map_err(|err| Error::wmi(format!("backup ilegible: {err}")))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)
        .map_err(|err| Error::wmi(format!("no pude escribir el backup: {err}")))?;
    fs::rename(&tmp, &path)
        .map_err(|err| Error::wmi(format!("no pude reemplazar el backup: {err}")))?;
    Ok(())
}

// --------------------------------------------------------------- utilidades

fn mode_obj(id: u8) -> Value {
    match MODES.iter().find(|(mode, _, _)| *mode == id) {
        Some((id, key, label)) => json!({"id": id, "mode": key, "label": label}),
        None => json!({"id": id, "mode": "unknown", "label": "Desconocido"}),
    }
}

fn modes_json() -> Value {
    Value::Array(
        MODES
            .iter()
            .map(|(id, key, label)| json!({"id": id, "mode": key, "label": label}))
            .collect(),
    )
}

fn normalize_mode(value: i64) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

/// Tabla de 64 bytes que espera `Fan_Set_Table`: FSTM, FSID, FSTL=10 (u32 LE),
/// diez niveles u16 LE y relleno en cero.
fn fan_table_bytes(table: &[u16]) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[0] = 1;
    bytes[1] = 0;
    bytes[2..6].copy_from_slice(&(FAN_TABLE_LEN as u32).to_le_bytes());
    for (index, level) in table.iter().take(FAN_TABLE_LEN).enumerate() {
        let offset = 6 + index * 2;
        bytes[offset..offset + 2].copy_from_slice(&level.to_le_bytes());
    }
    bytes
}

/// Color que el firmware pone en el LED del botón para cada modo térmico.
/// `custom` no tiene color fijo: lo decide el EC, así que no se inventa.
fn led_color(mode: u8) -> Option<(&'static str, &'static str)> {
    match mode {
        1 => Some(("blue", "azul")),
        2 => Some(("white", "blanco")),
        3 => Some(("red", "rojo")),
        _ => None,
    }
}

fn power_led_json(
    mode: u8,
    led: Option<&(i64, i64)>,
    follows: Option<bool>,
    warning: Option<&str>,
) -> Value {
    let color = led_color(mode);
    let mut value = json!({
        "readable": led.is_some(),
        "lighting_id": POWER_LED_ID,
        "state": led.map(|(state, _)| *state),
        "brightness": led.map(|(_, level)| *level),
        "expected": color.map(|(english, _)| english),
        "expected_label": color.map(|(_, spanish)| spanish),
        "follows_mode": follows,
    });
    if let Some(warning) = warning {
        if let Some(object) = value.as_object_mut() {
            object.insert("warning".to_string(), Value::String(warning.to_string()));
        }
    }
    value
}

/// Acepta las dos formas que puede devolver el firmware para `FanTable`: la
/// tabla de 10 niveles pelada (`UInt8Array` con `FanTableSize` = 10) o el
/// buffer de 64 bytes del formato de escritura (FSTM/FSID/FSTL + 10 u16 LE).
/// No normaliza valores: un 0 del stock se guarda como 0, no como 1.
fn parse_fan_table(raw: &[u16], size: usize) -> Option<Vec<u16>> {
    let fstl = if raw.len() >= 6 {
        u32::from_le_bytes([raw[2] as u8, raw[3] as u8, raw[4] as u8, raw[5] as u8])
    } else {
        0
    };
    let header = raw.len() >= 26 && raw[0] == 1 && raw[1] == 0 && fstl == FAN_TABLE_LEN as u32;
    if header {
        let mut table = Vec::with_capacity(FAN_TABLE_LEN);
        for index in 0..FAN_TABLE_LEN {
            let offset = 6 + index * 2;
            let low = raw[offset] as u8;
            let high = raw[offset + 1] as u8;
            table.push(u16::from_le_bytes([low, high]));
        }
        return Some(table);
    }
    let count = if (1..=FAN_TABLE_LEN).contains(&size) {
        size
    } else {
        raw.len()
    };
    if count < FAN_TABLE_LEN || raw.len() < FAN_TABLE_LEN {
        return None;
    }
    Some(raw.iter().take(FAN_TABLE_LEN).copied().collect())
}

fn variant_u16_array(value: &Variant) -> Option<Vec<u16>> {
    match value {
        Variant::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let number = as_i64(item)?;
                out.push(u16::try_from(number.clamp(0, u16::MAX as i64)).ok()?);
            }
            Some(out)
        }
        _ => None,
    }
}

fn as_i64(value: &Variant) -> Option<i64> {
    Some(match value {
        Variant::I1(value) => *value as i64,
        Variant::I2(value) => *value as i64,
        Variant::I4(value) => *value as i64,
        Variant::I8(value) => *value,
        Variant::UI1(value) => *value as i64,
        Variant::UI2(value) => *value as i64,
        Variant::UI4(value) => *value as i64,
        Variant::UI8(value) => i64::try_from(*value).ok()?,
        Variant::Bool(value) => i64::from(*value),
        Variant::R4(value) => *value as i64,
        Variant::R8(value) => *value as i64,
        Variant::String(text) => text.trim().parse().ok()?,
        _ => return None,
    })
}

fn as_bool(value: &Variant) -> Option<bool> {
    match value {
        Variant::Bool(value) => Some(*value),
        other => as_i64(other).map(|number| number != 0),
    }
}

fn out_int(output: &IWbemClassWrapper, property: &str) -> Result<i64, String> {
    let value = output
        .get_property(property)
        .map_err(|err| format!("{property}: {err}"))?;
    as_i64(&value).ok_or_else(|| format!("{property}: valor no numérico"))
}

fn clean_name(name: &str) -> String {
    name.trim()
        .trim_matches('\0')
        .trim_end_matches(".exe")
        .to_string()
}

fn lower_name(name: &str) -> String {
    clean_name(name).to_ascii_lowercase()
}

fn is_competitor(name: &str) -> bool {
    let name = lower_name(name);
    COMPETITOR_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn is_protected(name: &str) -> bool {
    let name = lower_name(name);
    PROTECTED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn is_denied(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("access")
        || message.contains("acceso")
        || message.contains("denied")
        || message.contains("0x80041003")
        || message.contains("wbem_e_access_denied")
}

fn dedup_notes(notes: Vec<String>) -> Option<String> {
    let mut unique: Vec<String> = Vec::new();
    for note in notes {
        let note = note.trim().to_string();
        if !note.is_empty() && !unique.iter().any(|seen| seen == &note) {
            unique.push(note);
        }
    }
    if unique.is_empty() {
        return None;
    }
    let mut text = unique.join(" | ");
    if text.chars().count() > 400 {
        text = text.chars().take(397).collect::<String>() + "...";
    }
    Some(text)
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_modos_por_numero_y_por_nombre() {
        assert_eq!(parse_mode(Some(&json!(1))), Some(1));
        assert_eq!(parse_mode(Some(&json!(4))), Some(4));
        assert_eq!(parse_mode(Some(&json!("quiet"))), Some(1));
        assert_eq!(parse_mode(Some(&json!("Performance"))), Some(3));
        assert_eq!(parse_mode(Some(&json!(5))), None);
        assert_eq!(parse_mode(Some(&json!("turbo"))), None);
        assert_eq!(parse_mode(None), None);
    }

    #[test]
    fn arma_la_tabla_de_64_bytes() {
        let table: Vec<u16> = (1..=10).collect();
        let bytes = fan_table_bytes(&table);
        assert_eq!(bytes.len(), 64);
        assert_eq!(bytes[0], 1);
        assert_eq!(bytes[1], 0);
        assert_eq!(u32::from_le_bytes(bytes[2..6].try_into().unwrap()), 10);
        for index in 0..10 {
            let offset = 6 + index * 2;
            let level = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            assert_eq!(level, (index + 1) as u16);
        }
        assert!(bytes[26..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn reconoce_competidores_y_protegidos() {
        for name in [
            "LenovoVantageService.exe",
            "LenovoVantage-(GenericMessagingAddin).exe",
            "LegionArena.exe",
            "VantageBridgePlugin",
            "LegionFanControl.exe",
            "LenovoLegionToolkit.exe",
            "UXTU.exe",
            "RyzenController.exe",
            "Ryzen Controller 2.5.4.exe",
        ] {
            assert!(is_competitor(name), "debería matchear: {name}");
        }
        for name in [
            "LenovoHotkeys.exe",
            "MSIAfterburner.exe",
            "chrome.exe",
            "steam.exe",
            "domador-tray.exe",
            "explorer.exe",
        ] {
            assert!(!is_competitor(name), "no debería matchear: {name}");
        }
        assert!(is_protected("domador-ui.exe"));
        assert!(is_protected("LenovoHotkeys"));
    }

    #[test]
    fn parsea_tabla_pelada_y_buffer_de_64_bytes() {
        // Formato "pelado": 10 niveles tal como los informa FanTableSize=10.
        let flat: Vec<u16> = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 10];
        assert_eq!(parse_fan_table(&flat, 10), Some(flat.clone()));
        // El 0 del stock se conserva: no se normaliza a 1.
        assert_eq!(parse_fan_table(&flat, 10).unwrap()[0], 0);

        // Formato de escritura: FSTM/FSID/FSTL + 10 u16 LE + padding.
        let mut buffer = vec![0u16; 32];
        buffer[0] = 1;
        buffer[1] = 0;
        buffer[2] = 10;
        for index in 0..10u16 {
            buffer[6 + index as usize * 2] = index + 1;
            buffer[7 + index as usize * 2] = 0;
        }
        let parsed = parse_fan_table(&buffer, 10).expect("buffer con header");
        assert_eq!(parsed, (1..=10).collect::<Vec<u16>>());

        // Tabla corta: no alcanza.
        assert_eq!(parse_fan_table(&[1, 2, 3], 3), None);
        // 10 valores que casualmente empiezan con 1 no se confunden con header.
        let flat_starting_one: Vec<u16> = vec![1, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(
            parse_fan_table(&flat_starting_one, 10),
            Some(flat_starting_one.clone())
        );
    }

    #[test]
    fn colores_del_led_por_modo() {
        assert_eq!(led_color(1).map(|(en, _)| en), Some("blue"));
        assert_eq!(led_color(2).map(|(en, _)| en), Some("white"));
        assert_eq!(led_color(3).map(|(en, _)| en), Some("red"));
        assert_eq!(led_color(4), None, "custom lo decide el firmware");
        assert_eq!(led_color(0), None);

        let led = (2i64, 3i64);
        let value = power_led_json(1, Some(&led), Some(true), None);
        assert_eq!(value["expected"], json!("blue"));
        assert_eq!(value["expected_label"], json!("azul"));
        assert_eq!(value["state"], json!(2));
        assert_eq!(value["brightness"], json!(3));
        assert_eq!(value["readable"], json!(true));
        assert!(value.get("warning").is_none());

        let value = power_led_json(0, None, None, Some("sin dato"));
        assert_eq!(value["readable"], json!(false));
        assert_eq!(value["warning"], json!("sin dato"));
        assert_eq!(value["expected"], Value::Null);
    }

    #[test]
    fn valida_limites_del_firmware() {
        let limit = json!({"value": 45, "min": 35, "max": 75, "step": 5});
        assert!(validate_limit(45, &limit).is_ok());
        assert!(validate_limit(50, &limit).is_ok());
        assert!(validate_limit(30, &limit).is_err());
        assert!(validate_limit(80, &limit).is_err());
        assert!(validate_limit(47, &limit).is_err());
    }

    /// Diagnóstico manual contra el hardware real. Sólo lee: no escribe en el
    /// EC. Sin admin devuelve 0x80041003 en ExecMethod, que es justamente lo
    /// que se quiere distinguir de "método inexistente" o "parámetro inválido".
    ///
    /// `cargo test --lib legion::tests::diagnostico_wmi -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn diagnostico_wmi() {
        let mut ctx = Ctx::new();
        println!("admin = {}", win::is_admin());
        println!("modelo/BIOS = {:?}", ctx.identity());
        println!("thermal = {:?}", ctx.read_thermal());
        println!("fan 1 rpm = {:?}", ctx.read_fan_rpm(1));
        println!("fan 2 rpm = {:?}", ctx.read_fan_rpm(2));
        println!("fan max 1 = {:?}", ctx.fan_max(1));
        println!("full speed = {:?}", ctx.read_full_speed());
        println!("tabla = {:?}", ctx.read_fan_table());
        println!("cpu long = {:?}", ctx.read_cpu_limit(true));
        println!("cpu short = {:?}", ctx.read_cpu_limit(false));
        println!("power led = {:?}", ctx.read_power_led());
        println!(
            "temps = {:?} / {:?}",
            ctx.call_int(CLASS_GAMEZONE, "GetCPUTemp", &[], "Data"),
            ctx.call_int(CLASS_GAMEZONE, "GetGPUTemp", &[], "Data")
        );
        println!(
            "tabla cruda = {:?}",
            ctx.call(
                CLASS_FAN,
                "Fan_Get_Table",
                &[("FanID", Variant::UI1(1)), ("SensorID", Variant::UI1(1))]
            )
            .map(|_| "ok")
        );
        println!("estado = {}", ctx.status());
    }

    #[test]
    fn backup_ida_y_vuelta() {
        let dir = std::env::temp_dir().join(format!("domador-legion-test-{}", std::process::id()));
        let path = dir.join("legion.json");
        std::env::set_var("DOMADOR_LEGION_CONFIG", &path);

        let backup = Backup {
            saved_at: 123.0,
            thermal_mode: Some(2),
            fan_table_stock: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
            full_speed: Some(false),
            cpu_long: Some(45),
            cpu_short: Some(65),
            chief: Some(true),
            ..Backup::default()
        };
        save_backup(&backup).expect("guardar backup");
        let loaded = load_backup().expect("leer backup");
        assert_eq!(loaded.thermal_mode, Some(2));
        assert_eq!(loaded.fan_table_stock, backup.fan_table_stock);
        assert_eq!(loaded.cpu_long, Some(45));
        assert_eq!(loaded.chief, Some(true));
        assert!(
            !path.with_extension("tmp").exists(),
            "quedó el archivo temporal"
        );

        std::env::remove_var("DOMADOR_LEGION_CONFIG");
        let _ = fs::remove_dir_all(&dir);
    }
}
