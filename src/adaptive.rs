//! Control adaptativo de temperatura para Legion.
//!
//! Evalúa las mismas muestras de la telemetría (cada 2 s) y actúa **sólo** con
//! admin y WMI disponibles. La escalera es deliberadamente corta y con
//! histéresis, dwell y cooldown para que no haya flapping:
//!
//! 1. `fans`: al pasar `hot_c` sostenido `dwell_s` → `Fan_Set_FullSpeed(true)`.
//! 2. `fans_cpu`: si sigue en `critical_c` después de otro dwell/cooldown →
//!    baja el límite largo de CPU un paso del firmware (nunca por debajo del
//!    mínimo que informa la BIOS).
//! 3. Al bajar a `hot_c - hysteresis_c` sostenido → restaura CPU primero y los
//!    fans después, un paso por cooldown.
//!
//! Nunca se toca el modo térmico, ni se escribe si falta admin/WMI, ni se
//! inventa una lectura: sin temperatura no se decide nada. El estado y el
//! baseline (full speed previo, límite de CPU previo) se persisten en
//! `adaptive.json` para poder restaurar incluso después de un reinicio.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config;
use crate::legion::{self, Error};
use crate::telemetry::{iso_utc, now_epoch, Sample};

const LOG_CAP: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Sólo fans: full speed cuando hace calor.
    Fans,
    /// Fans y, si el calor sigue en crítico, un paso menos de límite de CPU.
    FansCpu,
    /// Máximo rendimiento seguro: modo Performance con cargador, fans a fondo
    /// **antes** de que caliente (mira la pendiente y la potencia de la GPU,
    /// no sólo el umbral) y freno de CPU sólo en crítico sostenido.
    Max,
}

impl ActionKind {
    fn key(self) -> &'static str {
        match self {
            ActionKind::Fans => "fans",
            ActionKind::FansCpu => "fans_cpu",
            ActionKind::Max => "max",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "fans" => Some(ActionKind::Fans),
            "fans_cpu" | "fans+cpu" | "cpu" => Some(ActionKind::FansCpu),
            "max" | "maximo" | "máximo" => Some(ActionKind::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// Temperatura a la que se considera "frío" (objetivo).
    pub target_c: f64,
    /// Umbral de entrada: a partir de acá se encienden los fans.
    pub hot_c: f64,
    /// Umbral para bajar el límite de CPU (sólo `fans_cpu`).
    pub critical_c: f64,
    /// Banda muerta: se sale en `hot_c - hysteresis_c`.
    pub hysteresis_c: f64,
    /// Tiempo que la condición tiene que sostenerse antes de actuar.
    pub dwell_s: f64,
    /// Tiempo mínimo entre dos escrituras.
    pub cooldown_s: f64,
    pub action: ActionKind,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            target_c: 82.0,
            hot_c: 85.0,
            critical_c: 92.0,
            hysteresis_c: 3.0,
            dwell_s: 10.0,
            cooldown_s: 30.0,
            action: ActionKind::Fans,
        }
    }
}

impl Config {
    fn exit_c(&self) -> f64 {
        // Nunca se apaga por encima del objetivo: si `target_c` está más cerca
        // de `hot_c` que la histéresis configurada, manda el objetivo.
        (self.hot_c - self.hysteresis_c).max(self.target_c)
    }

    fn validate(&self) -> Result<(), String> {
        let finite = |value: f64| value.is_finite();
        if !finite(self.target_c) || !(40.0..=100.0).contains(&self.target_c) {
            return Err("target_c debe estar entre 40 y 100 °C".into());
        }
        if !finite(self.hot_c) || !(45.0..=105.0).contains(&self.hot_c) {
            return Err("hot_c debe estar entre 45 y 105 °C".into());
        }
        if self.target_c >= self.hot_c {
            return Err(format!(
                "target_c ({}) tiene que ser menor que hot_c ({})",
                self.target_c, self.hot_c
            ));
        }
        if !finite(self.critical_c) || !(self.hot_c..=110.0).contains(&self.critical_c) {
            return Err(format!(
                "critical_c debe estar entre hot_c ({}) y 110 °C",
                self.hot_c
            ));
        }
        if !finite(self.hysteresis_c) || !(1.0..=20.0).contains(&self.hysteresis_c) {
            return Err("hysteresis_c debe estar entre 1 y 20 °C".into());
        }
        if !finite(self.dwell_s) || !(2.0..=600.0).contains(&self.dwell_s) {
            return Err("dwell_s debe estar entre 2 y 600 segundos".into());
        }
        if !finite(self.cooldown_s) || !(5.0..=3600.0).contains(&self.cooldown_s) {
            return Err("cooldown_s debe estar entre 5 y 3600 segundos".into());
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "target_c": self.target_c,
            "hot_c": self.hot_c,
            "critical_c": self.critical_c,
            "hysteresis_c": self.hysteresis_c,
            "exit_c": self.exit_c(),
            "dwell_s": self.dwell_s,
            "cooldown_s": self.cooldown_s,
            "action": self.action.key(),
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Baseline {
    full_speed: Option<bool>,
    cpu_long: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    config: Config,
    baseline: Option<Baseline>,
    level: u8,
    last_action_at: f64,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            config: Config::default(),
            baseline: None,
            level: 0,
            last_action_at: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    FansOn,
    CpuDown,
    CpuRestore,
    FansOff,
    ModePerf,
}

impl Intent {
    fn key(self) -> &'static str {
        match self {
            Intent::FansOn => "fans_on",
            Intent::CpuDown => "cpu_cap",
            Intent::CpuRestore => "cpu_restore",
            Intent::FansOff => "fans_off",
            Intent::ModePerf => "mode_performance",
        }
    }
}

struct State {
    cfg: Config,
    level: u8,
    baseline: Option<Baseline>,
    hot_since: Option<f64>,
    cool_since: Option<f64>,
    last_action: Option<Value>,
    last_action_at: f64,
    log: VecDeque<Value>,
    writes: u64,
    failures: u64,
    last_error: Option<String>,
    last_temp: Option<f64>,
    last_available: Option<bool>,
    last_admin: bool,
    busy: bool,
    /// Últimos ~30 s de temperatura para medir la pendiente (°C/s).
    history: VecDeque<(f64, f64)>,
    last_power: Option<f64>,
    last_ac: Option<bool>,
    last_thermal: Option<u8>,
    power_since: Option<f64>,
    /// Qué está haciendo el control y por qué, en castellano llano.
    explain: String,
}

/// Parámetros del modo Máximo. Calibrados para la Legion 5 Pro Gen 6: la
/// RTX 3070 Laptop empieza a recortar clocks cerca de 87 °C y el 5800H a 95 °C.
const MAX_SLOPE_C_PER_S: f64 = 0.4;
const MAX_POWER_W: f64 = 85.0;
const MAX_IDLE_POWER_W: f64 = 55.0;
const MAX_REST_S: f64 = 120.0;

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn lock() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| Mutex::new(State::load()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn file_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOMADOR_ADAPTIVE_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    match config::default_path().parent() {
        Some(dir) => dir.join("adaptive.json"),
        None => PathBuf::from("adaptive.json"),
    }
}

impl State {
    fn load() -> Self {
        let mut cfg = Config::default();
        let mut baseline = None;
        let mut level = 0;
        let mut last_action_at = 0.0;
        if let Ok(text) = fs::read_to_string(file_path()) {
            if let Ok(saved) = serde_json::from_str::<Persisted>(&text) {
                if saved.config.validate().is_ok() {
                    cfg = saved.config;
                }
                baseline = saved.baseline;
                level = saved.level.min(2);
                last_action_at = saved.last_action_at;
            }
        }
        Self {
            cfg,
            level,
            baseline,
            hot_since: None,
            cool_since: None,
            last_action: None,
            last_action_at,
            log: VecDeque::new(),
            writes: 0,
            failures: 0,
            last_error: None,
            last_temp: None,
            last_available: None,
            last_admin: crate::win::is_admin(),
            busy: false,
            history: VecDeque::new(),
            last_power: None,
            last_ac: None,
            last_thermal: None,
            power_since: None,
            explain: String::new(),
        }
    }

    fn save(&self) {
        let path = file_path();
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let persisted = Persisted {
            config: self.cfg.clone(),
            baseline: self.baseline.clone(),
            level: self.level,
            last_action_at: self.last_action_at,
        };
        let Ok(text) = serde_json::to_string_pretty(&persisted) else {
            return;
        };
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }

    fn cooldown_ok(&self, now: f64) -> bool {
        self.last_action_at <= 0.0 || now - self.last_action_at >= self.cfg.cooldown_s
    }

    fn dwell_elapsed(&self, since: Option<f64>, now: f64) -> bool {
        since
            .map(|start| now - start >= self.cfg.dwell_s)
            .unwrap_or(false)
    }

    /// Máquina de decisión pura (sin WMI): la usan `observe` y los tests.
    fn decide(
        &mut self,
        now: f64,
        temp: f64,
        admin: bool,
        available: bool,
    ) -> Option<(Intent, &'static str)> {
        if !self.cfg.enabled {
            self.hot_since = None;
            self.cool_since = None;
            return None;
        }
        if !admin || !available {
            let message = if !admin {
                "control adaptativo activo pero sin admin: no toco el EC"
            } else {
                "control adaptativo activo pero WMI Legion no responde: no toco el EC"
            };
            if self.last_error.as_deref() != Some(message) {
                self.last_error = Some(message.to_string());
            }
            return None;
        }

        let hot = temp >= self.cfg.hot_c;
        let cool = temp <= self.cfg.exit_c();
        if hot {
            if self.hot_since.is_none() {
                self.hot_since = Some(now);
            }
            self.cool_since = None;
        } else {
            self.hot_since = None;
        }
        if cool {
            if self.cool_since.is_none() {
                self.cool_since = Some(now);
            }
        } else {
            self.cool_since = None;
        }

        if !self.cooldown_ok(now) {
            return None;
        }
        if hot && self.dwell_elapsed(self.hot_since, now) {
            match self.level {
                0 => return Some((Intent::FansOn, "hot")),
                1 if self.cfg.action == ActionKind::FansCpu && temp >= self.cfg.critical_c => {
                    return Some((Intent::CpuDown, "critical"))
                }
                _ => {}
            }
        }
        if cool && self.dwell_elapsed(self.cool_since, now) {
            match self.level {
                2 => return Some((Intent::CpuRestore, "cool")),
                1 => return Some((Intent::FansOff, "cool")),
                _ => {}
            }
        }
        None
    }

    /// Pendiente de temperatura (°C/s) en los últimos ~10 s.
    fn slope(&self, now: f64) -> Option<f64> {
        let (_, current) = *self.history.back()?;
        let (t0, v0) = self
            .history
            .iter()
            .find(|(ts, _)| now - ts <= 12.0)
            .copied()?;
        let dt = now - t0;
        if dt < 6.0 {
            return None;
        }
        Some((current - v0) / dt)
    }

    /// Modo Máximo: anticipa el calor en vez de reaccionar tarde.
    ///
    /// - Con cargador y sin modo Performance → pone Performance (GPU a 140 W).
    /// - Fans a fondo si: la temperatura sube rápido (pendiente), la GPU tira
    ///   mucha potencia sostenida, o se pasa `hot_c` sostenido.
    /// - En `critical_c` sostenido (fans ya a fondo) baja un paso el CPU.
    /// - Vuelve atrás sólo con la máquina en reposo `MAX_REST_S` seguidos.
    fn decide_max(
        &mut self,
        now: f64,
        temp: f64,
        power: Option<f64>,
        ac: bool,
        thermal: Option<u8>,
    ) -> Option<(Intent, &'static str)> {
        self.history.push_back((now, temp));
        while self
            .history
            .front()
            .map(|(ts, _)| now - ts > 30.0)
            .unwrap_or(false)
        {
            self.history.pop_front();
        }
        let slope = self.slope(now).unwrap_or(0.0);
        let heavy = power.map(|w| w >= MAX_POWER_W).unwrap_or(false);
        if heavy {
            if self.power_since.is_none() {
                self.power_since = Some(now);
            }
        } else {
            self.power_since = None;
        }
        let hot = temp >= self.cfg.hot_c;
        let critical = temp >= self.cfg.critical_c;
        let idle = power.map(|w| w < MAX_IDLE_POWER_W).unwrap_or(true);
        let cool = temp <= self.cfg.exit_c() && idle && slope <= 0.05;
        if hot {
            self.hot_since.get_or_insert(now);
        } else {
            self.hot_since = None;
        }
        if cool {
            self.cool_since.get_or_insert(now);
        } else {
            self.cool_since = None;
        }

        // Explicación del estado actual (aunque no haya acción).
        self.explain = if !ac {
            format!(
                "Con batería: no fuerzo Performance (la GPU igual queda limitada y la batería dura minutos). {temp:.0} °C."
            )
        } else if self.level >= 2 {
            format!("Freno de emergencia: {temp:.0} °C con fans a fondo. CPU un paso abajo hasta que enfríe.")
        } else if self.level == 1 {
            format!(
                "Fans a fondo · {temp:.0} °C · GPU {} · pendiente {slope:+.2} °C/s. Vuelvo a auto tras {:.0} s de reposo.",
                power.map(|w| format!("{w:.0} W")).unwrap_or_else(|| "—".into()),
                MAX_REST_S
            )
        } else {
            format!(
                "Listo y mirando: {temp:.0} °C, GPU {}. Si la temperatura sube rápido o la GPU pasa {MAX_POWER_W:.0} W, pongo los fans a fondo antes de que caliente.",
                power.map(|w| format!("{w:.0} W")).unwrap_or_else(|| "—".into())
            )
        };

        let cooldown_ok = self.cooldown_ok(now);
        // 1) Enchufada → modo Performance (lo único que destraba los 140 W de la GPU).
        if ac && thermal.is_some() && thermal != Some(3) && cooldown_ok {
            return Some((Intent::ModePerf, "enchufada"));
        }
        // 2) Subir: anticipar es inmediato; potencia y calor piden sostenerse.
        if self.level == 0 {
            if slope >= MAX_SLOPE_C_PER_S && temp >= self.cfg.target_c - 15.0 {
                return Some((Intent::FansOn, "anticipa"));
            }
            if cooldown_ok && self.dwell_elapsed(self.power_since, now) {
                return Some((Intent::FansOn, "carga"));
            }
            if cooldown_ok && hot && self.dwell_elapsed(self.hot_since, now) {
                return Some((Intent::FansOn, "calor"));
            }
        }
        if self.level == 1 && critical && cooldown_ok {
            let held = self
                .hot_since
                .map(|start| now - start >= self.cfg.dwell_s * 3.0)
                .unwrap_or(false);
            if held {
                return Some((Intent::CpuDown, "critico"));
            }
        }
        // 3) Bajar: sólo con reposo largo y de a un escalón.
        let rested = self
            .cool_since
            .map(|start| now - start >= MAX_REST_S)
            .unwrap_or(false);
        if rested && cooldown_ok {
            match self.level {
                2 => return Some((Intent::CpuRestore, "enfriado")),
                1 => return Some((Intent::FansOff, "reposo")),
                _ => {}
            }
        }
        None
    }

    fn push_log(&mut self, entry: Value) {
        self.last_action = Some(entry.clone());
        self.last_action_at = entry
            .get("ts")
            .and_then(Value::as_f64)
            .unwrap_or_else(now_epoch);
        self.log.push_back(entry);
        while self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }

    fn finish(
        &mut self,
        intent: Intent,
        trigger: &'static str,
        temp: f64,
        now: f64,
        outcome: Result<String, String>,
    ) {
        let (ok, detail) = match outcome {
            Ok(detail) => (true, detail),
            Err(error) => (false, error),
        };
        if ok {
            self.writes = self.writes.saturating_add(1);
            self.last_error = None;
            match intent {
                Intent::FansOn => {
                    self.level = 1;
                    self.hot_since = None;
                }
                Intent::CpuDown => {
                    self.level = 2;
                    self.hot_since = None;
                }
                Intent::CpuRestore => {
                    self.level = 1;
                    self.cool_since = None;
                    if let Some(base) = self.baseline.as_mut() {
                        base.cpu_long = None;
                    }
                }
                Intent::FansOff => {
                    self.level = 0;
                    self.cool_since = None;
                    if let Some(base) = self.baseline.as_mut() {
                        base.full_speed = None;
                    }
                }
                Intent::ModePerf => {
                    self.last_thermal = Some(3);
                }
            }
        } else {
            self.failures = self.failures.saturating_add(1);
            self.last_error = Some(detail.clone());
        }
        let entry = json!({
            "ts": now,
            "time": iso_utc(now),
            "temp_c": temp,
            "trigger": trigger,
            "action": intent.key(),
            "level": self.level,
            "ok": ok,
            "detail": detail,
        });
        self.push_log(entry);
        self.save();
    }

    /// Restaura lo que haya tocado el control (CPU primero, fans al final).
    fn revert_all(&mut self, trigger: &str) -> Vec<Value> {
        let mut results = Vec::new();
        let now = now_epoch();
        if let Some(base) = self.baseline.clone() {
            if let Some(value) = base.cpu_long {
                let outcome = legion::set_cpu(&json!({"long": value.round() as i64}))
                    .map(|_| format!("límite CPU restaurado a {} W", value.round() as i64))
                    .map_err(|err| err.message);
                let entry = json!({
                    "ts": now,
                    "time": iso_utc(now),
                    "temp_c": self.last_temp,
                    "trigger": trigger,
                    "action": "cpu_restore",
                    "level": self.level,
                    "ok": outcome.is_ok(),
                    "detail": outcome.clone().unwrap_or_else(|err| err),
                });
                self.push_log(entry.clone());
                results.push(entry);
            }
            if let Some(full) = base.full_speed {
                let outcome = legion::set_fans(&json!({"full_speed": full}))
                    .map(|_| format!("full speed restaurado a {full}"))
                    .map_err(|err| err.message);
                let entry = json!({
                    "ts": now,
                    "time": iso_utc(now),
                    "temp_c": self.last_temp,
                    "trigger": trigger,
                    "action": "fans_off",
                    "level": 0,
                    "ok": outcome.is_ok(),
                    "detail": outcome.clone().unwrap_or_else(|err| err),
                });
                self.push_log(entry.clone());
                results.push(entry);
            }
        }
        self.level = 0;
        self.baseline = None;
        self.hot_since = None;
        self.cool_since = None;
        self.save();
        results
    }
}

/// Ejecuta la escritura real. Sólo se llama con admin + WMI disponible.
fn execute(intent: Intent) -> Result<String, String> {
    match intent {
        Intent::ModePerf => legion::set_thermal(&json!({"mode": 3}))
            .map(|_| "modo Performance (GPU hasta 140 W)".to_string())
            .map_err(|err| err.message),
        Intent::FansOn => legion::set_fans(&json!({"full_speed": true}))
            .map(|_| "full speed activado".to_string())
            .map_err(|err| err.message),
        Intent::FansOff => {
            let prior = {
                let state = lock();
                state
                    .baseline
                    .as_ref()
                    .and_then(|base| base.full_speed)
                    .unwrap_or(false)
            };
            legion::set_fans(&json!({"full_speed": prior}))
                .map(|_| format!("full speed restaurado a {prior}"))
                .map_err(|err| err.message)
        }
        Intent::CpuDown => {
            let status = legion::status();
            let limit = &status["cpu"]["long"];
            let current = limit.get("value").and_then(Value::as_f64);
            let min = limit.get("min").and_then(Value::as_f64);
            let step = limit.get("step").and_then(Value::as_f64);
            let (Some(current), Some(min)) = (current, min) else {
                return Err("no pude leer el límite de CPU actual; no escribo".into());
            };
            if current <= min {
                return Err(format!("el límite de CPU ya está en el mínimo ({min} W)"));
            }
            let pace = step.filter(|value| *value > 0.0).unwrap_or(1.0);
            let target = (current - pace).max(min);
            if target >= current {
                return Err(format!("el firmware no permite bajar de {current} W"));
            }
            {
                let mut state = lock();
                if state.baseline.is_none() {
                    state.baseline = Some(Baseline::default());
                }
                if let Some(base) = state.baseline.as_mut() {
                    if base.cpu_long.is_none() {
                        base.cpu_long = Some(current);
                    }
                }
            }
            legion::set_cpu(&json!({"long": target.round() as i64}))
                .map(|_| {
                    format!(
                        "límite CPU {} W → {} W",
                        current.round() as i64,
                        target.round() as i64
                    )
                })
                .map_err(|err| err.message)
        }
        Intent::CpuRestore => {
            let prior = {
                let state = lock();
                state.baseline.as_ref().and_then(|base| base.cpu_long)
            };
            match prior {
                Some(value) => legion::set_cpu(&json!({"long": value.round() as i64}))
                    .map(|_| format!("límite CPU restaurado a {} W", value.round() as i64))
                    .map_err(|err| err.message),
                None => Err("no hay límite previo guardado; no escribo".into()),
            }
        }
    }
}

/// Llamado por el hilo de telemetría con cada muestra. Nunca bloquea al motor
/// (no toca su mutex) y nunca escribe si falta admin/WMI.
pub fn observe(sample: &Sample, available: bool) {
    let admin = crate::win::is_admin();
    let temp = match (sample.cpu_temp, sample.gpu_temp) {
        (Some(cpu), Some(gpu)) => Some(cpu.max(gpu)),
        (Some(cpu), None) => Some(cpu),
        (None, Some(gpu)) => Some(gpu),
        (None, None) => None,
    };

    let now = now_epoch();
    let decision = {
        let mut state = lock();
        state.last_admin = admin;
        state.last_available = Some(available);
        match temp {
            Some(temp) => {
                state.last_temp = Some(temp);
                state.last_power = sample.gpu_power_w;
                state.last_thermal = sample.thermal_mode;
                let ac = crate::win::ac_online();
                state.last_ac = Some(ac);
                let decision = if state.cfg.action == ActionKind::Max
                    && state.cfg.enabled
                    && admin
                    && available
                {
                    state.decide_max(now, temp, sample.gpu_power_w, ac, sample.thermal_mode)
                } else {
                    state.decide(now, temp, admin, available)
                };
                Some((decision, temp))
            }
            None => None,
        }
    };

    let Some((Some((intent, trigger)), temp)) = decision else {
        return;
    };

    {
        let mut state = lock();
        state.busy = true;
        if state.baseline.is_none() {
            state.baseline = Some(Baseline::default());
        }
    }

    // Buscar el full speed previo antes de encenderlo, para poder restaurarlo.
    if intent == Intent::FansOn {
        let status = legion::status();
        let prior = status.get("full_speed").and_then(Value::as_bool);
        let mut state = lock();
        if let Some(base) = state.baseline.as_mut() {
            if base.full_speed.is_none() {
                base.full_speed = Some(prior.unwrap_or(false));
            }
        }
    }

    let outcome = execute(intent);
    let mut state = lock();
    state.busy = false;
    state.finish(intent, trigger, temp, now, outcome);
}

/// Estado del control: contrato estable para la UI.
pub fn status() -> Value {
    let state = lock();
    let now = now_epoch();
    let next_allowed = if state.last_action_at > 0.0 {
        (state.cfg.cooldown_s - (now - state.last_action_at)).max(0.0)
    } else {
        0.0
    };
    let temp = state.last_temp;
    let state_label = if !state.cfg.enabled {
        "off"
    } else if temp
        .map(|value| value >= state.cfg.critical_c)
        .unwrap_or(false)
    {
        "critical"
    } else if temp.map(|value| value >= state.cfg.hot_c).unwrap_or(false) {
        "hot"
    } else {
        "normal"
    };
    let last_action_text = state.last_action.as_ref().map(|entry| {
        format!(
            "{} · {} °C · {}",
            entry.get("action").and_then(Value::as_str).unwrap_or("?"),
            entry.get("temp_c").and_then(Value::as_f64).unwrap_or(0.0),
            entry.get("trigger").and_then(Value::as_str).unwrap_or("?")
        )
    });
    json!({
        "ok": true,
        "enabled": state.cfg.enabled,
        "supported": state.last_admin && state.last_available.unwrap_or(false),
        "admin": state.last_admin,
        "available": state.last_available,
        "error": state.last_error,
        "state": state_label,
        "level": state.level,
        // Campos planos que consume la UI (además del objeto `config`).
        "target_c": state.cfg.target_c,
        "hot_c": state.cfg.hot_c,
        "critical_c": state.cfg.critical_c,
        "hysteresis_c": state.cfg.hysteresis_c,
        "exit_c": state.cfg.exit_c(),
        "dwell_s": state.cfg.dwell_s,
        "cooldown_s": state.cfg.cooldown_s,
        "action": state.cfg.action.key(),
        "last_action": last_action_text,
        "last_action_at": if state.last_action_at > 0.0 { json!(state.last_action_at) } else { Value::Null },
        "last_action_time": if state.last_action_at > 0.0 { json!(iso_utc(state.last_action_at)) } else { Value::Null },
        "last_action_detail": state.last_action,
        "last_temp_c": state.last_temp,
        "explain": if state.cfg.enabled { json!(state.explain) } else { Value::Null },
        "gpu_power_w": state.last_power,
        "on_ac": state.last_ac,
        "slope_c_s": state.slope(now).map(|v| (v * 100.0).round() / 100.0),
        "busy": state.busy,
        "writes": state.writes,
        "failures": state.failures,
        "next_allowed_in_s": (next_allowed * 10.0).round() / 10.0,
        "baseline": state.baseline,
        "config": state.cfg.to_json(),
        "config_path": file_path().to_string_lossy(),
        "log": state.log.iter().cloned().collect::<Vec<Value>>(),
    })
}

fn number(body: &Value, key: &str) -> Result<Option<f64>, Error> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| Error {
                status: 400,
                message: format!("{key} debe ser un número (grados o segundos)"),
            }),
    }
}

/// POST /api/legion/adaptive
pub fn update(body: &Value) -> Result<Value, Error> {
    if !body.is_object() {
        return Err(Error {
            status: 400,
            message: "el cuerpo debe ser un objeto JSON".into(),
        });
    }
    let mut state = lock();
    let mut cfg = state.cfg.clone();
    if let Some(value) = body.get("enabled") {
        cfg.enabled = value.as_bool().ok_or_else(|| Error {
            status: 400,
            message: "enabled debe ser true|false".into(),
        })?;
    }
    if let Some(value) = number(body, "target_c")? {
        cfg.target_c = value;
    }
    if let Some(value) = number(body, "hot_c")? {
        cfg.hot_c = value;
    }
    if let Some(value) = number(body, "critical_c")? {
        cfg.critical_c = value;
    }
    if let Some(value) = number(body, "hysteresis_c")? {
        cfg.hysteresis_c = value;
    }
    if let Some(value) = number(body, "dwell_s")? {
        cfg.dwell_s = value;
    }
    if let Some(value) = number(body, "cooldown_s")? {
        cfg.cooldown_s = value;
    }
    if let Some(value) = body.get("action") {
        let text = value.as_str().ok_or_else(|| Error {
            status: 400,
            message: "action debe ser \"fans\", \"fans_cpu\" o \"max\"".into(),
        })?;
        cfg.action = ActionKind::parse(text).ok_or_else(|| Error {
            status: 400,
            message: "action inválida: usá \"fans\", \"fans_cpu\" o \"max\"".into(),
        })?;
    }
    cfg.validate().map_err(|message| Error {
        status: 400,
        message,
    })?;

    let restore = body
        .get("restore")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let was_enabled = state.cfg.enabled;
    state.cfg = cfg;

    if restore {
        if !crate::win::is_admin() {
            return Err(Error {
                status: 403,
                message: "restaurar el control adaptativo requiere admin (escribe en el EC)".into(),
            });
        }
        state.cfg.enabled = false;
        state.revert_all("manual_restore");
    } else if was_enabled && !state.cfg.enabled {
        let applied = state
            .baseline
            .as_ref()
            .map(|base| base.full_speed.is_some() || base.cpu_long.is_some())
            .unwrap_or(false);
        if crate::win::is_admin() {
            state.revert_all("manual_disable");
        } else if applied {
            state.last_error = Some(
                "adaptativo apagado, pero quedan cambios aplicados: elevá Domador y usá \
                 {\"restore\":true} para volver a full speed/cap originales"
                    .into(),
            );
        } else {
            state.last_error = None;
        }
    }
    state.save();
    drop(state);
    Ok(status())
}

/// Hook para pruebas/estado interno: si está armado y soportado.
pub fn enabled() -> bool {
    lock().cfg.enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(enabled: bool, action: ActionKind) -> State {
        let mut state = State::load();
        state.cfg = Config {
            enabled,
            action,
            ..Config::default()
        };
        state.level = 0;
        state.hot_since = None;
        state.cool_since = None;
        state.last_action_at = 0.0;
        state.baseline = None;
        state
    }

    #[test]
    fn valida_umbrales_coherentes() {
        let good = Config::default();
        assert!(good.validate().is_ok());
        let bad = Config {
            target_c: 86.0,
            ..Config::default()
        };
        assert!(bad.validate().is_err(), "target por encima de hot-hyst");
        let bad = Config {
            critical_c: 80.0,
            ..Config::default()
        };
        assert!(bad.validate().is_err());
        let bad = Config {
            dwell_s: 1.0,
            ..Config::default()
        };
        assert!(bad.validate().is_err());
        let bad = Config {
            cooldown_s: 1.0,
            ..Config::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn respeta_dwell_cooldown_y_histeresis() {
        let mut state = controller(true, ActionKind::Fans);
        // Arranca caliente a los 0 s: todavía no actúa.
        assert_eq!(state.decide(0.0, 86.0, true, true), None);
        assert_eq!(state.decide(5.0, 86.0, true, true), None);
        assert_eq!(
            state.decide(10.0, 86.0, true, true),
            Some((Intent::FansOn, "hot"))
        );
        // Simula la escritura exitosa y el cooldown.
        state.level = 1;
        state.last_action_at = 10.0;
        state.hot_since = None;
        assert_eq!(state.decide(20.0, 86.0, true, true), None, "cooldown");
        // En la banda de histéresis no apaga nada.
        assert_eq!(state.decide(45.0, 83.0, true, true), None);
        // Recién por debajo de 82 sostenido 10 s apaga.
        assert_eq!(state.decide(60.0, 81.0, true, true), None);
        assert_eq!(
            state.decide(70.0, 81.0, true, true),
            Some((Intent::FansOff, "cool"))
        );
    }

    #[test]
    fn escala_a_cpu_solo_en_critico_y_con_fans_cpu() {
        let mut state = controller(true, ActionKind::FansCpu);
        state.decide(0.0, 86.0, true, true);
        assert_eq!(
            state.decide(10.0, 86.0, true, true),
            Some((Intent::FansOn, "hot"))
        );
        state.level = 1;
        state.last_action_at = 10.0;
        state.hot_since = None;

        // Con fans ya encendidos, 86 °C no baja el cap: hace falta critical_c.
        assert_eq!(state.decide(40.0, 86.0, true, true), None);
        state.hot_since = Some(40.0);
        assert_eq!(
            state.decide(50.0, 93.0, true, true),
            Some((Intent::CpuDown, "critical"))
        );

        // Con action=fans no hay escalón de CPU.
        let mut fans_only = controller(true, ActionKind::Fans);
        fans_only.level = 1;
        fans_only.last_action_at = 0.0;
        assert_eq!(fans_only.decide(100.0, 96.0, true, true), None);
    }

    #[test]
    fn desescala_un_paso_por_vez() {
        let mut state = controller(true, ActionKind::FansCpu);
        state.level = 2;
        state.last_action_at = 0.0;
        // El dwell también aplica al enfriamiento: no restaura de golpe.
        assert_eq!(state.decide(10.0, 78.0, true, true), None);
        assert_eq!(
            state.decide(20.0, 78.0, true, true),
            Some((Intent::CpuRestore, "cool"))
        );
        state.level = 1;
        state.last_action_at = 20.0;
        state.cool_since = None;
        assert_eq!(state.decide(40.0, 78.0, true, true), None, "dwell otra vez");
        assert_eq!(
            state.decide(50.0, 78.0, true, true),
            Some((Intent::FansOff, "cool"))
        );
    }

    #[test]
    fn sin_admin_o_wmi_no_actua_y_lo_informa() {
        let mut state = controller(true, ActionKind::Fans);
        assert_eq!(state.decide(0.0, 99.0, false, true), None);
        assert!(state.last_error.is_some());
        let mut state = controller(true, ActionKind::Fans);
        assert_eq!(state.decide(0.0, 99.0, true, false), None);
        assert!(state.last_error.is_some());
        // Apagado no hace nada aunque haya 100 °C.
        let mut state = controller(false, ActionKind::Fans);
        assert_eq!(state.decide(0.0, 100.0, true, true), None);
    }

    fn max_controller() -> State {
        let mut state = controller(true, ActionKind::Max);
        state.cfg.target_c = 58.0;
        state.cfg.hot_c = 70.0;
        state.cfg.critical_c = 90.0;
        state.cfg.hysteresis_c = 4.0;
        state.cfg.dwell_s = 4.0;
        state.cfg.cooldown_s = 15.0;
        state.history.clear();
        state.power_since = None;
        state
    }

    #[test]
    fn max_pone_performance_solo_con_cargador() {
        let mut state = max_controller();
        assert_eq!(
            state.decide_max(0.0, 50.0, Some(20.0), true, Some(2)),
            Some((Intent::ModePerf, "enchufada"))
        );
        let mut state = max_controller();
        assert_eq!(
            state.decide_max(0.0, 50.0, Some(20.0), false, Some(2)),
            None
        );
    }

    #[test]
    fn max_anticipa_por_pendiente_antes_del_umbral() {
        let mut state = max_controller();
        // Sube 6 °C en 8 s (0,75 °C/s) y todavía está lejos de hot_c.
        assert_eq!(state.decide_max(0.0, 55.0, Some(30.0), true, Some(3)), None);
        assert_eq!(state.decide_max(4.0, 58.0, Some(30.0), true, Some(3)), None);
        assert_eq!(
            state.decide_max(8.0, 61.0, Some(30.0), true, Some(3)),
            Some((Intent::FansOn, "anticipa"))
        );
    }

    #[test]
    fn max_sube_por_carga_sostenida_y_baja_solo_en_reposo_largo() {
        let mut state = max_controller();
        assert_eq!(
            state.decide_max(0.0, 60.0, Some(120.0), true, Some(3)),
            None
        );
        assert_eq!(
            state.decide_max(5.0, 60.0, Some(120.0), true, Some(3)),
            Some((Intent::FansOn, "carga"))
        );
        state.level = 1;
        state.last_action_at = 5.0;
        // Frío y en reposo, pero recién: no baja todavía.
        assert_eq!(
            state.decide_max(30.0, 50.0, Some(20.0), true, Some(3)),
            None
        );
        assert_eq!(
            state.decide_max(100.0, 50.0, Some(20.0), true, Some(3)),
            None
        );
        assert_eq!(
            state.decide_max(160.0, 50.0, Some(20.0), true, Some(3)),
            Some((Intent::FansOff, "reposo"))
        );
    }

    #[test]
    fn max_freno_de_cpu_solo_en_critico_sostenido() {
        let mut state = max_controller();
        state.level = 1;
        assert_eq!(
            state.decide_max(0.0, 91.0, Some(130.0), true, Some(3)),
            None
        );
        assert_eq!(
            state.decide_max(6.0, 91.0, Some(130.0), true, Some(3)),
            None
        );
        assert_eq!(
            state.decide_max(13.0, 91.0, Some(130.0), true, Some(3)),
            Some((Intent::CpuDown, "critico"))
        );
    }
}
