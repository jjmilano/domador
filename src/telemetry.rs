//! Telemetría histórica del subsistema Legion.
//!
//! Un hilo propio (`legion-telemetry`) muestrea cada 2 s sin tocar el mutex del
//! motor: las lecturas Legion pasan por el worker WMI (que ya serializa el EC)
//! y la GPU se consulta con `nvidia-smi` cada 5 s. El resultado se guarda en un
//! ring buffer de 24 h en memoria y se agrega (append) a un CSV en
//! `%LOCALAPPDATA%\Domador\telemetry.csv`, con rotación acotada.
//!
//! Reglas:
//! - Nunca se inventa una temperatura: si WMI no contesta, la muestra va con
//!   `null` y el historial sigue respondiendo `200` con `available:false`.
//! - El CSV se lee al arrancar para no perder el historial entre reinicios.
//! - La rotación deja como máximo `MAX_ARCHIVES` archivos viejos (≈24 MB en
//!   total): el archivo activo nunca crece sin límite.
//! - El sampler arranca en el primer GET de Legion (la UI lo hace al abrir la
//!   pestaña) y sigue hasta que el proceso termina.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::config;

/// Cadencia de muestreo. El snapshot WMI tiene cache de 2 s, así que este es
/// el menor intervalo que no duplica lecturas del EC.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Retención del ring buffer y del historial servido.
pub const RETENTION_S: f64 = 24.0 * 3600.0;
const RING_CAP: usize = 43_264;
/// Tope del archivo activo; al pasarlo rota y se conservan `MAX_ARCHIVES`.
const MAX_CSV_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVES: usize = 2;
const MAX_WINDOW_MIN: f64 = 1440.0;
const DRAW_WINDOW_MIN: f64 = 60.0;
const HOT_C: f64 = 90.0;
const GPU_QUERY_TTL: Duration = Duration::from_secs(5);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const CSV_HEADER: &str = "ts,time_utc,cpu_temp_c,gpu_temp_c,fan1_rpm,fan2_rpm,\
gpu_power_w,gpu_clock_mhz,thermal_mode,cpu_limit_w";

// ------------------------------------------------------------------ muestra

/// Una lectura del hardware. Todo opcional: `None` significa "el firmware no
/// contestó", no cero.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub ts: f64,
    pub cpu_temp: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub fan1_rpm: Option<u32>,
    pub fan2_rpm: Option<u32>,
    pub gpu_power_w: Option<f64>,
    pub gpu_clock_mhz: Option<u32>,
    pub thermal_mode: Option<u8>,
    pub cpu_limit_w: Option<f64>,
}

impl Sample {
    fn to_json(&self) -> Value {
        json!({
            "ts": round(self.ts, 3),
            "time": iso_utc(self.ts),
            "cpu_temp": self.cpu_temp.map(|value| round(value, 1)),
            "gpu_temp": self.gpu_temp.map(|value| round(value, 1)),
            "fan1_rpm": self.fan1_rpm,
            "fan2_rpm": self.fan2_rpm,
            "gpu_power_w": self.gpu_power_w.map(|value| round(value, 2)),
            "gpu_clock_mhz": self.gpu_clock_mhz,
            "thermal_mode": self.thermal_mode,
            "thermal": self.thermal_mode.map(crate::legion::thermal_key),
            "mode": self.thermal_mode.map(crate::legion::thermal_key),
            "cpu_limit_w": self.cpu_limit_w.map(|value| round(value, 1)),
        })
    }

    fn to_csv(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{},{},{}",
            format_number(self.ts, 3),
            iso_utc(self.ts),
            opt_number(self.cpu_temp, 1),
            opt_number(self.gpu_temp, 1),
            opt_int(self.fan1_rpm.map(|value| value as i64)),
            opt_int(self.fan2_rpm.map(|value| value as i64)),
            opt_number(self.gpu_power_w, 2),
            opt_int(self.gpu_clock_mhz.map(|value| value as i64)),
            opt_int(self.thermal_mode.map(|value| value as i64)),
            opt_number(self.cpu_limit_w, 1),
        )
    }

    fn from_csv(line: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 10 {
            return None;
        }
        let ts: f64 = fields[0].trim().parse().ok()?;
        if !ts.is_finite() || ts <= 0.0 {
            return None;
        }
        Some(Self {
            ts,
            cpu_temp: parse_f64(fields[2]),
            gpu_temp: parse_f64(fields[3]),
            fan1_rpm: parse_u32(fields[4]),
            fan2_rpm: parse_u32(fields[5]),
            gpu_power_w: parse_f64(fields[6]),
            gpu_clock_mhz: parse_u32(fields[7]),
            thermal_mode: parse_u32(fields[8]).and_then(|value| u8::try_from(value).ok()),
            cpu_limit_w: parse_f64(fields[9]),
        })
    }
}

// -------------------------------------------------------------------- store

struct Store {
    samples: VecDeque<Sample>,
    /// Ruta del CSV activo; `None` si no se pudo resolver.
    path: Option<PathBuf>,
    /// Último estado que informó el worker Legion. `None` = todavía no hubo
    /// muestra (no se afirma que WMI falte).
    available: Option<bool>,
    admin: bool,
    last_error: Option<String>,
    last_at: f64,
    samples_total: u64,
    csv_error: Option<String>,
}

impl Store {
    fn open(path: PathBuf) -> Self {
        let loaded = load_csv(&path);
        let mut store = Self {
            samples: VecDeque::new(),
            path: Some(path),
            available: None,
            admin: crate::win::is_admin(),
            last_error: None,
            last_at: 0.0,
            samples_total: 0,
            csv_error: None,
        };
        for sample in loaded.samples {
            store.push(sample);
        }
        store.last_at = loaded.last_at;
        store.samples_total = store.samples.len() as u64;
        store.csv_error = loaded.error;
        store
    }

    fn push(&mut self, sample: Sample) {
        let ts = sample.ts;
        if let Some(last) = self.samples.back() {
            if (last.ts - ts).abs() < 0.4 {
                // Reinicio en el mismo segundo: la muestra nueva reemplaza a la
                // que vino del CSV, no se duplica.
                self.samples.pop_back();
            }
        }
        self.samples.push_back(sample);
        self.last_at = ts;
        let cutoff = ts - RETENTION_S;
        while self.samples.front().map(|s| s.ts < cutoff).unwrap_or(false) {
            self.samples.pop_front();
        }
        while self.samples.len() > RING_CAP {
            self.samples.pop_front();
        }
    }
}

struct Loaded {
    samples: Vec<Sample>,
    last_at: f64,
    error: Option<String>,
}

fn lock_store() -> MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE
        .get_or_init(|| Mutex::new(Store::open(csv_path())))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

static STARTED: OnceLock<()> = OnceLock::new();

/// Arranca el sampler una sola vez. Idempotente y barato: el primer GET de
/// Legion lo dispara.
pub fn ensure_started() {
    STARTED.get_or_init(|| {
        // Inicializa el store (y carga el CSV) antes de que la API lo consulte.
        {
            let _guard = lock_store();
        }
        match thread::Builder::new()
            .name("legion-telemetry".to_string())
            .spawn(run)
        {
            Ok(_) => {}
            Err(err) => eprintln!("telemetría: no pude crear el hilo: {err}"),
        }
    });
}

fn csv_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOMADOR_TELEMETRY_CSV") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    match config::default_path().parent() {
        Some(dir) => dir.join("telemetry.csv"),
        None => PathBuf::from("telemetry.csv"),
    }
}

fn load_csv(path: &Path) -> Loaded {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => {
            return Loaded {
                samples: Vec::new(),
                last_at: 0.0,
                error: None,
            }
        }
    };
    let reader = BufReader::new(file);
    let now = now_epoch();
    let cutoff = now - RETENTION_S;
    let mut samples: VecDeque<Sample> = VecDeque::new();
    let mut bad_lines = 0u64;
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() || line.starts_with("ts,") {
            continue;
        }
        match Sample::from_csv(line) {
            Some(sample) => {
                if sample.ts >= cutoff {
                    samples.push_back(sample);
                    while samples.len() > RING_CAP {
                        samples.pop_front();
                    }
                }
            }
            None => bad_lines += 1,
        }
    }
    let last_at = samples.back().map(|sample| sample.ts).unwrap_or(0.0);
    let error = if bad_lines > 0 {
        Some(format!(
            "{bad_lines} línea(s) del CSV no se pudieron parsear"
        ))
    } else {
        None
    };
    Loaded {
        samples: samples.into(),
        last_at,
        error,
    }
}

// ------------------------------------------------------------------ sampler

fn run() {
    let mut sink = CsvSink::new(csv_path());
    let mut gpu = GpuCache::default();
    loop {
        let started = Instant::now();
        let (sample, available, error) = collect(&mut gpu);
        let line = sample.to_csv();
        let observed = sample.clone();
        {
            let mut store = lock_store();
            store.push(sample);
            store.available = Some(available);
            store.admin = crate::win::is_admin();
            store.samples_total = store.samples_total.saturating_add(1);
            store.last_error = error;
        }
        sink.append(&line);
        // El control adaptativo evalúa la misma muestra y, si hace falta,
        // escribe en el EC por el worker WMI. Nunca bloquea al motor.
        crate::adaptive::observe(&observed, available);
        if let Some(err) = sink.error.clone() {
            let mut store = lock_store();
            store.csv_error = Some(err);
        }
        let elapsed = started.elapsed();
        let wait = SAMPLE_INTERVAL.saturating_sub(elapsed);
        thread::sleep(wait.max(Duration::from_millis(200)));
    }
}

/// Lee Legion (por el worker WMI) y completa GPU con nvidia-smi. Nunca falla:
/// si no hay datos, devuelve la muestra con `null` y el motivo.
fn collect(gpu: &mut GpuCache) -> (Sample, bool, Option<String>) {
    let status = crate::legion::sample();
    let available = status
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let error = status
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string);

    gpu.refresh();

    let cpu_temp = sanitize_temp(status.pointer("/temps/cpu").and_then(Value::as_f64));
    let gpu_temp = sanitize_temp(status.pointer("/temps/gpu").and_then(Value::as_f64))
        .or_else(|| sanitize_temp(gpu.temp));
    let fan1_rpm = status
        .pointer("/fans/0/rpm")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let fan2_rpm = status
        .pointer("/fans/1/rpm")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let thermal_mode = status
        .get("thermal")
        .and_then(|value| value.get("id"))
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| (1..=4).contains(value));
    let cpu_limit_w = status
        .pointer("/cpu/long/value")
        .and_then(Value::as_f64)
        .filter(|value| *value > 0.0);

    let sample = Sample {
        ts: now_epoch(),
        cpu_temp,
        gpu_temp,
        fan1_rpm,
        fan2_rpm,
        gpu_power_w: gpu.power.filter(|value| *value >= 0.0),
        gpu_clock_mhz: gpu.clock,
        thermal_mode,
        cpu_limit_w,
    };
    (sample, available, error)
}

/// La BIOS a veces devuelve centinelas (0, 255). Se descartan: mejor `null`
/// que un pico falso en el gráfico.
fn sanitize_temp(value: Option<f64>) -> Option<f64> {
    value.filter(|temp| *temp > 0.0 && *temp <= 150.0)
}

struct GpuCache {
    at: Option<Instant>,
    temp: Option<f64>,
    power: Option<f64>,
    clock: Option<u32>,
}

impl Default for GpuCache {
    fn default() -> Self {
        Self {
            at: None,
            temp: None,
            power: None,
            clock: None,
        }
    }
}

impl GpuCache {
    fn refresh(&mut self) {
        if self
            .at
            .map(|at| at.elapsed() < GPU_QUERY_TTL)
            .unwrap_or(false)
        {
            return;
        }
        self.at = Some(Instant::now());
        let output = Command::new("nvidia-smi")
            .args([
                "--query-gpu=temperature.gpu,power.draw,clocks.current.graphics",
                "--format=csv,noheader,nounits",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let Ok(output) = output else {
            self.temp = None;
            self.power = None;
            self.clock = None;
            return;
        };
        if !output.status.success() {
            self.temp = None;
            self.power = None;
            self.clock = None;
            return;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let Some(line) = text.lines().next() else {
            return;
        };
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        self.temp = parts.first().and_then(|value| parse_f64(value));
        self.power = parts.get(1).and_then(|value| parse_f64(value));
        self.clock = parts
            .get(2)
            .and_then(|value| parse_f64(value))
            .map(|value| value.round() as u32);
    }
}

// --------------------------------------------------------------------- CSV

struct CsvSink {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    error: Option<String>,
}

impl CsvSink {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            size: 0,
            error: None,
        }
    }

    fn append(&mut self, line: &str) {
        if self.ensure_open().is_err() {
            return;
        }
        let needed = line.len() as u64 + 1;
        if self.size > 0 && self.size + needed > MAX_CSV_BYTES {
            if let Err(err) = self.rotate() {
                self.error = Some(err);
                return;
            }
            if self.ensure_open().is_err() {
                return;
            }
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if writeln!(file, "{line}").is_err() || file.flush().is_err() {
            self.error = Some("no pude escribir telemetry.csv (¿disco lleno o bloqueado?)".into());
            self.file = None;
            return;
        }
        self.size += needed;
    }

    fn ensure_open(&mut self) -> Result<(), ()> {
        if self.file.is_some() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            if fs::create_dir_all(parent).is_err() {
                self.error = Some(format!("no pude crear {parent:?}"));
                return Err(());
            }
        }
        let existing = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                let mut size = existing;
                if existing == 0 {
                    match writeln!(file, "{CSV_HEADER}") {
                        Ok(()) => size = CSV_HEADER.len() as u64 + 1,
                        Err(err) => {
                            self.error = Some(format!("no pude escribir el encabezado: {err}"));
                            return Err(());
                        }
                    }
                }
                self.file = Some(file);
                self.size = size;
                Ok(())
            }
            Err(err) => {
                self.error = Some(format!("no pude abrir telemetry.csv: {err}"));
                Err(())
            }
        }
    }

    fn rotate(&mut self) -> Result<(), String> {
        self.file = None;
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        let stamp = iso_utc(now_epoch()).replace([':', 'T', 'Z'], "");
        let mut archive = dir.join(format!("telemetry-{stamp}.csv"));
        let mut counter = 1;
        while archive.exists() {
            archive = dir.join(format!("telemetry-{stamp}-{counter}.csv"));
            counter += 1;
        }
        fs::rename(&self.path, &archive)
            .map_err(|err| format!("no pude rotar telemetry.csv: {err}"))?;
        self.size = 0;

        let mut archives: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|err| format!("no pude listar {dir:?}: {err}"))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("telemetry-") && name.ends_with(".csv"))
                    .unwrap_or(false)
            })
            .collect();
        archives.sort();
        while archives.len() > MAX_ARCHIVES {
            if let Some(old) = archives.first().cloned() {
                let _ = fs::remove_file(old);
                archives.remove(0);
            } else {
                break;
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ history

/// Ventana pedida por la UI: `minutes` y/o `limit` (últimos N puntos).
struct Window {
    minutes: f64,
    limit: Option<usize>,
}

impl Window {
    fn parse(url: &str) -> Result<Self, String> {
        let minutes_raw = crate::api::query_param(url, "minutes");
        let limit_raw = crate::api::query_param(url, "limit");
        let minutes = match minutes_raw {
            Some(raw) => {
                let value: f64 = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("minutes inválido: {raw} (esperaba minutos, ej. 60)"))?;
                if !value.is_finite() || value <= 0.0 || value > MAX_WINDOW_MIN {
                    return Err(format!(
                        "minutes fuera de rango: {raw} (acepto 0 < minutes ≤ {MAX_WINDOW_MIN})"
                    ));
                }
                value
            }
            // `limit` sin `minutes`: los últimos N puntos de todo el buffer.
            None if limit_raw.is_some() => MAX_WINDOW_MIN,
            None => DRAW_WINDOW_MIN,
        };
        let limit = match limit_raw {
            Some(raw) => {
                let value: usize = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("limit inválido: {raw} (esperaba un entero)"))?;
                if value == 0 || value > RING_CAP {
                    return Err(format!(
                        "limit fuera de rango: {raw} (acepto 1..{RING_CAP})"
                    ));
                }
                Some(value)
            }
            None => None,
        };
        Ok(Self { minutes, limit })
    }
}

/// GET /api/legion/history?minutes=60[&limit=N]
pub fn history(url: &str) -> Result<Value, String> {
    ensure_started();
    let window = Window::parse(url)?;
    let now = now_epoch();
    let (selected, meta) = {
        let store = lock_store();
        let cutoff = now - window.minutes * 60.0;
        let mut selected: Vec<Sample> = store
            .samples
            .iter()
            .filter(|sample| sample.ts >= cutoff)
            .cloned()
            .collect();
        if let Some(limit) = window.limit {
            if selected.len() > limit {
                selected = selected.split_off(selected.len() - limit);
            }
        }
        let meta = json!({
            "available": store.available,
            "admin": store.admin,
            "last_error": store.last_error,
            "last_at": store.last_at,
            "buffer_samples": store.samples.len(),
            "buffer_capacity": RING_CAP,
            "retention_hours": RETENTION_S / 3600.0,
            "samples_total": store.samples_total,
            "csv": store.path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "csv_error": store.csv_error,
        });
        (selected, meta)
    };

    let summary = summarize(&selected);
    let mut payload = json!({
        "ok": true,
        "available": meta["available"],
        "sampling": {
            "interval_s": SAMPLE_INTERVAL.as_secs(),
            "running": STARTED.get().is_some(),
            "last_at": meta["last_at"],
            "last_error": meta["last_error"],
            "buffer_samples": meta["buffer_samples"],
            "buffer_capacity": meta["buffer_capacity"],
            "retention_hours": meta["retention_hours"],
            "samples_total": meta["samples_total"],
            "csv": meta["csv"],
            "csv_error": meta["csv_error"],
            "admin": meta["admin"],
        },
        "window": {
            "minutes": window.minutes,
            "limit": window.limit,
            "from": now - window.minutes * 60.0,
            "to": now,
        },
        "samples": selected.iter().map(Sample::to_json).collect::<Vec<Value>>(),
        "summary": summary,
    });
    if selected.is_empty() {
        let note = match meta["available"].as_bool() {
            Some(true) => "todavía no hay muestras en la ventana pedida",
            Some(false) => {
                "sin muestras: WMI Legion no está disponible (sin admin o sin clases Lenovo)"
            }
            // available null = primera muestra todavía en camino
            None => "muestreo recién arrancado: la primera muestra llega en ~2 s",
        };
        if let Some(object) = payload.as_object_mut() {
            object.insert("note".into(), json!(note));
        }
    }
    Ok(payload)
}

/// GET /api/legion/export.csv?minutes=...[&limit=N] — todo el buffer por
/// defecto, en el mismo formato que el CSV en disco.
pub fn export_csv(url: &str) -> Result<String, String> {
    ensure_started();
    let has_filter = url.contains("minutes=") || url.contains("limit=");
    let selected: Vec<Sample> = {
        let store = lock_store();
        let mut samples: Vec<Sample> = if has_filter {
            let window = Window::parse(url)?;
            let cutoff = now_epoch() - window.minutes * 60.0;
            store
                .samples
                .iter()
                .filter(|sample| sample.ts >= cutoff)
                .cloned()
                .collect()
        } else {
            store.samples.iter().cloned().collect()
        };
        if has_filter {
            if let Some(limit) = Window::parse(url)?.limit {
                if samples.len() > limit {
                    samples = samples.split_off(samples.len() - limit);
                }
            }
        }
        samples
    };
    let mut out = String::with_capacity(selected.len() * 80 + CSV_HEADER.len() + 1);
    out.push_str(CSV_HEADER);
    out.push('\n');
    for sample in &selected {
        out.push_str(&sample.to_csv());
        out.push('\n');
    }
    Ok(out)
}

fn summarize(samples: &[Sample]) -> Value {
    let temps = |value: &Sample| value.cpu_temp;
    let gpu_temps = |value: &Sample| value.gpu_temp;
    let fan1 = |value: &Sample| value.fan1_rpm.map(|rpm| rpm as f64);
    let fan2 = |value: &Sample| value.fan2_rpm.map(|rpm| rpm as f64);
    let power = |value: &Sample| value.gpu_power_w;
    let clock = |value: &Sample| value.gpu_clock_mhz.map(|mhz| mhz as f64);

    let first_ts = samples.first().map(|sample| sample.ts).unwrap_or(0.0);
    let last_ts = samples.last().map(|sample| sample.ts).unwrap_or(0.0);
    let intervals: Vec<f64> = samples
        .windows(2)
        .map(|pair| pair[1].ts - pair[0].ts)
        .filter(|dt| *dt > 0.0)
        .collect();

    json!({
        "count": samples.len(),
        "first_ts": if samples.is_empty() { Value::Null } else { json!(round(first_ts, 3)) },
        "last_ts": if samples.is_empty() { Value::Null } else { json!(round(last_ts, 3)) },
        "first_time": if samples.is_empty() { Value::Null } else { json!(iso_utc(first_ts)) },
        "last_time": if samples.is_empty() { Value::Null } else { json!(iso_utc(last_ts)) },
        "span_s": round((last_ts - first_ts).max(0.0), 1),
        "interval_s": interval_stats(&intervals),
        "cpu_temp_c": stats(samples.iter().filter_map(temps), 1),
        "gpu_temp_c": stats(samples.iter().filter_map(gpu_temps), 1),
        "fan1_rpm": stats(samples.iter().filter_map(fan1), 0),
        "fan2_rpm": stats(samples.iter().filter_map(fan2), 0),
        "gpu_power_w": stats(samples.iter().filter_map(power), 2),
        "gpu_clock_mhz": stats(samples.iter().filter_map(clock), 0),
        "hot": hot_json(samples, &intervals),
        "thermal_modes": thermal_modes_json(samples),
        "fan_curve": fan_curve_json(samples),
        "fan_effect": fan_effect_json(samples, &intervals),
    })
}

fn stats(values: impl Iterator<Item = f64>, decimals: u32) -> Value {
    let mut count = 0u64;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut last = None;
    for value in values.filter(|value| value.is_finite()) {
        count += 1;
        min = min.min(value);
        max = max.max(value);
        sum += value;
        last = Some(value);
    }
    if count == 0 {
        return json!({"min": null, "max": null, "avg": null, "last": null, "count": 0});
    }
    json!({
        "min": round(min, decimals),
        "max": round(max, decimals),
        "avg": round(sum / count as f64, decimals),
        "last": last.map(|value| round(value, decimals)),
        "count": count,
    })
}

fn interval_stats(intervals: &[f64]) -> Value {
    if intervals.is_empty() {
        return json!({"avg": null, "min": null, "max": null});
    }
    let sum: f64 = intervals.iter().sum();
    let min = intervals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = intervals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    json!({
        "avg": round(sum / intervals.len() as f64, 2),
        "min": round(min, 2),
        "max": round(max, 2),
    })
}

/// Tiempo por encima del umbral: cada muestra caliente "vale" el intervalo
/// hasta la siguiente, con tope de 60 s para no inflar huecos (equipo dormido).
fn hot_json(samples: &[Sample], intervals: &[f64]) -> Value {
    let median = median(intervals).unwrap_or(SAMPLE_INTERVAL.as_secs_f64());
    let step = |index: usize| -> f64 {
        samples
            .get(index + 1)
            .map(|next| (next.ts - samples[index].ts).clamp(0.0, 60.0))
            .unwrap_or(median.min(60.0))
    };
    let mut cpu_s = 0.0;
    let mut gpu_s = 0.0;
    let mut any_s = 0.0;
    let mut cpu_samples = 0u64;
    let mut gpu_samples = 0u64;
    let mut any_samples = 0u64;
    for (index, sample) in samples.iter().enumerate() {
        let cpu_hot = sample.cpu_temp.map(|temp| temp >= HOT_C).unwrap_or(false);
        let gpu_hot = sample.gpu_temp.map(|temp| temp >= HOT_C).unwrap_or(false);
        if cpu_hot {
            cpu_samples += 1;
            cpu_s += step(index);
        }
        if gpu_hot {
            gpu_samples += 1;
            gpu_s += step(index);
        }
        if cpu_hot || gpu_hot {
            any_samples += 1;
            any_s += step(index);
        }
    }
    json!({
        "threshold_c": HOT_C,
        "cpu_samples": cpu_samples,
        "gpu_samples": gpu_samples,
        "any_samples": any_samples,
        "cpu_s": round(cpu_s, 1),
        "gpu_s": round(gpu_s, 1),
        "any_s": round(any_s, 1),
        "cpu_min": round(cpu_s / 60.0, 2),
        "gpu_min": round(gpu_s / 60.0, 2),
        "any_min": round(any_s / 60.0, 2),
    })
}

fn thermal_modes_json(samples: &[Sample]) -> Value {
    let mut counts: Vec<(String, u64)> = Vec::new();
    for sample in samples {
        let key = sample
            .thermal_mode
            .map(crate::legion::thermal_key)
            .unwrap_or_else(|| "unknown".to_string());
        match counts.iter_mut().find(|(name, _)| *name == key) {
            Some((_, count)) => *count += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut object = serde_json::Map::new();
    for (key, count) in counts {
        object.insert(key, json!(count));
    }
    Value::Object(object)
}

/// Curva fans→temperatura: 6 bandas de RPM (fan 1; si no hay, fan 2) con la
/// temperatura media de CPU en cada banda. Es observación, no causalidad: el
/// fan sube porque la temperatura sube.
fn fan_curve_json(samples: &[Sample]) -> Value {
    let rpm: Vec<f64> = samples
        .iter()
        .filter_map(|sample| {
            sample
                .fan1_rpm
                .or(sample.fan2_rpm)
                .map(|value| value as f64)
        })
        .collect();
    if rpm.len() < 4 {
        return json!([]);
    }
    let min = rpm.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = rpm.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let bins = 6usize;
    let width = if max > min {
        (max - min) / bins as f64
    } else {
        0.0
    };
    let mut buckets: Vec<(u64, f64, f64, f64)> = vec![(0, 0.0, 0.0, f64::NEG_INFINITY); bins];
    for sample in samples {
        let Some(value) = sample.fan1_rpm.or(sample.fan2_rpm) else {
            continue;
        };
        // Sin temperatura de CPU no hay punto que promediar: no se inventa 0.
        let Some(cpu) = sample.cpu_temp else {
            continue;
        };
        let index = if width > 0.0 {
            (((value as f64 - min) / width).floor() as usize).min(bins - 1)
        } else {
            0
        };
        let bucket = &mut buckets[index];
        bucket.0 += 1;
        bucket.1 += value as f64;
        bucket.2 += cpu;
        bucket.3 = bucket.3.max(cpu);
    }
    buckets
        .into_iter()
        .filter(|(count, _, _, _)| *count > 0)
        .map(|(count, sum_rpm, sum_cpu, max_cpu)| {
            json!({
                "fan_rpm_avg": (sum_rpm / count as f64).round(),
                "samples": count,
                "cpu_avg_c": round(sum_cpu / count as f64, 1),
                "cpu_max_c": if max_cpu.is_finite() { json!(round(max_cpu, 1)) } else { Value::Null },
            })
        })
        .collect::<Vec<Value>>()
        .into()
}

/// ¿Los fans acompañan una baja de temperatura? Correlación entre el RPM de la
/// muestra y la variación de temperatura de CPU hasta la siguiente muestra
/// (grados por minuto). Negativa = más RPM, temperatura que baja más rápido.
/// Confundida por la inercia térmica: sirve como pista, no como prueba.
fn fan_effect_json(samples: &[Sample], _intervals: &[f64]) -> Value {
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for pair in samples.windows(2) {
        let (current, next) = (&pair[0], &pair[1]);
        let Some(rpm) = current.fan1_rpm.or(current.fan2_rpm) else {
            continue;
        };
        let (Some(temp), Some(next_temp)) = (current.cpu_temp, next.cpu_temp) else {
            continue;
        };
        let dt = next.ts - current.ts;
        if dt <= 0.0 {
            continue;
        }
        pairs.push((rpm as f64, (next_temp - temp) / dt * 60.0));
    }
    if pairs.len() < 20 {
        return json!({
            "pairs": pairs.len(),
            "corr_rpm_vs_dtemp_per_min": null,
            "avg_dtemp_low_rpm": null,
            "avg_dtemp_high_rpm": null,
            "rpm_split": null,
        });
    }
    let corr = pearson(&pairs);
    let mut sorted: Vec<f64> = pairs.iter().map(|(rpm, _)| *rpm).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let split = sorted[sorted.len() / 2 - 1];
    let avg = |low: bool| {
        let values: Vec<f64> = pairs
            .iter()
            .filter(|(rpm, _)| (*rpm <= split) == low)
            .map(|(_, delta)| *delta)
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(round(values.iter().sum::<f64>() / values.len() as f64, 2))
        }
    };
    json!({
        "pairs": pairs.len(),
        "corr_rpm_vs_dtemp_per_min": corr.map(|value| round(value, 3)),
        "avg_dtemp_low_rpm": avg(true),
        "avg_dtemp_high_rpm": avg(false),
        "rpm_split": round(split, 0),
    })
}

fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    let n = pairs.len() as f64;
    if n < 2.0 {
        return None;
    }
    let mean_x = pairs.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = pairs.iter().map(|(_, y)| y).sum::<f64>() / n;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (x, y) in pairs {
        let dx = x - mean_x;
        let dy = y - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    if var_x <= f64::EPSILON || var_y <= f64::EPSILON {
        return None;
    }
    Some(cov / (var_x.sqrt() * var_y.sqrt()))
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(sorted[sorted.len() / 2])
}

// ------------------------------------------------------------------ formato

pub(crate) fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn round(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

fn format_number(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}")
}

fn opt_number(value: Option<f64>, decimals: usize) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format_number(value, decimals))
        .unwrap_or_default()
}

fn opt_int(value: Option<i64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn parse_f64(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_u32(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok()
}

/// Epoch → `YYYY-MM-DDTHH:MM:SSZ` (UTC), sin dependencias nuevas.
pub(crate) fn iso_utc(ts: f64) -> String {
    if !ts.is_finite() || ts <= 0.0 {
        return String::new();
    }
    let secs = ts.floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant, `civil_from_days`: días desde 1970-01-01 → fecha gregoriana.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: f64, cpu: f64, gpu: f64, fan: u32) -> Sample {
        Sample {
            ts,
            cpu_temp: Some(cpu),
            gpu_temp: Some(gpu),
            fan1_rpm: Some(fan),
            fan2_rpm: Some(fan),
            gpu_power_w: Some(35.0),
            gpu_clock_mhz: Some(1200),
            thermal_mode: Some(2),
            cpu_limit_w: Some(45.0),
        }
    }

    #[test]
    fn formatea_iso_utc_sin_dependencias() {
        assert_eq!(iso_utc(0.0), "");
        assert_eq!(iso_utc(1.0), "1970-01-01T00:00:01Z");
        assert_eq!(iso_utc(951_782_400.0), "2000-02-29T00:00:00Z");
        // 2026-11-13T18:00:00Z — fecha de esta sesión.
        assert_eq!(iso_utc(1_794_592_800.0), "2026-11-13T18:00:00Z");
    }

    #[test]
    fn csv_ida_y_vuelta() {
        let original = sample(1_794_592_800.5, 71.5, 62.0, 2400);
        let line = original.to_csv();
        let parsed = Sample::from_csv(&line).expect("parsear csv");
        assert_eq!(parsed, original);
        assert!(line.contains("2026-11-13T18:00:00Z"));

        // Los nulls quedan como columnas vacías y vuelven como None.
        let empty = Sample {
            ts: 1_794_592_802.0,
            cpu_temp: None,
            gpu_temp: None,
            fan1_rpm: None,
            fan2_rpm: None,
            gpu_power_w: None,
            gpu_clock_mhz: None,
            thermal_mode: None,
            cpu_limit_w: None,
        };
        assert_eq!(Sample::from_csv(&empty.to_csv()).unwrap(), empty);
        // Línea basura o header: se ignora.
        assert!(Sample::from_csv("ts,time_utc,cpu_temp_c").is_none());
        assert!(Sample::from_csv("").is_none());
    }

    #[test]
    fn el_ring_recorta_por_tiempo_y_por_capacidad() {
        let dir = std::env::temp_dir().join(format!("domador-tel-ring-{}", std::process::id()));
        let path = dir.join("telemetry.csv");
        let mut store = Store::open(path);
        store.samples.clear();
        // 3 h a 2 s: 5400 muestras, quedan todas.
        let base = now_epoch() - 10_800.0;
        for index in 0..5400 {
            store.push(sample(base + index as f64 * 2.0, 70.0, 60.0, 2000));
        }
        assert_eq!(store.samples.len(), 5400);
        // Una muestra más nueva que 24 h recorta las viejas.
        store.push(sample(now_epoch(), 70.0, 60.0, 2000));
        assert!(store
            .samples
            .iter()
            .all(|s| s.ts >= now_epoch() - RETENTION_S - 5.0));
        // El tope duro también se respeta.
        store.samples.clear();
        for index in 0..(RING_CAP + 10) {
            store.push(sample(
                now_epoch() - 60_000.0 + index as f64 * 0.5,
                70.0,
                60.0,
                2000,
            ));
        }
        assert!(store.samples.len() <= RING_CAP);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_temperaturas_fans_y_tiempo_caliente() {
        let mut samples = Vec::new();
        // 10 muestras a 2 s: 6 calientes (94 °C) y 4 frías.
        for index in 0..10 {
            let cpu = if index < 6 { 94.0 } else { 80.0 };
            samples.push(sample(
                index as f64 * 2.0,
                cpu,
                70.0,
                if index < 6 { 3000 } else { 1800 },
            ));
        }
        let summary = summarize(&samples);
        assert_eq!(summary["count"], 10);
        assert_eq!(summary["cpu_temp_c"]["max"], 94.0);
        assert_eq!(summary["cpu_temp_c"]["min"], 80.0);
        assert_eq!(summary["cpu_temp_c"]["count"], 10);
        // 6 muestras calientes: 5 intervalos de 2 s + la última con la mediana.
        assert_eq!(summary["hot"]["cpu_samples"], 6);
        assert_eq!(summary["hot"]["cpu_s"], 12.0);
        assert_eq!(summary["hot"]["gpu_samples"], 0);
        assert_eq!(summary["thermal_modes"]["balance"], 10);
        assert!(summary["fan_curve"].as_array().unwrap().len() >= 2);
        // Sin GPU caliente el campo existe y es null, no ausente.
        assert!(summary.get("gpu_power_w").is_some());
    }

    #[test]
    fn la_curva_de_fans_promedia_por_banda() {
        let mut samples = Vec::new();
        for index in 0..40 {
            let fan = 1000 + index as u32 * 50;
            let cpu = if index < 20 { 88.0 } else { 72.0 };
            samples.push(sample(index as f64 * 2.0, cpu, 65.0, fan));
        }
        let curve = fan_curve_json(&samples);
        let curve = curve.as_array().unwrap();
        assert_eq!(curve.len(), 6);
        let total: u64 = curve
            .iter()
            .map(|bin| bin["samples"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 40);
        // Bandas más frías (más RPM) deben promediar menos que las primeras.
        let first = curve.first().unwrap()["cpu_avg_c"].as_f64().unwrap();
        let last = curve.last().unwrap()["cpu_avg_c"].as_f64().unwrap();
        assert!(last < first, "curva {curve:?}");
    }

    #[test]
    fn correlacion_negativa_cuando_mas_rpm_baja_la_temperatura() {
        let mut samples = Vec::new();
        for index in 0..60 {
            // Alterna RPM alto con caída de temperatura, bajo con suba.
            let hot = index % 2 == 0;
            let fan = if hot { 3200 } else { 2000 };
            let cpu = if hot { 85.0 } else { 78.0 };
            samples.push(sample(index as f64 * 2.0, cpu, 70.0, fan));
        }
        let effect = fan_effect_json(&samples, &[]);
        let corr = effect["corr_rpm_vs_dtemp_per_min"].as_f64().unwrap();
        assert!(corr < -0.5, "correlación {corr}");
        assert!(
            effect["avg_dtemp_high_rpm"].as_f64().unwrap()
                < effect["avg_dtemp_low_rpm"].as_f64().unwrap()
        );
    }

    #[test]
    fn valida_la_ventana_pedida() {
        assert!(Window::parse("/api/legion/history").is_ok());
        assert_eq!(Window::parse("/api/legion/history").unwrap().minutes, 60.0);
        assert_eq!(
            Window::parse("/api/legion/history?minutes=30")
                .unwrap()
                .minutes,
            30.0
        );
        assert!(Window::parse("/api/legion/history?minutes=0").is_err());
        assert!(Window::parse("/api/legion/history?minutes=2000").is_err());
        assert!(Window::parse("/api/legion/history?minutes=abc").is_err());
        assert!(Window::parse("/api/legion/history?limit=0").is_err());
        assert!(Window::parse("/api/legion/history?limit=abc").is_err());
        assert_eq!(
            Window::parse("/api/legion/history?limit=100")
                .unwrap()
                .limit,
            Some(100)
        );
        // `limit` solo barre todo el buffer de 24 h.
        assert_eq!(
            Window::parse("/api/legion/history?limit=100")
                .unwrap()
                .minutes,
            MAX_WINDOW_MIN
        );
    }

    #[test]
    fn csv_rota_y_conserva_solo_dos_archivos() {
        let dir = std::env::temp_dir().join(format!("domador-tel-rot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("telemetry.csv");
        let mut sink = CsvSink::new(path.clone());
        sink.append("1763056800,2026-11-13T18:00:00Z,70,,,,,,2,");
        assert!(path.exists());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("ts,time_utc,"), "encabezado: {text:?}");

        // Forzar 4 rotaciones: sólo quedan las 2 más nuevas.
        for _ in 0..4 {
            sink.size = MAX_CSV_BYTES;
            sink.append("1763056802,2026-11-13T18:00:02Z,71,,,,,,2,");
            std::thread::sleep(Duration::from_millis(20));
        }
        let archives: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name != "telemetry.csv")
            .collect();
        assert_eq!(archives.len(), MAX_ARCHIVES, "archivos: {archives:?}");
        let _ = fs::remove_dir_all(dir);
    }
}
