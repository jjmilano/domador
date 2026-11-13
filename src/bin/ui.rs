//! UI nativa de Domador (egui/eframe) — cabina de control steampunk.
//!
//! No usa Edge ni WebView: habla con el motor local por HTTP y dibuja con
//! OpenGL. El motor sigue siendo el mismo `domador.exe`; esta ventana es
//! descartable, igual que la UI web, pero pesa una fracción.
//!
//! Diseño: superficie tipo "monitor/operate". Carbón, latón y cobre para
//! jerarquizar; tarjetas con borde y remaches; números en monospace. Cero
//! emojis: los iconos se dibujan con el painter para que no dependan de la
//! fuente de emojis de Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Id, Margin, Pos2, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, Vec2,
};
use egui_extras::{Column, TableBuilder};
use serde_json::{json, Value};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Puerto del motor local. `DOMADOR_PORT` permite apuntar la UI a otro motor
/// (útil para probar la interfaz contra un doble sin tocar el que está en uso).
fn port() -> u16 {
    static PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *PORT.get_or_init(|| {
        std::env::var("DOMADOR_PORT")
            .ok()
            .and_then(|raw| raw.trim().parse::<u16>().ok())
            .filter(|p| *p >= 1024)
            .unwrap_or(8787)
    })
}

// ---------------------------------------------------------------- tokens ---
// Carbón cálido de fondo, latón para la marca y el foco, cobre para alertas.
const BG: Color32 = Color32::from_rgb(20, 16, 13);
const PANEL: Color32 = Color32::from_rgb(28, 21, 16);
const CARD: Color32 = Color32::from_rgb(36, 27, 20);
const CARD_HOVER: Color32 = Color32::from_rgb(46, 34, 25);
const SUNKEN: Color32 = Color32::from_rgb(15, 11, 8);
const INK: Color32 = Color32::from_rgb(24, 17, 10);
const BORDER: Color32 = Color32::from_rgb(74, 54, 38);
const BORDER_SOFT: Color32 = Color32::from_rgb(52, 39, 28);
const BRASS: Color32 = Color32::from_rgb(215, 162, 74);
const BRASS_HI: Color32 = Color32::from_rgb(236, 190, 112);
const BRASS_DIM: Color32 = Color32::from_rgb(150, 112, 52);
const COPPER: Color32 = Color32::from_rgb(185, 99, 61);
const COPPER_HI: Color32 = Color32::from_rgb(226, 141, 98);
const TEXT: Color32 = Color32::from_rgb(244, 231, 211);
const MUTED: Color32 = Color32::from_rgb(185, 164, 143);
const FAINT: Color32 = Color32::from_rgb(143, 122, 103);
const OK: Color32 = Color32::from_rgb(134, 194, 145);
const WARN: Color32 = Color32::from_rgb(232, 176, 92);
const DANGER: Color32 = Color32::from_rgb(219, 106, 86);
const ZEBRA: Color32 = Color32::from_rgb(31, 23, 18);

fn a(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn mk_stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

#[derive(Clone)]
struct Toast {
    text: String,
    ok: bool,
    at: Instant,
}

#[derive(Clone)]
struct Detail {
    pid: u32,
    loading: bool,
    error: Option<String>,
    data: Option<Arc<Value>>,
}

/// Estado del backend Legion (`/api/legion`). Vive aparte del snapshot de
/// procesos porque se refresca a otro ritmo y puede fallar por su cuenta
/// (404 mientras el backend no exista, 403 sin admin, 503 sin WMI).
#[derive(Clone, Debug)]
enum LegionErr {
    /// 404: el motor todavía no publica el endpoint.
    Pending,
    /// 401/403: hace falta una sesión elevada.
    NoAdmin,
    /// 501/503: el EC no expone WMI.
    Wmi,
    /// El motor no contesta.
    Offline,
    /// Cualquier otra respuesta con cuerpo.
    Other(String),
}

impl LegionErr {
    fn from_status(code: u16, body: String) -> Self {
        match code {
            401 | 403 => Self::NoAdmin,
            404 => Self::Pending,
            501 | 503 => Self::Wmi,
            _ => {
                let body: String = body.trim().chars().take(180).collect();
                if body.is_empty() {
                    Self::Other(format!("HTTP {code}"))
                } else {
                    Self::Other(format!("HTTP {code} · {body}"))
                }
            }
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Pending => "Backend Legion pendiente",
            Self::NoAdmin => "Falta administrador",
            Self::Wmi => "WMI del EC no disponible",
            Self::Offline => "Motor sin conexión",
            Self::Other(_) => "El EC devolvió un error",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::Pending => "GET /api/legion responde 404: el motor todavía no publica el panel del EC. La pestaña queda armada y se llena sola cuando exista el backend; Procesos y Salud siguen funcionando igual.".into(),
            Self::NoAdmin => "El control del EC sólo acepta órdenes de una sesión elevada. Elevá la UI (UAC) y volvé a Legion: los botones se habilitan solos.".into(),
            Self::Wmi => "Windows no expone el controlador WMI del EC (ATKWMI/ASUS o equivalente). Sin eso no hay RPM ni límites: el panel queda en modo lectura.".into(),
            Self::Offline => "No hay respuesta en 127.0.0.1:8787. Arrancá el motor con `domador run --no-open` y reintentá.".into(),
            Self::Other(text) => text.clone(),
        }
    }

    fn color(&self) -> Color32 {
        match self {
            Self::Pending | Self::NoAdmin | Self::Wmi => WARN,
            Self::Offline => DANGER,
            Self::Other(_) => COPPER_HI,
        }
    }
}

#[derive(Default)]
struct LegionData {
    data: Option<Arc<Value>>,
    error: Option<LegionErr>,
    /// Última respuesta de un POST de Legion (para saber qué se cerró).
    action: Option<Arc<Value>>,
    busy: Option<String>,
}

#[derive(Default)]
struct Shared {
    state: Option<Arc<Value>>,
    error: Option<String>,
    busy: Option<String>,
    toast: Option<Toast>,
    last_ok: Option<Instant>,
    detail: Option<Detail>,
    legion: LegionData,
    telemetry: Telemetry,
    adaptive: Adaptive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Cpu,
    Ram,
    Name,
}

impl SortBy {
    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Ram => "RAM",
            Self::Name => "Nombre",
        }
    }
}

#[derive(Clone, Copy)]
struct SortState {
    key: SortBy,
    desc: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Processes,
    Health,
    Settings,
    Legion,
}

#[derive(Clone, Copy)]
enum Icon {
    List,
    Pulse,
    Gear,
    Fan,
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

struct DomadorUi {
    shared: Arc<Mutex<Shared>>,
    tab: Tab,
    search: String,
    sort: SortState,
    selected: Option<u32>,
    kill_confirm: Option<(u32, String)>,
    logo: Option<egui::TextureHandle>,
    focus_search: bool,
    /// Borradores de la pestaña Legion: se sincronizan con el EC salvo que el
    /// usuario los esté tocando.
    legion_ui: LegionUi,
}

/// Borradores editables del panel Legion. `*_edited` marca que el usuario movió
/// algo y todavía no aplicó: mientras esté en true no pisamos sus valores con
/// el polling.
struct LegionUi {
    cpu_long: i64,
    cpu_short: i64,
    cpu_edited: bool,
    curve: [i64; 10],
    curve_edited: bool,
    pending_cpu: Option<(i64, i64, String)>,
    confirm_restore: bool,
    /// Ventana del histórico de telemetría, en minutos (10/30/60).
    tele_range: u32,
    /// Refresco automático del histórico mientras la pestaña está abierta.
    tele_auto: bool,
    /// Borradores del lazo AUTO: se sincronizan con el motor salvo que el
    /// usuario los esté tocando (`ad_edited`).
    ad_enabled: bool,
    ad_target: i64,
    ad_hot: i64,
    ad_critical: i64,
    ad_edited: bool,
}

impl Default for LegionUi {
    fn default() -> Self {
        Self {
            cpu_long: 0,
            cpu_short: 0,
            cpu_edited: false,
            curve: [5; 10],
            curve_edited: false,
            pending_cpu: None,
            confirm_restore: false,
            tele_range: 10,
            tele_auto: true,
            ad_enabled: false,
            ad_target: 82,
            ad_hot: 85,
            ad_critical: 92,
            ad_edited: false,
        }
    }
}

/// Intenciones que devuelve el dibujo de la pestaña. Se aplican después de
/// pintar para no pedir `&mut self` dentro de las tarjetas.
#[derive(Default)]
struct LegionIntent {
    thermal: Option<String>,
    cpu: Option<(i64, i64)>,
    curve: Option<Vec<i64>>,
    full_speed: Option<bool>,
    chief: Option<bool>,
    restore: bool,
    refresh: bool,
    elevate: bool,
    tele_fetch: bool,
    tele_export: bool,
    adaptive_fetch: bool,
    adaptive: Option<Value>,
}

// ------------------------------------------------------------------- HTTP ---

fn base() -> String {
    format!("http://127.0.0.1:{}", port())
}

fn get_state() -> Result<Value, String> {
    ureq::get(&format!("{}/api/state", base()))
        .call()
        .map_err(|err| format!("motor no disponible: {err}"))?
        .into_json()
        .map_err(|err| format!("respuesta inválida: {err}"))
}

fn get_path(path: &str) -> Result<Value, String> {
    ureq::get(&format!("{}{}", base(), path))
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => format!("no pude conectar: {other}"),
        })?
        .into_json()
        .map_err(|err| format!("respuesta inválida: {err}"))
}

fn post(path: &'static str, body: Value) -> Result<Value, String> {
    match ureq::post(&format!("{}{path}", base()))
        .set("X-Domador", "1")
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(response) => response
            .into_json()
            .map_err(|err| format!("respuesta inválida: {err}")),
        Err(ureq::Error::Status(code, response)) => Err(format!(
            "HTTP {code}: {}",
            response.into_string().unwrap_or_default()
        )),
        Err(err) => Err(format!("no pude conectar: {err}")),
    }
}

fn ensure_server() {
    let addr = SocketAddr::from(([127, 0, 0, 1], port()));
    let alive = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok();
    if alive {
        // Si la UI ya es elevada pero el motor quedó viejo y sin admin, hay
        // que reiniciarlo: el token no se puede "ascender" en caliente.
        if domador::win::is_admin() && !engine_is_admin() {
            let _ = post("/api/shutdown", json!({}));
            thread::sleep(Duration::from_millis(800));
            spawn_engine();
            wait_for_engine(&addr);
        }
        return;
    }
    spawn_engine();
    wait_for_engine(&addr);
}

fn engine_is_admin() -> bool {
    get_state()
        .ok()
        .and_then(|state| state.pointer("/app/admin").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

fn engine_exe() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("domador.exe")))
}

fn spawn_engine() {
    if let Some(exe) = engine_exe() {
        let _ = Command::new(exe)
            .args(["run", "--no-open"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

fn spawn_engine_elevated() {
    if let Some(exe) = engine_exe() {
        let _ = shell_runas(&exe.to_string_lossy(), "run --no-open");
    }
}

fn relaunch_ui_admin() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = shell_runas(&exe.to_string_lossy(), "");
    }
}

/// `ShellExecuteW(..., "runas")` es más confiable que un PowerShell oculto:
/// el UAC aparece asociado a la app y no queda un proceso intermedio colgado.
fn shell_runas(exe: &str, args: &str) -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }
    let operation = wide("runas");
    let file = wide(exe);
    let params = wide(args);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1,
        )
    };
    result as isize > 32
}

fn wait_for_engine(addr: &SocketAddr) {
    for _ in 0..40 {
        if TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn store_state(shared: &Arc<Mutex<Shared>>, result: Result<Value, String>) {
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    match result {
        Ok(value) => {
            guard.state = Some(Arc::new(value));
            guard.error = None;
            guard.last_ok = Some(Instant::now());
        }
        Err(err) => {
            guard.error = Some(err);
        }
    }
}

fn poll_thread(shared: Arc<Mutex<Shared>>) {
    loop {
        let result = get_state();
        store_state(&shared, result);
        thread::sleep(Duration::from_millis(700));
    }
}

fn act_bg(
    shared: Arc<Mutex<Shared>>,
    path: &'static str,
    body: Value,
    busy: &'static str,
    done: &'static str,
) {
    {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.busy = Some(busy.into());
    }
    thread::spawn(move || {
        let result = post(path, body);
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.busy = None;
        match result {
            Ok(value) => {
                let freed = value.get("freed").and_then(|v| v.as_u64()).unwrap_or(0);
                let text = if freed > 0 {
                    format!("{done} · {} liberados", fmt_bytes(freed))
                } else {
                    done.to_string()
                };
                guard.toast = Some(Toast {
                    text,
                    ok: true,
                    at: Instant::now(),
                });
                drop(guard);
                store_state(&shared, get_state());
            }
            Err(err) => {
                guard.toast = Some(Toast {
                    text: err,
                    ok: false,
                    at: Instant::now(),
                });
            }
        }
    });
}

// --------------------------------------------------------------- legion ---

fn get_legion() -> Result<Value, LegionErr> {
    match ureq::get(&format!("{}/api/legion", base())).call() {
        Ok(response) => response
            .into_json()
            .map_err(|err| LegionErr::Other(format!("respuesta inválida: {err}"))),
        Err(ureq::Error::Status(code, response)) => Err(LegionErr::from_status(
            code,
            response.into_string().unwrap_or_default(),
        )),
        Err(_) => Err(LegionErr::Offline),
    }
}

fn legion_thread(shared: Arc<Mutex<Shared>>) {
    // Un respiro al arrancar: primero que la UI pinte y consiga /api/state.
    thread::sleep(Duration::from_millis(900));
    loop {
        let result = get_legion();
        {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            match result {
                Ok(value) => {
                    guard.legion.data = Some(Arc::new(value));
                    guard.legion.error = None;
                }
                Err(err) => guard.legion.error = Some(err),
            }
        }
        thread::sleep(Duration::from_millis(2600));
    }
}

fn closed_names(value: &Value) -> Vec<String> {
    for key in ["killed", "closed"] {
        if let Some(items) = value.get(key).and_then(|v| v.as_array()) {
            let names: Vec<String> = items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| jstr(item, &["name", "process", "exe"]).map(str::to_string))
                })
                .collect();
            if !names.is_empty() {
                return names;
            }
        }
    }
    Vec::new()
}

fn friendly_error(err: &str) -> String {
    if err.contains("401") || err.contains("403") {
        format!("Necesita administrador · {err}")
    } else if err.contains("404") {
        "El motor no conoce este comando (404): falta el backend Legion".to_string()
    } else if err.contains("503") || err.contains("501") {
        format!("El EC no respondió · {err}")
    } else {
        err.to_string()
    }
}

fn push_legion_action(
    shared: Arc<Mutex<Shared>>,
    path: &'static str,
    body: Value,
    busy: String,
    done: String,
) {
    {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.legion.busy = Some(busy);
    }
    thread::spawn(move || {
        let result = post(path, body);
        let ok = result.is_ok();
        {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.legion.busy = None;
            match result {
                Ok(value) => {
                    let closed = closed_names(&value);
                    let text = if closed.is_empty() {
                        done.clone()
                    } else {
                        format!("{done} · cerré {}", closed.join(", "))
                    };
                    guard.toast = Some(Toast {
                        text,
                        ok: true,
                        at: Instant::now(),
                    });
                    guard.legion.action = Some(Arc::new(value));
                }
                Err(err) => {
                    guard.toast = Some(Toast {
                        text: friendly_error(&err),
                        ok: false,
                        at: Instant::now(),
                    });
                }
            }
        }
        if ok {
            // El EC tardó lo suyo: releemos antes de que llegue el próximo poll.
            if let Ok(value) = get_legion() {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.legion.data = Some(Arc::new(value));
                guard.legion.error = None;
            }
            store_state(&shared, get_state());
        }
    });
}

// -------------------------------------------------------------- telemetría ---

/// Una muestra del histórico del EC (`GET /api/legion/history`). Los campos
/// que el motor no publica quedan en `None`: el gráfico corta la línea en vez
/// de inventar un cero, y las estadísticas ignoran el hueco.
#[derive(Clone)]
struct TeleSample {
    ts: f64,
    cpu: Option<f64>,
    gpu: Option<f64>,
    fan1: Option<f64>,
    fan2: Option<f64>,
    gpu_w: Option<f64>,
    gpu_mhz: Option<f64>,
    mode: Option<String>,
}

/// Estado del histórico. Tiene sus propios errores porque puede faltar aunque
/// `/api/legion` responda bien (404 mientras el motor no guarde muestras).
#[derive(Clone, Debug)]
enum TelemetryErr {
    Pending,
    NoAdmin,
    Offline,
    Other(String),
}

impl TelemetryErr {
    fn from_status(code: u16, body: String) -> Self {
        match code {
            401 | 403 => Self::NoAdmin,
            404 => Self::Pending,
            _ => {
                let body: String = body.trim().chars().take(160).collect();
                if body.is_empty() {
                    Self::Other(format!("HTTP {code}"))
                } else {
                    Self::Other(format!("HTTP {code} · {body}"))
                }
            }
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Pending => "Histórico pendiente",
            Self::NoAdmin => "Falta administrador",
            Self::Offline => "Motor sin conexión",
            Self::Other(_) => "El histórico devolvió un error",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::Pending => "GET /api/legion/history responde 404: el motor todavía no guarda histórico. La sección queda armada y se llena sola cuando publique el buffer; el resto de la pestaña sigue igual.".into(),
            Self::NoAdmin => "El histórico del EC pide sesión elevada, igual que el resto del panel. Elevá la UI (UAC) y volvé a intentar.".into(),
            Self::Offline => "No hay respuesta en el motor local. Arrancalo con `domador run --no-open` y reintentá.".into(),
            Self::Other(text) => text.clone(),
        }
    }

    fn color(&self) -> Color32 {
        match self {
            Self::Pending | Self::NoAdmin => WARN,
            Self::Offline => DANGER,
            Self::Other(_) => COPPER_HI,
        }
    }
}

/// Último histórico leído. `minutes` guarda el rango **pedido** (no el que
/// trajo el motor) para saber cuándo hay que volver a pedir al mover el
/// selector; `attempted` marca el último intento, así el reintento automático
/// no martilla el endpoint en cada frame.
#[derive(Clone, Default)]
struct Telemetry {
    data: Option<Arc<TelemetryData>>,
    error: Option<TelemetryErr>,
    loading: bool,
    minutes: u32,
    attempted: Option<Instant>,
    fetched: Option<Instant>,
}

#[derive(Default)]
struct TelemetryData {
    samples: Vec<TeleSample>,
    minutes: u32,
}

fn opt_num(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(number) = value.as_f64() {
        return number.is_finite().then_some(number);
    }
    if let Some(text) = value.as_str() {
        return text.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    }
    if let Some(object) = value.as_object() {
        // Forma objeto (`{"rpm": 3200}` o `{"value": 62}`) por si el backend
        // publica los fans como estructuras en vez de números sueltos.
        for key in ["value", "temp", "celsius", "rpm"] {
            if let Some(number) = object.get(key).and_then(|v| v.as_f64()) {
                return number.is_finite().then_some(number);
            }
        }
    }
    None
}

fn sample_num(raw: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| opt_num(raw.get(*key)))
}

fn parse_sample(raw: &Value) -> Option<TeleSample> {
    let ts = sample_num(raw, &["ts", "t", "time", "timestamp"])?;
    // Algunos backends mandan milisegundos: 1e11 segundos no existe.
    let ts = if ts > 1.0e11 { ts / 1000.0 } else { ts };
    Some(TeleSample {
        ts,
        cpu: sample_num(raw, &["cpu_temp", "temp_cpu", "cpu"]),
        gpu: sample_num(raw, &["gpu_temp", "temp_gpu", "gpu"]),
        fan1: sample_num(raw, &["fan1", "fan1_rpm", "fan_1", "fan_1_rpm"]),
        fan2: sample_num(raw, &["fan2", "fan2_rpm", "fan_2", "fan_2_rpm"]),
        gpu_w: sample_num(raw, &["gpu_power_w", "gpu_power", "gpu_w"]),
        gpu_mhz: sample_num(raw, &["gpu_clock_mhz", "gpu_clock", "gpu_mhz"]),
        mode: jstr(raw, &["thermal_mode", "mode"]).map(str::to_string),
    })
}

fn get_telemetry(minutes: u32) -> Result<TelemetryData, TelemetryErr> {
    let url = format!("{}/api/legion/history?minutes={}", base(), minutes);
    let value: Value = match ureq::get(&url).call() {
        Ok(response) => response
            .into_json()
            .map_err(|err| TelemetryErr::Other(format!("respuesta inválida: {err}")))?,
        Err(ureq::Error::Status(code, response)) => {
            return Err(TelemetryErr::from_status(
                code,
                response.into_string().unwrap_or_default(),
            ))
        }
        Err(_) => return Err(TelemetryErr::Offline),
    };
    let mut samples: Vec<TeleSample> = value
        .get("samples")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_sample).collect())
        .unwrap_or_default();
    samples.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap_or(std::cmp::Ordering::Equal));
    if samples.is_empty() && value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        if let Some(message) = jstr(&value, &["error", "detail", "message"]) {
            let message: String = message.trim().chars().take(160).collect();
            if !message.is_empty() {
                return Err(TelemetryErr::Other(message));
            }
        }
    }
    Ok(TelemetryData { samples, minutes })
}

/// Pide el histórico en un hilo aparte: dibujar nunca bloquea la UI.
fn refresh_telemetry(shared: &Arc<Mutex<Shared>>, minutes: u32) {
    {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        if guard.telemetry.loading {
            return;
        }
        guard.telemetry.loading = true;
        guard.telemetry.attempted = Some(Instant::now());
    }
    let shared = shared.clone();
    thread::spawn(move || {
        let result = get_telemetry(minutes);
        {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.telemetry.loading = false;
            guard.telemetry.minutes = minutes;
            match result {
                Ok(data) => {
                    guard.telemetry.data = Some(Arc::new(data));
                    guard.telemetry.error = None;
                    guard.telemetry.fetched = Some(Instant::now());
                }
                Err(err) => guard.telemetry.error = Some(err),
            }
        }
        // El lazo AUTO viaja en el mismo tick: misma cadencia, un request más.
        refresh_adaptive(&shared);
    });
}

/// Epoch → fecha civil UTC (algoritmo civil_from_days). Sin dependencias de
/// fecha: el CSV necesita un `utc` legible y el nombre de archivo una marca.
fn civil_utc(epoch: f64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = epoch.max(0.0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day, hour, minute, second)
}

fn iso_utc(epoch: f64) -> String {
    let (y, m, d, h, mi, s) = civil_utc(epoch);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn stamp_utc() -> String {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let (y, m, d, h, mi, s) = civil_utc(epoch);
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// CSV armado por la UI, con el mismo esquema que publica el motor, por si
/// `/api/legion/export.csv` todavía no existe.
fn csv_from(samples: &[TeleSample]) -> String {
    let cell = |v: Option<f64>| match v {
        Some(v) if v.is_finite() => format!("{v:.2}"),
        _ => String::new(),
    };
    let mut out = String::from(
        "ts,utc,cpu_temp,gpu_temp,fan1_rpm,fan2_rpm,gpu_power_w,gpu_clock_mhz,thermal_mode\n",
    );
    for s in samples {
        let mode = s.mode.clone().unwrap_or_default().replace(',', ";");
        out.push_str(&format!(
            "{:.0},{},{},{},{},{},{},{},{}\n",
            s.ts,
            iso_utc(s.ts),
            cell(s.cpu),
            cell(s.gpu),
            cell(s.fan1),
            cell(s.fan2),
            cell(s.gpu_w),
            cell(s.gpu_mhz),
            mode
        ));
    }
    out
}

fn write_export(minutes: u32, body: &str) -> std::io::Result<std::path::PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = root.join("Domador").join("exports");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("telemetria-{minutes}m-{}.csv", stamp_utc()));
    std::fs::write(&path, body.as_bytes())?;
    Ok(path)
}

/// Exporta el CSV: primero pregunta al motor; si no publica el endpoint,
/// escribe lo que la UI ya tiene en memoria (y lo dice en el aviso).
fn export_telemetry(shared: Arc<Mutex<Shared>>, minutes: u32, samples: Vec<TeleSample>) {
    thread::spawn(move || {
        let mut origin = "motor";
        let fetched = match ureq::get(&format!(
            "{}/api/legion/export.csv?minutes={}",
            base(),
            minutes
        ))
        .call()
        {
            Ok(response) => response
                .into_string()
                .ok()
                .filter(|text| !text.trim().is_empty()),
            Err(_) => None,
        };
        let body = match fetched {
            Some(text) => text,
            None if !samples.is_empty() => {
                origin = "UI";
                csv_from(&samples)
            }
            None => {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.toast = Some(Toast {
                    text: "No pude exportar: el motor no publica /api/legion/export.csv y no hay muestras en memoria".to_string(),
                    ok: false,
                    at: Instant::now(),
                });
                return;
            }
        };
        let (text, ok) = match write_export(minutes, &body) {
            Ok(path) => (
                format!("CSV guardado ({origin}) en {}", path.display()),
                true,
            ),
            Err(err) => (format!("No pude escribir el CSV: {err}"), false),
        };
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.toast = Some(Toast {
            text,
            ok,
            at: Instant::now(),
        });
    });
}

// ------------------------------------------------------------ formatters ---

fn fmt_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= GB {
        if b / GB >= 10.0 {
            format!("{:.1} GB", b / GB)
        } else {
            format!("{:.2} GB", b / GB)
        }
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}

fn fmt_uptime(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let d = s / 86_400;
    let h = (s % 86_400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

fn num(value: Option<&Value>) -> u64 {
    value.and_then(|v| v.as_u64()).unwrap_or(0)
}

fn fnum(value: Option<&Value>) -> f64 {
    value.and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn text_of<'a>(value: Option<&'a Value>) -> &'a str {
    value.and_then(|v| v.as_str()).unwrap_or("")
}

fn jstr<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
}

fn jbool(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_bool()))
}

fn parent_text(value: &Value) -> Option<String> {
    let parent = value.get("parent").or_else(|| value.get("parent_pid"))?;
    if let Some(text) = parent.as_str() {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_owned());
        }
    }
    if let Some(pid) = parent.as_u64() {
        return Some(format!("PID {pid}"));
    }
    let name = jstr(parent, &["name", "exe"]);
    let pid = parent.get("pid").and_then(|v| v.as_u64());
    match (name, pid) {
        (Some(name), Some(pid)) => Some(format!("{name} (PID {pid})")),
        (Some(name), None) => Some(name.to_owned()),
        (None, Some(pid)) => Some(format!("PID {pid}")),
        _ => None,
    }
}

fn rel_time(epoch: f64) -> String {
    if epoch <= 0.0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let delta = (now - epoch).max(0.0);
    if delta < 60.0 {
        "recién".to_string()
    } else {
        format!("hace {}", fmt_uptime(delta))
    }
}

fn start_text(value: &Value) -> Option<String> {
    let raw = value.get("start_time")?;
    if let Some(secs) = raw.as_f64() {
        let rel = rel_time(secs);
        if !rel.is_empty() {
            return Some(rel);
        }
    }
    if let Some(text) = raw.as_str() {
        let text = text.trim();
        if !text.is_empty() && text != "0" {
            return Some(text.to_owned());
        }
    }
    None
}

fn signature_line(value: &Value) -> Option<(String, bool)> {
    if let Some(text) = jstr(
        value,
        &[
            "signature",
            "signer",
            "signature_status",
            "trust",
            "trust_status",
        ],
    ) {
        let lower = text.to_lowercase();
        let ok = !lower.contains("sin firma")
            && !lower.contains("unsigned")
            && !lower.contains("no firmado")
            && !lower.contains("untrusted");
        return Some((text.to_owned(), ok));
    }
    if let Some(trusted) = jbool(value, &["trusted", "valid"]) {
        return Some((
            if trusted {
                "Firma verificada".into()
            } else {
                "Sin firma verificada".into()
            },
            trusted,
        ));
    }
    if let Some(signed) = jbool(value, &["signed"]) {
        return Some((
            if signed {
                "Firmado".into()
            } else {
                "Sin firma digital".into()
            },
            signed,
        ));
    }
    None
}

fn cpu_color(cpu: f64) -> Color32 {
    if cpu >= 25.0 {
        COPPER_HI
    } else if cpu >= 5.0 {
        BRASS
    } else {
        OK
    }
}

fn prio_colors(prio: &str) -> (Color32, Color32) {
    match prio {
        "realtime" => (DANGER, a(DANGER, 34)),
        "high" => (BRASS, a(BRASS, 30)),
        "above_normal" => (Color32::from_rgb(222, 192, 140), a(BRASS, 20)),
        "below_normal" => (
            Color32::from_rgb(146, 170, 190),
            a(Color32::from_rgb(120, 152, 184), 26),
        ),
        "idle" => (FAINT, a(FAINT, 26)),
        _ => (MUTED, a(MUTED, 18)),
    }
}

fn avatar_color(name: &str) -> Color32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in name.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    const PALETTE: [Color32; 6] = [
        BRASS,
        COPPER,
        Color32::from_rgb(139, 158, 124),
        Color32::from_rgb(161, 133, 178),
        Color32::from_rgb(122, 148, 168),
        Color32::from_rgb(194, 154, 96),
    ];
    PALETTE[hash as usize % PALETTE.len()]
}

fn avatar_letter(name: &str) -> String {
    name.chars()
        .find(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

fn ellipsize(ui: &egui::Ui, text: &str, font: FontId, max_width: f32) -> String {
    let measure = |candidate: &str| {
        ui.fonts(|f| {
            f.layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    if max_width <= 0.0 {
        return String::new();
    }
    if measure(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high + 1) / 2;
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        if measure(&candidate) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut out: String = chars[..low].iter().collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------- drawing ---

fn rivets(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    for pos in [
        Pos2::new(rect.left() + 7.0, rect.top() + 7.0),
        Pos2::new(rect.right() - 7.0, rect.top() + 7.0),
        Pos2::new(rect.left() + 7.0, rect.bottom() - 7.0),
        Pos2::new(rect.right() - 7.0, rect.bottom() - 7.0),
    ] {
        painter.circle_filled(pos, 1.5, a(BRASS_DIM, 150));
        painter.circle_filled(Pos2::new(pos.x - 0.4, pos.y - 0.4), 0.6, a(BRASS_HI, 90));
    }
}

fn hairline(ui: &egui::Ui, y: f32, color: Color32) {
    let rect = ui.max_rect();
    ui.painter().hline(rect.x_range(), y, mk_stroke(1.0, color));
}

fn text_width(ui: &egui::Ui, text: &str, font: FontId) -> f32 {
    ui.fonts(|f| {
        f.layout_no_wrap(text.to_owned(), font, Color32::WHITE)
            .size()
            .x
    })
}

fn paint_icon(painter: &egui::Painter, center: Pos2, size: f32, icon: Icon, color: Color32) {
    let s = size * 0.5;
    let stroke = mk_stroke(1.4, color);
    match icon {
        Icon::List => {
            for (i, width) in [1.0_f32, 0.7, 0.88].iter().enumerate() {
                let y = center.y - s * 0.62 + i as f32 * s * 0.62;
                painter.circle_filled(Pos2::new(center.x - s * 0.72, y), 1.1, color);
                painter.line_segment(
                    [
                        Pos2::new(center.x - s * 0.38, y),
                        Pos2::new(center.x - s * 0.38 + s * 1.35 * width, y),
                    ],
                    stroke,
                );
            }
        }
        Icon::Pulse => {
            let points = vec![
                Pos2::new(center.x - s, center.y),
                Pos2::new(center.x - s * 0.45, center.y),
                Pos2::new(center.x - s * 0.18, center.y - s * 0.7),
                Pos2::new(center.x + s * 0.16, center.y + s * 0.7),
                Pos2::new(center.x + s * 0.42, center.y),
                Pos2::new(center.x + s, center.y),
            ];
            painter.add(egui::Shape::line(points, stroke));
        }
        Icon::Gear => {
            painter.circle_stroke(center, s * 0.52, stroke);
            painter.circle_filled(center, s * 0.16, color);
            for i in 0..8 {
                let angle = i as f32 * std::f32::consts::TAU / 8.0;
                let (sin, cos) = angle.sin_cos();
                painter.line_segment(
                    [
                        center + Vec2::new(cos * s * 0.6, sin * s * 0.6),
                        center + Vec2::new(cos * s, sin * s),
                    ],
                    stroke,
                );
            }
        }
        Icon::Fan => {
            // Impeller de cuatro aspas: carcasa, palas y eje.
            painter.circle_stroke(center, s * 0.94, mk_stroke(1.2, a(color, 150)));
            painter.circle_filled(center, s * 0.15, color);
            let thin = mk_stroke(1.2, a(color, 190));
            for i in 0..4 {
                let angle = i as f32 * std::f32::consts::TAU / 4.0 + 0.42;
                let (sin, cos) = angle.sin_cos();
                let perp = Vec2::new(-sin, cos);
                let root = center + Vec2::new(cos * s * 0.2, sin * s * 0.2);
                let tip = center + Vec2::new(cos * s * 0.7, sin * s * 0.7);
                painter.line_segment([root, tip], thin);
                painter.line_segment([tip, tip + perp * s * 0.22], thin);
            }
        }
    }
}

fn avatar(ui: &mut egui::Ui, name: &str, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let color = avatar_color(name);
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size * 0.5, a(color, 40));
    painter.circle_stroke(
        rect.center(),
        size * 0.5 - 0.5,
        mk_stroke(1.0, a(color, 130)),
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 0.5),
        Align2::CENTER_CENTER,
        avatar_letter(name),
        FontId::proportional(size * 0.48),
        color,
    );
    response
}

fn badge(ui: &mut egui::Ui, label: &str, fg: Color32, bg: Color32) -> Response {
    let font = FontId::proportional(9.5);
    let width = text_width(ui, label, font.clone()) + 16.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter()
        .rect_stroke(rect, 4.0, mk_stroke(1.0, a(fg, 70)), StrokeKind::Inside);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    response
}

fn kbd(ui: &mut egui::Ui, label: &str) {
    let font = FontId::monospace(9.5);
    let width = text_width(ui, label, font.clone()) + 12.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, SUNKEN);
    ui.painter()
        .rect_stroke(rect, 3.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font, MUTED);
}

fn btn(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    height: f32,
    kind: Btn,
    enabled: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = response.hovered() && enabled;
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (fill, stroke, fg) = if !enabled {
        (a(CARD, 110), BORDER_SOFT, FAINT)
    } else {
        match kind {
            Btn::Primary => (
                if hovered { BRASS_HI } else { BRASS },
                a(BRASS_HI, 200),
                INK,
            ),
            Btn::Secondary => (
                if hovered { CARD_HOVER } else { a(CARD, 230) },
                if hovered { BRASS_DIM } else { BORDER },
                TEXT,
            ),
            Btn::Ghost => (
                if hovered {
                    a(BRASS, 22)
                } else {
                    Color32::TRANSPARENT
                },
                Color32::TRANSPARENT,
                if hovered { TEXT } else { MUTED },
            ),
            Btn::Danger => (
                if hovered {
                    a(DANGER, 46)
                } else {
                    a(DANGER, 16)
                },
                a(DANGER, if hovered { 190 } else { 110 }),
                if hovered {
                    Color32::from_rgb(255, 205, 195)
                } else {
                    DANGER
                },
            ),
        }
    };
    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, 7.0, fill);
    }
    if stroke != Color32::TRANSPARENT {
        painter.rect_stroke(rect, 7.0, mk_stroke(1.0, stroke), StrokeKind::Inside);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.0),
        fg,
    );
    response
}

fn switch(ui: &mut egui::Ui, id: Id, on: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(46.0, 24.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(id, on, 0.12);
    let track = if on { a(BRASS, 70) } else { SUNKEN };
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, track);
    painter.rect_stroke(
        rect,
        12.0,
        mk_stroke(1.0, if on { BRASS } else { BORDER }),
        StrokeKind::Inside,
    );
    let knob_x = egui::lerp((rect.left() + 13.0)..=(rect.right() - 13.0), t);
    let knob = Pos2::new(knob_x, rect.center().y);
    painter.circle_filled(knob + Vec2::new(0.0, 1.0), 8.5, a(Color32::BLACK, 70));
    painter.circle_filled(knob, 8.0, if on { BRASS } else { MUTED });
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn meter(ui: &mut egui::Ui, frac: f32, color: Color32) {
    let width = ui.available_width().max(20.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 5.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.5, SUNKEN);
    let filled = (width * frac.clamp(0.0, 1.0)).max(if frac > 0.0 { 2.0 } else { 0.0 });
    if filled > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(filled, rect.height())),
            2.5,
            color,
        );
    }
}

fn card(ui: &mut egui::Ui, width: f32, body: impl FnOnce(&mut egui::Ui)) {
    let inner = (width - 28.0).max(40.0);
    let frame = egui::Frame::new()
        .fill(CARD)
        .stroke(mk_stroke(1.0, BORDER))
        .corner_radius(10.0)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(inner);
            ui.set_max_width(inner);
            body(ui);
        });
    rivets(ui, frame.response.rect);
}

fn section(ui: &mut egui::Ui, title: &str, right: Option<&str>) {
    ui.horizontal(|ui| {
        let (tick, _) = ui.allocate_exact_size(Vec2::new(3.0, 11.0), Sense::hover());
        ui.painter().rect_filled(tick, 1.5, BRASS);
        ui.add(egui::Label::new(
            RichText::new(title.to_uppercase())
                .size(9.5)
                .strong()
                .color(MUTED),
        ));
        if let Some(right) = right {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(right).size(9.5).color(FAINT)).truncate());
            });
        }
    });
    ui.add_space(7.0);
}

/// Fila clave/valor acotada al ancho disponible: la etiqueta se corta con
/// elipsis y el valor tiene una columna fija, así nunca desborda la tarjeta.
fn kv(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    let width = ui.available_width();
    let height = 19.0;
    let value_width = 118.0_f32.min(width * 0.52);
    let label_width = (width - value_width - 8.0).max(20.0);
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [label_width, height],
                egui::Label::new(RichText::new(label).size(11.0).color(MUTED)).truncate(),
            );
            ui.allocate_ui_with_layout(
                Vec2::new(value_width, height),
                egui::Layout::right_to_left(Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(value)
                                .size(11.5)
                                .monospace()
                                .strong()
                                .color(color),
                        )
                        .truncate(),
                    );
                },
            );
        },
    );
}

fn eyebrow(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label.to_uppercase())
            .size(9.0)
            .strong()
            .color(FAINT),
    );
}

fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(radius * 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(title).size(19.0).strong().color(TEXT));
        ui.add_space(1.0);
        ui.label(RichText::new(subtitle).size(10.5).color(MUTED));
    });
}

fn gauge_chip(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    frac: f32,
    color: Color32,
    tooltip: &str,
) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(100.0, 42.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, CARD);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.top() + 8.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(9.0),
        FAINT,
    );
    painter.text(
        Pos2::new(rect.right() - 10.0, rect.top() + 5.0),
        Align2::RIGHT_TOP,
        value,
        FontId::proportional(14.0),
        color,
    );
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 12.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 8.0),
    );
    painter.rect_filled(track, 2.0, SUNKEN);
    let filled = (track.width() * frac.clamp(0.0, 1.0)).max(2.0);
    painter.rect_filled(
        Rect::from_min_size(track.min, Vec2::new(filled, track.height())),
        2.0,
        color,
    );
    response.on_hover_text(tooltip);
}

fn nav_item(
    ui: &mut egui::Ui,
    compact: bool,
    selected: bool,
    icon: Icon,
    label: &str,
    hint: &str,
    width: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 8.0, a(BRASS, 20));
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.left(), rect.top() + 7.0),
                Vec2::new(3.0, rect.height() - 14.0),
            ),
            1.5,
            BRASS,
        );
    } else if hovered {
        painter.rect_filled(rect, 8.0, a(TEXT, 8));
    }
    let fg = if selected {
        BRASS
    } else if hovered {
        TEXT
    } else {
        MUTED
    };
    let icon_center = if compact {
        rect.center()
    } else {
        Pos2::new(rect.left() + 20.0, rect.center().y)
    };
    paint_icon(painter, icon_center, 16.0, icon, fg);
    if !compact {
        painter.text(
            Pos2::new(rect.left() + 38.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(12.5),
            fg,
        );
        painter.text(
            Pos2::new(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            hint,
            FontId::monospace(9.5),
            FAINT,
        );
    }
    if compact {
        response.on_hover_text(label)
    } else {
        response
    }
}

fn profile_chip(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    tip: &str,
    enabled: bool,
) -> Response {
    let font = FontId::proportional(11.5);
    let width = (text_width(ui, label, font.clone()) + 34.0).max(96.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    let hovered = response.hovered() && enabled;
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    let (fill, stroke, fg) = if !enabled {
        (a(CARD, 120), BORDER_SOFT, FAINT)
    } else if active {
        (BRASS, a(BRASS_HI, 220), INK)
    } else if hovered {
        (CARD_HOVER, BRASS_DIM, TEXT)
    } else {
        (CARD, BORDER_SOFT, MUTED)
    };
    painter.rect_filled(rect, 8.0, fill);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, stroke), StrokeKind::Inside);
    let dot_center = Pos2::new(rect.left() + 12.0, rect.center().y);
    if active {
        painter.circle_filled(dot_center, 3.0, INK);
        painter.circle_stroke(dot_center, 4.6, mk_stroke(1.0, a(INK, 120)));
    } else {
        painter.circle_stroke(dot_center, 3.2, mk_stroke(1.0, a(fg, 150)));
    }
    painter.text(
        Pos2::new(rect.left() + 23.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    response.on_hover_text(tip)
}

fn sort_cell(
    ui: &mut egui::Ui,
    label: &str,
    key: SortBy,
    sort: SortState,
    right: bool,
) -> Option<SortBy> {
    let width = ui.available_width().max(24.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let active = sort.key == key;
    let fg = if active { BRASS } else { MUTED };
    let painter = ui.painter().with_clip_rect(rect);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.rect_filled(rect, 4.0, a(CARD_HOVER, 160));
    }
    let mut text = label.to_string();
    if active {
        text.push_str(if sort.desc { " ↓" } else { " ↑" });
    }
    let font = FontId::proportional(10.0);
    let pos = if right {
        Pos2::new(rect.right() - 2.0, rect.center().y)
    } else {
        Pos2::new(rect.left() + 2.0, rect.center().y)
    };
    let align = if right {
        Align2::RIGHT_CENTER
    } else {
        Align2::LEFT_CENTER
    };
    painter.text(pos, align, text, font, fg);
    if response.clicked() {
        Some(key)
    } else {
        None
    }
}

fn header_label(ui: &mut egui::Ui, label: &str, right: bool) {
    let width = ui.available_width().max(24.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    let pos = if right {
        Pos2::new(rect.right() - 2.0, rect.center().y)
    } else {
        Pos2::new(rect.left() + 2.0, rect.center().y)
    };
    let align = if right {
        Align2::RIGHT_CENTER
    } else {
        Align2::LEFT_CENTER
    };
    painter.text(pos, align, label, FontId::proportional(10.0), MUTED);
}

// ------------------------------------------------------------------- app ---

impl DomadorUi {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        visuals.faint_bg_color = ZEBRA;
        visuals.extreme_bg_color = SUNKEN;
        visuals.code_bg_color = SUNKEN;
        visuals.override_text_color = Some(TEXT);
        visuals.selection.bg_fill = a(BRASS, 55);
        visuals.selection.stroke = mk_stroke(1.0, BRASS);
        visuals.widgets.noninteractive.bg_fill = PANEL;
        visuals.widgets.noninteractive.bg_stroke.color = BORDER_SOFT;
        visuals.widgets.noninteractive.fg_stroke = mk_stroke(1.0, MUTED);
        visuals.widgets.inactive.bg_fill = CARD;
        visuals.widgets.inactive.bg_stroke.color = BORDER_SOFT;
        visuals.widgets.inactive.fg_stroke = mk_stroke(1.0, TEXT);
        visuals.widgets.hovered.bg_fill = CARD_HOVER;
        visuals.widgets.hovered.bg_stroke.color = BRASS_DIM;
        visuals.widgets.hovered.fg_stroke = mk_stroke(1.0, TEXT);
        visuals.widgets.active.bg_fill = CARD_HOVER;
        visuals.widgets.active.bg_stroke.color = BRASS;
        visuals.widgets.active.fg_stroke = mk_stroke(1.0, BRASS);
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = egui::CornerRadius::same(7);
        }
        cc.egui_ctx.set_visuals(visuals);

        let mut style = (*cc.egui_ctx.style()).clone();
        style.animation_time = 0.12;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_outer_margin = 3.0;
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(19.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(12.5));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(11.5));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(10.5));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, FontId::monospace(11.5));
        cc.egui_ctx.set_style(style);
        // QA-TEMP: zoom y pestaña inicial por env, sólo para capturas.
        if let Ok(zoom) = std::env::var("DOMADOR_QA_ZOOM") {
            if let Ok(zoom) = zoom.trim().parse::<f32>() {
                cc.egui_ctx.set_zoom_factor(zoom);
            }
        }

        let logo = image::load_from_memory(include_bytes!("../../assets/icon-64.png"))
            .ok()
            .map(|img| {
                let img = img.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                cc.egui_ctx
                    .load_texture("domador-logo", color, egui::TextureOptions::LINEAR)
            });

        let shared = Arc::new(Mutex::new(Shared::default()));
        let poll = shared.clone();
        thread::spawn(move || poll_thread(poll));
        let legion = shared.clone();
        thread::spawn(move || legion_thread(legion));
        Self {
            shared,
            tab: if std::env::var("DOMADOR_QA_TAB").as_deref() == Ok("legion") {
                Tab::Legion
            } else {
                Tab::Processes
            },
            search: String::new(),
            sort: SortState {
                key: SortBy::Cpu,
                desc: true,
            },
            selected: None,
            kill_confirm: None,
            logo,
            focus_search: false,
            legion_ui: LegionUi::default(),
        }
    }

    fn state(&self) -> Option<Arc<Value>> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone()
    }

    fn error(&self) -> Option<String> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .error
            .clone()
    }

    fn busy(&self) -> Option<String> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .busy
            .clone()
    }

    fn toast(&self) -> Option<Toast> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .toast
            .clone()
    }

    fn detail(&self) -> Option<Detail> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .detail
            .clone()
    }

    fn close_detail(&self) {
        self.shared.lock().unwrap_or_else(|p| p.into_inner()).detail = None;
    }

    fn open_detail(&self, pid: u32) {
        {
            let mut guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.detail = Some(Detail {
                pid,
                loading: true,
                error: None,
                data: None,
            });
        }
        let shared = self.shared.clone();
        thread::spawn(move || {
            let result = get_path(&format!("/api/process?pid={pid}"));
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(detail) = guard.detail.as_mut() {
                if detail.pid == pid {
                    detail.loading = false;
                    match result {
                        Ok(value) => {
                            detail.data = Some(Arc::new(value));
                            detail.error = None;
                        }
                        Err(err) => detail.error = Some(err),
                    }
                }
            }
        });
    }

    fn refresh_bg(&self) {
        let shared = self.shared.clone();
        thread::spawn(move || {
            let result = get_state();
            store_state(&shared, result);
        });
    }

    fn action(&self, path: &'static str, body: Value, busy: &'static str, done: &'static str) {
        act_bg(self.shared.clone(), path, body, busy, done);
    }

    fn profile(&self, name: &'static str, label: &'static str) {
        let busy = match name {
            "full_power" => "Activando Full Power",
            "gaming" => "Activando modo Juego",
            "ai" => "Activando modo IA",
            _ => "Equilibrando",
        };
        self.action("/api/profile", json!({ "name": name }), busy, label);
    }

    fn set_sort(&mut self, key: SortBy) {
        if self.sort.key == key {
            self.sort.desc = !self.sort.desc;
        } else {
            self.sort = SortState {
                key,
                desc: key != SortBy::Name,
            };
        }
    }

    fn relaunch_admin(&self) {
        // Reiniciar el MOTOR, no la UI: el motor es quien necesita el token
        // elevado para hablar con el EC. La UI elevada se queda abierta y
        // vuelve a consultar el estado en el próximo poll.
        let _ = post("/api/shutdown", json!({}));
        thread::sleep(Duration::from_millis(800));
        if domador::win::is_admin() {
            spawn_engine();
        } else {
            spawn_engine_elevated();
        }
    }

    // ------------------------------------------------------------- shell ---

    fn top(&mut self, ctx: &egui::Context) {
        let state = self.state();
        let app = state.as_ref().and_then(|s| s.get("app"));
        let admin = app
            .and_then(|a| a.get("admin"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let version = app
            .and_then(|a| a.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let cpu = state
            .as_ref()
            .and_then(|s| s.pointer("/system/cpu"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ram = state
            .as_ref()
            .and_then(|s| s.pointer("/system/ram"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ram_used = state
            .as_ref()
            .and_then(|s| s.pointer("/system/ram_used"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let ram_total = state
            .as_ref()
            .and_then(|s| s.pointer("/system/ram_total"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let online = state.is_some();

        egui::TopBottomPanel::top("header")
            .exact_height(74.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::hover());
                        ui.painter().rect_filled(rect, 9.0, a(SUNKEN, 220));
                        ui.painter().rect_stroke(
                            rect,
                            9.0,
                            mk_stroke(1.0, a(BRASS_DIM, 150)),
                            StrokeKind::Inside,
                        );
                        ui.painter().image(
                            logo.id(),
                            rect.shrink(3.0),
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.label(RichText::new("DOMADOR").size(18.0).strong().color(BRASS));
                        ui.add_space(1.0);
                        ui.label(
                            RichText::new(format!("CABINA DE CONTROL · v{version}"))
                                .size(9.0)
                                .strong()
                                .color(FAINT),
                        );
                    });
                    ui.add_space(12.0);
                    if admin {
                        badge(ui, "ADMIN", OK, a(OK, 26));
                    } else {
                        badge(ui, "SIN ADMIN", WARN, a(WARN, 24));
                        ui.add_space(2.0);
                        if btn(ui, "Elevar", 66.0, 26.0, Btn::Secondary, true)
                            .on_hover_text(
                                "Reinicia la UI como administrador (UAC). Necesario para tocar procesos del sistema.",
                            )
                            .clicked()
                        {
                            self.relaunch_admin();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(if online { "MOTOR EN LÍNEA" } else { "SIN MOTOR" })
                                .size(8.5)
                                .strong()
                                .color(if online { OK } else { DANGER }),
                        );
                        ui.add_space(4.0);
                        dot(ui, if online { OK } else { DANGER }, 3.5);
                        ui.add_space(12.0);
                        gauge_chip(
                            ui,
                            "RAM",
                            &format!("{ram:.0}%"),
                            (ram / 100.0) as f32,
                            if ram > 85.0 {
                                COPPER_HI
                            } else if ram > 70.0 {
                                BRASS
                            } else {
                                OK
                            },
                            &format!("{} de {} en uso", fmt_bytes(ram_used), fmt_bytes(ram_total)),
                        );
                        ui.add_space(6.0);
                        gauge_chip(
                            ui,
                            "CPU",
                            &format!("{cpu:.0}%"),
                            (cpu / 100.0) as f32,
                            if cpu > 70.0 { COPPER_HI } else { BRASS },
                            &format!("{cpu:.1}% promedio en todos los núcleos"),
                        );
                    });
                });
                hairline(ui, ui.max_rect().bottom() - 0.5, a(BRASS, 60));
            });
    }

    fn profile_bar(&mut self, ctx: &egui::Context) {
        let state = self.state();
        let active = state
            .as_ref()
            .and_then(|s| s.pointer("/profile/active"))
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
            .to_string();
        let busy = self.busy().is_some();
        egui::TopBottomPanel::top("profiles")
            .exact_height(54.0)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    eyebrow(ui, "Perfil");
                    ui.add_space(2.0);
                    let profiles = [
                        (
                            "full_power",
                            "Full Power",
                            "Máximo rendimiento: sube prioridades y usa el plan de energía más alto.",
                        ),
                        (
                            "gaming",
                            "Juego",
                            "Prioriza la ventana en foco y baja el resto mientras jugás.",
                        ),
                        (
                            "ai",
                            "IA",
                            "Prepara el equipo para cargas largas de cálculo.",
                        ),
                        (
                            "balanced",
                            "Equilibrado",
                            "Uso general: sin cambios agresivos, todo estable.",
                        ),
                    ];
                    for (key, label, tip) in profiles {
                        let is_active = active == key;
                        if profile_chip(ui, label, is_active, tip, !busy).clicked() && !busy {
                            match key {
                                "full_power" => self.profile("full_power", "Full Power activo"),
                                "gaming" => self.profile("gaming", "Modo Juego activo"),
                                "ai" => self.profile("ai", "Modo IA activo"),
                                _ => self.profile("balanced", "Perfil Equilibrado activo"),
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("se aplica al instante")
                                .size(9.5)
                                .color(FAINT),
                        );
                    });
                });
            });
    }

    fn side(&mut self, ctx: &egui::Context) {
        let compact = ctx.screen_rect().width() < 1240.0;
        let width = if compact { 66.0 } else { 184.0 };
        let state = self.state();
        let counts = state.as_ref().and_then(|s| s.get("counts"));
        let procs = num(counts.and_then(|c| c.get("processes")));
        let rules = num(counts.and_then(|c| c.get("rules")));
        let throttled = num(counts.and_then(|c| c.get("throttled")));
        let runaways = state
            .as_ref()
            .and_then(|s| s.get("runaways"))
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let online = state.is_some();

        egui::SidePanel::left("nav")
            .exact_width(width)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(if compact { 9 } else { 12 }, 14)),
            )
            .show(ctx, |ui| {
                ui.painter().vline(
                    ui.max_rect().right() - 0.5,
                    ui.max_rect().y_range(),
                    mk_stroke(1.0, a(BORDER, 140)),
                );
                let item_width = width - if compact { 18.0 } else { 24.0 };
                if nav_item(
                    ui,
                    compact,
                    self.tab == Tab::Processes,
                    Icon::List,
                    "Procesos",
                    "1",
                    item_width,
                )
                .clicked()
                {
                    self.tab = Tab::Processes;
                }
                if nav_item(
                    ui,
                    compact,
                    self.tab == Tab::Health,
                    Icon::Pulse,
                    "Salud",
                    "2",
                    item_width,
                )
                .clicked()
                {
                    self.tab = Tab::Health;
                }
                if nav_item(
                    ui,
                    compact,
                    self.tab == Tab::Settings,
                    Icon::Gear,
                    "Ajustes",
                    "3",
                    item_width,
                )
                .clicked()
                {
                    self.tab = Tab::Settings;
                }
                if nav_item(
                    ui,
                    compact,
                    self.tab == Tab::Legion,
                    Icon::Fan,
                    "Legion",
                    "4",
                    item_width,
                )
                .clicked()
                {
                    self.tab = Tab::Legion;
                }

                ui.add_space(18.0);
                let line = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(line, 1.0), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, a(BORDER, 160));
                ui.add_space(14.0);

                if !compact {
                    eyebrow(ui, "Estado");
                    ui.add_space(6.0);
                    side_kv(ui, "Procesos", &procs.to_string());
                    side_kv(ui, "Reglas", &rules.to_string());
                    side_kv(ui, "Regulados", &throttled.to_string());
                    if runaways > 0 {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            dot(ui, COPPER_HI, 3.0);
                            ui.label(
                                RichText::new(format!("{runaways} colgado(s)"))
                                    .size(10.5)
                                    .color(COPPER_HI),
                            );
                        });
                    }
                } else {
                    ui.with_layout(egui::Layout::top_down(Align::Center), |ui| {
                        dot(ui, if online { OK } else { DANGER }, 4.0);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(procs.to_string())
                                .size(10.0)
                                .monospace()
                                .color(MUTED),
                        );
                    });
                }

                ui.with_layout(egui::Layout::bottom_up(Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(format!("127.0.0.1:{}", port()))
                            .size(9.0)
                            .monospace()
                            .color(FAINT),
                    );
                    if !compact {
                        ui.label(
                            RichText::new("DOMADOR · control de procesos")
                                .size(8.5)
                                .color(FAINT),
                        );
                    }
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let toast = self.toast();
        let busy = self.busy();
        let error = self.error();
        let state = self.state();
        let (age, uptime) = {
            let guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard.last_ok.map(|t| t.elapsed().as_secs_f32()),
                state
                    .as_ref()
                    .and_then(|s| s.pointer("/app/uptime"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
            )
        };
        let stale = state.is_some() && error.is_some();
        egui::TopBottomPanel::bottom("status")
            .exact_height(32.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(16, 7)),
            )
            .show(ctx, |ui| {
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().top() + 0.5,
                    mk_stroke(1.0, a(BORDER, 170)),
                );
                ui.horizontal(|ui| {
                    let (color, text) = if stale {
                        (WARN, "Sin respuesta del motor · últimos datos".to_string())
                    } else if state.is_some() {
                        (OK, "Motor en línea".to_string())
                    } else {
                        (DANGER, "Motor sin conexión".to_string())
                    };
                    dot(ui, color, 3.5);
                    ui.label(RichText::new(text).size(10.0).color(color));
                    if let Some(age) = age {
                        ui.label(
                            RichText::new(format!("· actualizado hace {age:.1} s"))
                                .size(9.5)
                                .color(FAINT),
                        );
                    }
                    if state.is_some() {
                        ui.label(
                            RichText::new(format!("· motor arriba {}", fmt_uptime(uptime)))
                                .size(9.5)
                                .color(FAINT),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if let Some(busy) = &busy {
                            ui.add(egui::Spinner::new().size(13.0).color(BRASS));
                            ui.label(RichText::new(busy).size(10.5).color(BRASS));
                        } else if let Some(toast) = &toast {
                            let color = if toast.ok { OK } else { COPPER_HI };
                            if ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(&toast.text).size(10.5).color(color),
                                    )
                                    .sense(Sense::click()),
                                )
                                .on_hover_text("Clic para descartar")
                                .clicked()
                            {
                                self.shared.lock().unwrap_or_else(|p| p.into_inner()).toast = None;
                            }
                        } else {
                            ui.label(
                                RichText::new("Ctrl+F buscar · Ctrl+R actualizar")
                                    .size(9.5)
                                    .color(FAINT),
                            );
                        }
                    });
                });
            });
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let (find, refresh, one, two, three, four, escape) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(egui::Key::F),
                cmd && i.key_pressed(egui::Key::R),
                cmd && i.key_pressed(egui::Key::Num1),
                cmd && i.key_pressed(egui::Key::Num2),
                cmd && i.key_pressed(egui::Key::Num3),
                cmd && i.key_pressed(egui::Key::Num4),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if find {
            self.tab = Tab::Processes;
            self.focus_search = true;
        }
        if refresh {
            self.refresh_bg();
        }
        if one {
            self.tab = Tab::Processes;
        }
        if two {
            self.tab = Tab::Health;
        }
        if three {
            self.tab = Tab::Settings;
        }
        if four {
            self.tab = Tab::Legion;
        }
        if escape
            && self.kill_confirm.is_none()
            && self.detail().is_none()
            && !self.search.is_empty()
        {
            self.search.clear();
        }
    }

    // ---------------------------------------------------------- principal ---

    fn primary(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(18, 14)),
            )
            .show(ctx, |ui| match self.tab {
                Tab::Processes => self.processes(ui),
                Tab::Health => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            ui.set_width(width);
                            self.health(ui);
                        });
                }
                Tab::Settings => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            ui.set_width(width);
                            self.settings(ui);
                        });
                }
                Tab::Legion => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            let width = ui.available_width();
                            ui.set_width(width);
                            self.legion(ui);
                        });
                }
            });
    }

    fn processes(&mut self, ui: &mut egui::Ui) {
        let state = self.state();
        let busy = self.busy().is_some();
        let total = state
            .as_ref()
            .and_then(|s| s.get("processes"))
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let visible = state
            .as_ref()
            .and_then(|s| s.pointer("/app/visible"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        self.process_toolbar(ui, total, visible, busy);
        ui.add_space(10.0);

        let runaways: Vec<(u32, String, f64)> = state
            .as_ref()
            .and_then(|s| s.get("runaways"))
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .map(|r| {
                        (
                            r.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                            r.get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string(),
                            r.get("minutes").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !runaways.is_empty() {
            self.runaway_alert(ui, &runaways);
            ui.add_space(10.0);
        }

        let table_height = (ui.available_height() - 4.0).max(200.0);
        let Some(state) = state else {
            if let Some(err) = self.error() {
                self.error_card(ui, &err);
            } else {
                self.loading_card(ui);
            }
            return;
        };

        let empty: Vec<Value> = Vec::new();
        let procs = state
            .get("processes")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let needle = self.search.trim().to_lowercase();
        let mut rows: Vec<&Value> = procs
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| n.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .collect();
        sort_rows(&mut rows, self.sort);
        self.process_table(ui, &rows, table_height, busy);
    }

    fn process_toolbar(&mut self, ui: &mut egui::Ui, total: usize, visible: u64, busy: bool) {
        ui.horizontal(|ui| {
            heading(
                ui,
                "Procesos",
                &format!(
                    "{} procesos · {} con ventana · los que más consumen primero",
                    total, visible
                ),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if btn(ui, "Actualizar", 88.0, 30.0, Btn::Secondary, !busy)
                    .on_hover_text("Vuelve a leer el motor (Ctrl+R)")
                    .clicked()
                {
                    self.refresh_bg();
                }
                ui.add_space(6.0);
                // Orden: segmento compacto, sin combo ambiguo.
                egui::Frame::new()
                    .fill(SUNKEN)
                    .stroke(mk_stroke(1.0, BORDER_SOFT))
                    .corner_radius(9.0)
                    .inner_margin(Margin::same(2))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.horizontal(|ui| {
                            for key in [SortBy::Cpu, SortBy::Ram, SortBy::Name] {
                                let active = self.sort.key == key;
                                let mut label = key.label().to_string();
                                if active {
                                    label.push_str(if self.sort.desc { " ↓" } else { " ↑" });
                                }
                                if btn(
                                    ui,
                                    &label,
                                    66.0,
                                    24.0,
                                    if active { Btn::Primary } else { Btn::Ghost },
                                    true,
                                )
                                .clicked()
                                {
                                    self.set_sort(key);
                                }
                            }
                        });
                    });
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            egui::Frame::new()
                .fill(SUNKEN)
                .stroke(mk_stroke(
                    1.0,
                    if self.focus_search { BRASS_DIM } else { BORDER },
                ))
                .corner_radius(8.0)
                .inner_margin(Margin::symmetric(9, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        let painter = ui.painter();
                        let center = rect.center();
                        painter.circle_stroke(center, 5.0, mk_stroke(1.2, FAINT));
                        painter.line_segment(
                            [center + Vec2::new(3.6, 3.6), center + Vec2::new(6.6, 6.6)],
                            mk_stroke(1.2, FAINT),
                        );
                        let search_id = Id::new("process-search");
                        let edit = egui::TextEdit::singleline(&mut self.search)
                            .id(search_id)
                            .frame(false)
                            .hint_text("Buscar proceso…")
                            .desired_width(230.0)
                            .font(FontId::proportional(12.0));
                        let response = ui.add(edit);
                        if self.focus_search {
                            response.request_focus();
                            self.focus_search = false;
                        }
                        if !self.search.is_empty()
                            && btn(ui, "×", 20.0, 18.0, Btn::Ghost, true)
                                .on_hover_text("Limpiar búsqueda")
                                .clicked()
                        {
                            self.search.clear();
                        }
                    });
                });
            if !self.search.trim().is_empty() {
                ui.label(
                    RichText::new(format!("filtrando: «{}»", self.search.trim()))
                        .size(10.0)
                        .color(BRASS),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Clic en una fila para ver qué es · las acciones destructivas piden confirmación.")
                        .size(9.5)
                        .color(FAINT),
                );
            });
        });
    }

    fn process_table(&mut self, ui: &mut egui::Ui, rows: &[&Value], height: f32, busy: bool) {
        if rows.is_empty() {
            let width = ui.available_width();
            card(ui, width, |ui| {
                empty_state(
                    ui,
                    "Sin resultados",
                    if self.search.trim().is_empty() {
                        "El motor todavía no reporta procesos."
                    } else {
                        "Ningún proceso coincide con la búsqueda. Probá con otro nombre o limpiá el filtro."
                    },
                );
                if !self.search.is_empty() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if btn(ui, "Limpiar filtro", 110.0, 26.0, Btn::Secondary, true).clicked() {
                            self.search.clear();
                        }
                    });
                }
            });
            ui.add_space(4.0);
            return;
        }

        let show_pid = ui.available_width() >= 780.0;
        let selected = self.selected;
        let sort = self.sort;
        let mut clicked_sort: Option<SortBy> = None;
        let mut clicked_action: Option<Action> = None;

        egui::Frame::new()
            .fill(PANEL)
            .stroke(mk_stroke(1.0, BORDER))
            .corner_radius(10.0)
            .inner_margin(Margin::same(1))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut builder = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(false)
                    .sense(egui::Sense::click())
                    .vscroll(true)
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(Align::Center))
                    .max_scroll_height(height)
                    .column(Column::remainder().at_least(180.0).clip(true));
                if show_pid {
                    builder = builder.column(Column::exact(58.0));
                }
                builder = builder
                    .column(Column::exact(70.0))
                    .column(Column::exact(86.0))
                    .column(Column::exact(94.0))
                    .column(Column::exact(238.0));
                let table = builder.header(30.0, |mut header| {
                    header.col(|ui| {
                        if let Some(key) = sort_cell(ui, "Proceso", SortBy::Name, sort, false) {
                            clicked_sort = Some(key);
                        }
                    });
                    if show_pid {
                        header.col(|ui| {
                            header_label(ui, "PID", false);
                        });
                    }
                    header.col(|ui| {
                        if let Some(key) = sort_cell(ui, "CPU", SortBy::Cpu, sort, true) {
                            clicked_sort = Some(key);
                        }
                    });
                    header.col(|ui| {
                        if let Some(key) = sort_cell(ui, "RAM", SortBy::Ram, sort, true) {
                            clicked_sort = Some(key);
                        }
                    });
                    header.col(|ui| {
                        header_label(ui, "Prioridad", false);
                    });
                    header.col(|ui| {
                        header_label(ui, "Acciones", false);
                    });
                });
                table.body(|body| {
                    body.rows(34.0, rows.len(), |mut row| {
                        let index = row.index();
                        let p = rows[index];
                        let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let display = name.strip_suffix(".exe").unwrap_or(name);
                        let exe = p.get("exe").and_then(|v| v.as_str()).unwrap_or("");
                        let cpu = p.get("cpu").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let rss = p.get("rss").and_then(|v| v.as_u64()).unwrap_or(0);
                        let prio = p.get("prio").and_then(|v| v.as_str()).unwrap_or("normal");
                        let prio_label = p
                            .get("prio_label")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Normal");
                        let protected = p
                            .get("protected")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let is_selected = selected == Some(pid);
                        let mut row_acted = false;
                        row.set_selected(is_selected);
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                avatar(ui, name, 22.0);
                                let label = egui::Label::new(
                                    RichText::new(display)
                                        .size(12.0)
                                        .color(if is_selected { BRASS } else { TEXT }),
                                )
                                .truncate()
                                .sense(Sense::click());
                                let resp = ui.add(label);
                                // El nombre es un blanco de clic explícito: abre la
                                // ficha sin depender del sense de la fila entera.
                                if resp.clicked() {
                                    self.selected = Some(pid);
                                    self.open_detail(pid);
                                    row_acted = true;
                                }
                                let tip = if exe.is_empty() {
                                    format!("{name} · PID {pid}\nClic para ver la ficha")
                                } else {
                                    format!("{name} · PID {pid}\n{exe}\nClic para ver la ficha")
                                };
                                resp.on_hover_text(tip);
                            });
                        });
                        if show_pid {
                            row.col(|ui| {
                                ui.with_layout(
                                    egui::Layout::right_to_left(Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(pid.to_string())
                                                .size(10.5)
                                                .monospace()
                                                .color(FAINT),
                                        );
                                    },
                                );
                            });
                        }
                        row.col(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("{cpu:.1}%"))
                                        .size(11.0)
                                        .monospace()
                                        .color(cpu_color(cpu)),
                                );
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(fmt_bytes(rss))
                                        .size(10.5)
                                        .monospace()
                                        .color(MUTED),
                                );
                            });
                        });
                        row.col(|ui| {
                            if protected {
                                badge(ui, "Sistema", FAINT, a(FAINT, 22))
                                    .on_hover_text("Proceso protegido: Domador no lo modifica");
                            } else {
                                let (fg, bg) = prio_colors(prio);
                                badge(ui, prio_label, fg, bg).on_hover_text(format!(
                                    "Prioridad actual: {prio_label}"
                                ));
                            }
                        });
                        row.col(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.horizontal(|ui| {
                                if btn(
                                    ui,
                                    "Subir",
                                    52.0,
                                    24.0,
                                    Btn::Secondary,
                                    !busy && !protected,
                                )
                                .on_hover_text(if protected {
                                    "Proceso protegido: el sistema rechaza el cambio"
                                } else {
                                    "Subir prioridad a Alta"
                                })
                                .clicked()
                                {
                                    clicked_action = Some(Action::Priority(pid));
                                    row_acted = true;
                                }
                                if btn(ui, "Liberar", 60.0, 24.0, Btn::Secondary, !busy)
                                    .on_hover_text(
                                        "Libera el working set. No cierra la app; Windows lo recarga si hace falta.",
                                    )
                                    .clicked()
                                {
                                    clicked_action = Some(Action::Trim(pid));
                                    row_acted = true;
                                }
                                if btn(ui, "Restaurar", 72.0, 24.0, Btn::Ghost, !busy)
                                    .on_hover_text("Vuelve a la prioridad original")
                                    .clicked()
                                {
                                    clicked_action = Some(Action::Restore(pid));
                                    row_acted = true;
                                }
                                if btn(ui, "Fin", 38.0, 24.0, Btn::Danger, !busy && !protected)
                                    .on_hover_text(if protected {
                                        "Proceso protegido: no se puede terminar"
                                    } else {
                                        "Terminar el proceso (pide confirmación)"
                                    })
                                    .clicked()
                                {
                                    clicked_action = Some(Action::Kill(pid, name.to_string()));
                                    row_acted = true;
                                }
                            });
                        });
                        let resp = row
                            .response()
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() && !row_acted {
                            self.selected = Some(pid);
                            self.open_detail(pid);
                        }
                    });
                });
            });

        if let Some(key) = clicked_sort {
            self.set_sort(key);
        }
        match clicked_action {
            Some(Action::Priority(pid)) => self.action(
                "/api/priority",
                json!({ "pid": pid, "cls": "high" }),
                "Subiendo prioridad",
                "Prioridad alta aplicada",
            ),
            Some(Action::Trim(pid)) => self.action(
                "/api/trim",
                json!({ "pid": pid }),
                "Liberando RAM",
                "Working set liberado",
            ),
            Some(Action::Restore(pid)) => self.action(
                "/api/restore",
                json!({ "pid": pid }),
                "Restaurando",
                "Prioridad original restaurada",
            ),
            Some(Action::Kill(pid, name)) => {
                self.kill_confirm = Some((pid, name));
            }
            None => {}
        }
    }

    fn runaway_alert(&mut self, ui: &mut egui::Ui, runaways: &[(u32, String, f64)]) {
        let width = ui.available_width();
        let inner = (width - 28.0).max(40.0);
        let frame = egui::Frame::new()
            .fill(a(COPPER, 22))
            .stroke(mk_stroke(1.0, a(COPPER, 120)))
            .corner_radius(10.0)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(inner);
                ui.set_max_width(inner);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("PROCESOS COLGADOS")
                            .size(9.5)
                            .strong()
                            .color(COPPER_HI),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} sin responder. Revisalos antes de cerrar algo.",
                            runaways.len()
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                });
                ui.add_space(6.0);
                for (pid, name, minutes) in runaways.iter().take(4) {
                    ui.allocate_ui_with_layout(
                        Vec2::new(inner, 26.0),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add_sized(
                                [(inner - 76.0).max(40.0), 24.0],
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{}  ·  PID {}  ·  {:.0} min",
                                        name, pid, minutes
                                    ))
                                    .size(11.0)
                                    .color(TEXT),
                                )
                                .truncate(),
                            )
                            .on_hover_text(name);
                            if btn(ui, "Fin", 66.0, 24.0, Btn::Danger, true)
                                .on_hover_text("Terminar este proceso")
                                .clicked()
                            {
                                self.kill_confirm = Some((*pid, name.clone()));
                            }
                        },
                    );
                }
            });
        rivets(ui, frame.response.rect);
    }

    fn loading_card(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        card(ui, width, |ui| {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(22.0).color(BRASS));
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Conectando con el motor…")
                        .size(13.0)
                        .color(TEXT),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new("Domador arranca el motor local si no está corriendo.")
                        .size(10.5)
                        .color(MUTED),
                );
            });
            ui.add_space(18.0);
        });
    }

    fn error_card(&mut self, ui: &mut egui::Ui, error: &str) {
        let width = ui.available_width();
        let frame = egui::Frame::new()
            .fill(CARD)
            .stroke(mk_stroke(1.0, a(DANGER, 140)))
            .corner_radius(10.0)
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_width((width - 32.0).max(40.0));
                ui.horizontal(|ui| {
                    dot(ui, DANGER, 4.0);
                    ui.label(
                        RichText::new("No pude hablar con el motor")
                            .size(13.0)
                            .strong()
                            .color(TEXT),
                    );
                });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(RichText::new(error).size(10.5).monospace().color(COPPER_HI))
                        .wrap(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Probá ejecutar `domador run --no-open` en una terminal y volvé a intentar.",
                    )
                    .size(10.5)
                    .color(MUTED),
                );
                ui.add_space(10.0);
                if btn(ui, "Reintentar", 104.0, 28.0, Btn::Primary, true).clicked() {
                    self.refresh_bg();
                }
            });
        rivets(ui, frame.response.rect);
    }

    // ------------------------------------------------------------- salud ---

    fn health(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy().is_some();
        ui.horizontal(|ui| {
            heading(
                ui,
                "Salud",
                "Memoria real, GPU y procesos que el Task Manager no muestra.",
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if btn(
                    ui,
                    "Aliviar memoria de fondo",
                    180.0,
                    30.0,
                    Btn::Secondary,
                    !busy,
                )
                .on_hover_text(
                    "Recorta el working set de procesos ocultos y con CPU baja. No cierra nada.",
                )
                .clicked()
                {
                    self.action(
                        "/api/trim-idle",
                        json!({}),
                        "Liberando memoria de fondo",
                        "Memoria de fondo liberada",
                    );
                }
            });
        });
        ui.add_space(12.0);

        let Some(state) = self.state() else {
            if let Some(err) = self.error() {
                self.error_card(ui, &err);
            } else {
                self.loading_card(ui);
            }
            return;
        };

        let width = ui.available_width();
        if width >= 720.0 {
            let col = ((width - 12.0) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| memory_card(ui, col, &state),
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| gpu_card(ui, col, &state),
                );
            });
        } else {
            memory_card(ui, width, &state);
            ui.add_space(12.0);
            gpu_card(ui, width, &state);
        }

        ui.add_space(12.0);
        private_card(ui, width, &state);
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "La caché y el standby no son RAM perdida: Windows los reutiliza. Domador sólo recorta lo que nadie está usando.",
            )
            .size(9.5)
            .color(FAINT),
        );
    }

    // ----------------------------------------------------------- ajustes ---

    fn settings(&mut self, ui: &mut egui::Ui) {
        let state = self.state();
        let busy = self.busy().is_some();
        ui.horizontal(|ui| {
            heading(
                ui,
                "Ajustes",
                "Rendimiento, permisos y ubicación del motor.",
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Los cambios se guardan en el config local")
                        .size(9.5)
                        .color(FAINT),
                );
            });
        });
        ui.add_space(12.0);

        let enabled = state
            .as_ref()
            .and_then(|s| s.pointer("/probalance/enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let admin = state
            .as_ref()
            .and_then(|s| s.pointer("/app/admin"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut toggle_pb = false;

        let width = ui.available_width();
        if width >= 720.0 {
            let col = ((width - 12.0) / 2.0).floor();
            let mut left_toggle = false;
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| {
                        card(ui, col, |ui| {
                            section(ui, "Rendimiento", Some("ProBalanza"));
                            ui.label(
                                RichText::new(
                                    "Baja la prioridad de los procesos que se comen la CPU y la devuelve cuando se calman.",
                                )
                                .size(11.0)
                                .color(MUTED),
                            );
                            ui.add_space(10.0);
                            let inner = ui.available_width();
                            ui.allocate_ui_with_layout(
                                Vec2::new(inner, 30.0),
                                egui::Layout::left_to_right(Align::Center),
                                |ui| {
                                    let response = switch(ui, Id::new("prob-switch"), enabled);
                                    if response.clicked() && !busy {
                                        left_toggle = true;
                                    }
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(if enabled { "Activado" } else { "Desactivado" })
                                            .size(12.0)
                                            .strong()
                                            .color(if enabled { BRASS } else { MUTED }),
                                    );
                                },
                            );
                            ui.add_space(10.0);
                            if let Some(pb) = state.as_ref().and_then(|s| s.get("probalance")) {
                                let thr = fnum(pb.get("threshold"));
                                let sust = num(pb.get("sustain_s"));
                                let fast = fnum(pb.get("fast_threshold"));
                                let fast_s = num(pb.get("fast_sustain_s"));
                                let low = fnum(pb.get("restore_below"));
                                let low_s = num(pb.get("restore_s"));
                                kv(ui, "Umbral", &format!("{thr:.0}% · {sust} s"), TEXT);
                                kv(ui, "Pico", &format!("{fast:.0}% · {fast_s} s"), TEXT);
                                kv(ui, "Restaura", &format!("<{low:.0}% · {low_s} s"), TEXT);
                                let prio = text_of(pb.get("priority")).replace('_', " ");
                                kv(ui, "Prioridad", &prio, MUTED);
                                kv(
                                    ui,
                                    "EcoQoS",
                                    if pb.get("eco").and_then(|v| v.as_bool()).unwrap_or(false) {
                                        "Sí"
                                    } else {
                                        "No"
                                    },
                                    MUTED,
                                );
                            }
                        });
                    },
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| {
                        card(ui, col, |ui| {
                            section(ui, "Motor y permisos", None);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Estado").size(11.0).color(MUTED));
                                if admin {
                                    badge(ui, "ADMIN", OK, a(OK, 26));
                                } else {
                                    badge(ui, "SIN ADMIN", WARN, a(WARN, 24));
                                }
                            });
                            ui.add_space(6.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(if admin {
                                        "Podés cambiar prioridades de procesos del sistema y crear tareas elevadas."
                                    } else {
                                        "Algunos procesos del sistema van a rechazar los cambios hasta que eleves la UI."
                                    })
                                    .size(11.0)
                                    .color(MUTED),
                                )
                                .wrap(),
                            );
                            if let Some(s) = state.as_ref() {
                                let version = s
                                    .pointer("/app/version")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let path = s
                                    .pointer("/app/config_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("—");
                                let tick = fnum(s.pointer("/app/tick_ms/p95"));
                                ui.add_space(8.0);
                                kv(ui, "Versión", version, TEXT);
                                if tick > 0.0 {
                                    kv(ui, "Tick p95", &format!("{tick:.1} ms"), MUTED);
                                }
                                ui.add_space(4.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            ellipsize(
                                                ui,
                                                path,
                                                FontId::monospace(9.5),
                                                (ui.available_width() - 8.0).max(40.0),
                                            ),
                                        )
                                        .monospace()
                                        .size(9.5)
                                        .color(MUTED),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(path);
                            }
                            ui.add_space(10.0);
                            if admin {
                                if btn(
                                    ui,
                                    "Hacer permanente",
                                    ui.available_width(),
                                    30.0,
                                    Btn::Primary,
                                    !busy,
                                )
                                .on_hover_text("Crea la tarea de inicio elevada (UAC una sola vez)")
                                .clicked()
                                {
                                    self.action(
                                        "/api/admin-task",
                                        json!({}),
                                        "Creando tarea de inicio",
                                        "Tarea de inicio creada",
                                    );
                                }
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(
                                        "Si Windows pide permiso, aceptá el UAC para que la tarea quede registrada.",
                                    )
                                    .size(9.5)
                                    .color(FAINT),
                                );
                            } else if btn(
                                ui,
                                "Reiniciar como administrador",
                                ui.available_width(),
                                30.0,
                                Btn::Secondary,
                                true,
                            )
                            .on_hover_text("Cierra la UI y la vuelve a abrir elevada (UAC)")
                            .clicked()
                            {
                                self.relaunch_admin();
                            }
                        });
                    },
                );
            });
            if left_toggle {
                toggle_pb = true;
            }
        } else {
            card(ui, width, |ui| {
                section(ui, "Rendimiento", Some("ProBalanza"));
                ui.horizontal(|ui| {
                    let response = switch(ui, Id::new("prob-switch"), enabled);
                    if response.clicked() && !busy {
                        toggle_pb = true;
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if enabled { "Activado" } else { "Desactivado" })
                            .size(12.0)
                            .strong()
                            .color(if enabled { BRASS } else { MUTED }),
                    );
                });
            });
            ui.add_space(12.0);
            card(ui, width, |ui| {
                section(ui, "Administrador", None);
                if admin {
                    badge(ui, "ADMIN", OK, a(OK, 26));
                } else {
                    badge(ui, "SIN ADMIN", WARN, a(WARN, 24));
                }
            });
        }

        if toggle_pb {
            self.action(
                "/api/probalance",
                json!({ "enabled": !enabled }),
                "Guardando ProBalanza",
                if enabled {
                    "ProBalanza desactivado"
                } else {
                    "ProBalanza activado"
                },
            );
        }

        ui.add_space(12.0);
        shortcuts_card(ui, width);
    }

    // ------------------------------------------------------------- legion ---

    fn refresh_legion(&self) {
        let shared = self.shared.clone();
        thread::spawn(move || {
            let result = get_legion();
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            match result {
                Ok(value) => {
                    guard.legion.data = Some(Arc::new(value));
                    guard.legion.error = None;
                }
                Err(err) => guard.legion.error = Some(err),
            }
        });
    }

    fn legion_action(&self, path: &'static str, body: Value, busy: String, done: String) {
        push_legion_action(self.shared.clone(), path, body, busy, done);
    }

    /// POST del lazo AUTO: manda el parche y relee el estado, sin bloquear.
    fn adaptive_action(&self, body: Value) {
        let shared = self.shared.clone();
        thread::spawn(move || {
            let result = post("/api/legion/adaptive", body);
            {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.toast = Some(Toast {
                    text: match &result {
                        Ok(_) => "AUTO actualizado".to_string(),
                        Err(err) => friendly_error(err),
                    },
                    ok: result.is_ok(),
                    at: Instant::now(),
                });
            }
            refresh_adaptive(&shared);
        });
    }

    /// Aplica lo que pidió el dibujo de telemetría y AUTO. El refresco
    /// automático vive acá porque esta función sólo corre con la pestaña
    /// abierta: si el usuario está en Procesos, no se pide nada.
    fn telemetry_data_actions(
        &self,
        tele: &Telemetry,
        _adaptive: &Adaptive,
        range: u32,
        auto: bool,
        intents: &mut LegionIntent,
    ) {
        if intents.tele_export {
            let samples = tele
                .data
                .as_ref()
                .map(|data| data.samples.clone())
                .unwrap_or_default();
            export_telemetry(self.shared.clone(), range, samples);
        }
        if intents.adaptive_fetch {
            // Un hilo aparte: el GET no puede frenar el frame.
            let shared = self.shared.clone();
            thread::spawn(move || refresh_adaptive(&shared));
        }
        if let Some(body) = intents.adaptive.take() {
            self.adaptive_action(body);
        }
        // 6 s cuando hay datos, 12 s cuando falló (404 incluido): un endpoint
        // que no existe no se pide en cada frame.
        let interval = if tele.error.is_some() {
            Duration::from_secs(12)
        } else {
            Duration::from_secs(6)
        };
        let due = tele
            .attempted
            .map(|at| at.elapsed() >= interval)
            .unwrap_or(true);
        let range_changed = tele.minutes != range;
        if !tele.loading && (intents.tele_fetch || range_changed || (auto && due)) {
            refresh_telemetry(&self.shared, range);
        }
    }

    fn legion(&mut self, ui: &mut egui::Ui) {
        let (data, err, legion_busy, action, tele, adaptive) = {
            let guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard.legion.data.clone(),
                guard.legion.error.clone(),
                guard.legion.busy.clone(),
                guard.legion.action.clone(),
                guard.telemetry.clone(),
                guard.adaptive.clone(),
            )
        };
        // Los borradores salen de `self` mientras dibujamos para que las
        // tarjetas no necesiten `&mut self`; vuelven al final.
        let mut lstate = std::mem::take(&mut self.legion_ui);
        let mut intents = LegionIntent::default();
        let busy = self.busy().is_some() || legion_busy.is_some();
        let state = self.state();
        let motor = state.is_some();
        let admin = data
            .as_deref()
            .and_then(|d| jbool(d, &["admin"]))
            .or_else(|| {
                state
                    .as_ref()
                    .and_then(|s| s.pointer("/app/admin"))
                    .and_then(|v| v.as_bool())
            })
            .unwrap_or(false);
        let closed = action.as_deref().map(closed_names).unwrap_or_default();
        let view = data.as_deref().map(LegionView::from);
        let width = ui.available_width();

        ui.horizontal(|ui| {
            heading(
                ui,
                "Legion",
                "Ventiladores, modo térmico y límites de CPU por el controlador del equipo.",
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if btn(ui, "Actualizar", 104.0, 28.0, Btn::Secondary, !busy)
                    .on_hover_text("Relee /api/legion ahora mismo")
                    .clicked()
                {
                    intents.refresh = true;
                }
                if admin {
                    badge(ui, "ADMIN", OK, a(OK, 26));
                } else {
                    badge(ui, "SIN ADMIN", WARN, a(WARN, 24));
                }
            });
        });
        ui.add_space(8.0);
        if let Some(view) = view.as_ref() {
            let cpu = view
                .temp_cpu
                .map(|v| format!("{v:.0} °C"))
                .unwrap_or_else(|| "—".to_string());
            let gpu = view
                .temp_gpu
                .map(|v| format!("{v:.0} °C"))
                .unwrap_or_else(|| "—".to_string());
            ui.horizontal(|ui| {
                let reported = view
                    .fans
                    .iter()
                    .filter(|fan| fan.rpm > 0 || fan.max > 0)
                    .count();
                dot(ui, if view.available { OK } else { WARN }, 3.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} · BIOS {} · CPU {cpu} · GPU {gpu} · {} fan(s) reportado(s) · {}",
                            view.model,
                            view.bios,
                            reported,
                            if view.available {
                                "WMI OK"
                            } else {
                                "WMI limitado"
                            }
                        ))
                        .size(10.0)
                        .color(MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(format!(
                    "{} · BIOS {} · {}{}",
                    view.model,
                    view.bios,
                    if admin { "ADMIN" } else { "SIN ADMIN" },
                    view.note
                        .as_deref()
                        .map(|n| format!(" · {n}"))
                        .unwrap_or_default()
                ));
            });
        }
        if let Some(view) = view.as_ref() {
            if !view.warnings.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Lecturas limitadas por el BIOS: {}",
                            view.warnings.join(" · ")
                        ))
                        .size(9.5)
                        .color(FAINT),
                    )
                    .wrap(),
                );
                ui.add_space(6.0);
            }
        }
        ui.add_space(12.0);
        hardware_guide(ui);

        // Avisos: primero lo que bloquea, después lo que informa.
        if !motor {
            notice(
                ui,
                width,
                "Motor sin conexión",
                "La UI no encuentra el motor en 127.0.0.1:8787. Arrancalo con `domador run --no-open`: la pestaña se llena sola cuando responda.",
                DANGER,
            );
            ui.add_space(10.0);
        }
        if let Some(err) = &err {
            notice(ui, width, err.title(), &err.body(), err.color());
            ui.add_space(10.0);
        }
        if motor && !admin {
            admin_notice(ui, width, busy, &mut intents);
            ui.add_space(10.0);
        } else if let Some(view) = &view {
            if !view.available {
                notice(
                    ui,
                    width,
                    "WMI del EC no disponible",
                    "El motor responde pero Windows no expone el controlador del EC: no hay RPM ni límites que leer. Los controles quedan apagados para no mandar órdenes a ciegas.",
                    WARN,
                );
                ui.add_space(10.0);
            }
            if let Some(note) = view.note.as_deref() {
                notice(ui, width, "El EC reportó una advertencia", note, COPPER_HI);
                ui.add_space(10.0);
            }
        }

        let Some(view) = view.as_ref() else {
            legion_preview(ui, width);
            ui.add_space(12.0);
            // La telemetría y el lazo AUTO no dependen de `/api/legion`: se
            // dibujan igual para que la pestaña sirva aunque el panel del EC
            // todavía no exista.
            legion_analysis_section(ui, width, &adaptive, &tele, &mut lstate, &mut intents);
            if intents.elevate {
                self.relaunch_admin();
            }
            if intents.refresh {
                self.refresh_legion();
            }
            self.telemetry_data_actions(
                &tele,
                &adaptive,
                lstate.tele_range,
                lstate.tele_auto,
                &mut intents,
            );
            self.legion_ui = lstate;
            return;
        };

        // Sincronizamos los borradores con el EC salvo que el usuario esté
        // editando: así el polling no le borra lo que acaba de mover.
        if !lstate.cpu_edited {
            lstate.cpu_long = view.long.value;
            lstate.cpu_short = view.short.value;
        }
        if !lstate.curve_edited {
            for (i, slot) in lstate.curve.iter_mut().enumerate() {
                *slot = view.table.get(i).copied().unwrap_or(5).clamp(1, 10);
            }
        }
        // El lazo AUTO se sincroniza igual, salvo que el usuario esté moviendo
        // los escalones o el endpoint todavía no exista (404).
        let adaptive_pending = adaptive
            .error
            .as_ref()
            .map(|err| err.pending())
            .unwrap_or(false);
        if !adaptive_pending && !lstate.ad_edited {
            if let Some(data) = adaptive.data.as_deref() {
                lstate.ad_enabled = data.enabled;
                lstate.ad_target = data.target_c.clamp(60, 100);
                lstate.ad_hot = data.hot_c.clamp(65, 105);
                lstate.ad_critical = data.critical_c.clamp(70, 110);
            }
        }
        let enabled = !busy && admin && view.available;

        let fans_ready = view.fans.iter().any(|fan| fan.rpm > 0 || fan.max > 0);
        let curve_ready = view.table.len() >= 10;
        if width >= 760.0 {
            let col = ((width - 12.0) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| {
                        legion_thermal_card(ui, col, view, &mut intents, enabled);
                        if fans_ready {
                            ui.add_space(12.0);
                            legion_fans_card(ui, col, view, &mut intents, enabled);
                        }
                    },
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| {
                        legion_cpu_card(ui, col, view, &mut lstate, &mut intents, enabled);
                    },
                );
            });
        } else {
            legion_thermal_card(ui, width, view, &mut intents, enabled);
            if fans_ready {
                ui.add_space(12.0);
                legion_fans_card(ui, width, view, &mut intents, enabled);
            }
            ui.add_space(12.0);
            legion_cpu_card(ui, width, view, &mut lstate, &mut intents, enabled);
        }

        if curve_ready {
            ui.add_space(10.0);
            egui::CollapsingHeader::new("Avanzado · curva de fans y restauración")
                .default_open(false)
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    legion_curve_card(ui, width, view, &mut lstate, &mut intents, enabled);
                    ui.add_space(12.0);
                    legion_restore_strip(ui, width, &mut lstate, &mut intents, enabled);
                });
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Modo Jefe · quién manda en el EC")
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(6.0);
                legion_chief_card(ui, width, view, &closed, &mut intents, enabled);
            });

        ui.add_space(8.0);
        legion_analysis_section(ui, width, &adaptive, &tele, &mut lstate, &mut intents);

        // Recién acá se disparan las acciones: el dibujo nunca manda requests.
        self.telemetry_data_actions(
            &tele,
            &adaptive,
            lstate.tele_range,
            lstate.tele_auto,
            &mut intents,
        );
        if intents.elevate {
            self.relaunch_admin();
        }
        if intents.refresh {
            self.refresh_legion();
        }
        if let Some(mode) = intents.thermal {
            let label = view
                .modes
                .iter()
                .find(|(id, _)| *id == mode)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| pretty(&mode));
            self.legion_action(
                "/api/legion/thermal",
                json!({ "mode": mode }),
                format!("Cambiando a modo {label}"),
                format!("Modo térmico: {label}"),
            );
        }
        if let Some(full) = intents.full_speed {
            self.legion_action(
                "/api/legion/fans",
                json!({ "full_speed": full }),
                if full {
                    "Subiendo ventiladores al máximo".to_string()
                } else {
                    "Soltando full speed".to_string()
                },
                if full {
                    "Máxima refrigeración activa".to_string()
                } else {
                    "Full speed apagado".to_string()
                },
            );
        }
        if let Some((long, short)) = intents.cpu {
            self.legion_action(
                "/api/legion/cpu",
                json!({ "long": long, "short": short }),
                format!("Aplicando {long} W / {short} W"),
                format!("Límites: {long} W largo · {short} W corto"),
            );
            lstate.cpu_edited = false;
            lstate.pending_cpu = None;
        }
        if let Some(table) = intents.curve {
            self.legion_action(
                "/api/legion/fans",
                json!({ "table": table }),
                "Aplicando curva de ventiladores".to_string(),
                "Curva de ventiladores aplicada".to_string(),
            );
            lstate.curve_edited = false;
        }
        if let Some(on) = intents.chief {
            self.legion_action(
                "/api/legion/chief",
                json!({ "enabled": on }),
                if on {
                    "Activando Modo Jefe".to_string()
                } else {
                    "Soltando el EC".to_string()
                },
                if on {
                    "Modo Jefe activo: el EC queda para Domador".to_string()
                } else {
                    "Modo Jefe apagado".to_string()
                },
            );
        }
        if intents.restore {
            self.legion_action(
                "/api/legion/restore",
                json!({}),
                "Restaurando valores de fábrica".to_string(),
                "Valores de fábrica restaurados".to_string(),
            );
            lstate.cpu_edited = false;
            lstate.curve_edited = false;
            lstate.confirm_restore = false;
        }
        self.legion_ui = lstate;
    }

    fn kill_dialog(&mut self, ctx: &egui::Context) {
        let Some((pid, name)) = self.kill_confirm.clone() else {
            return;
        };
        let mut close = false;
        let frame = egui::Frame::new()
            .fill(PANEL)
            .stroke(mk_stroke(1.0, a(DANGER, 120)))
            .corner_radius(12.0)
            .inner_margin(Margin::same(20));
        let modal = egui::Modal::new(Id::new("kill-modal"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 11.0, a(DANGER, 30));
                    ui.painter()
                        .circle_stroke(rect.center(), 11.0, mk_stroke(1.0, a(DANGER, 150)));
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "!",
                        FontId::proportional(14.0),
                        DANGER,
                    );
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Terminar proceso")
                                .size(14.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(format!("{name} · PID {pid}"))
                                .size(10.5)
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(10.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Se cierra ahora mismo. Perdés lo que no esté guardado y puede arrastrar a otros procesos de la misma app.",
                        )
                        .size(11.0)
                        .color(MUTED),
                    )
                    .wrap(),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if btn(ui, "Cancelar", 96.0, 30.0, Btn::Secondary, true).clicked() {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if btn(ui, "Terminar", 104.0, 30.0, Btn::Danger, true).clicked() {
                            self.action(
                                "/api/kill",
                                json!({ "pid": pid }),
                                "Terminando proceso",
                                "Proceso terminado",
                            );
                            close = true;
                        }
                    });
                });
            });
        if modal.should_close() {
            close = true;
        }
        if close {
            self.kill_confirm = None;
        }
    }

    fn detail_modal(&mut self, ctx: &egui::Context) {
        let Some(detail) = self.detail() else {
            return;
        };
        let snapshot = self.state().and_then(|s| {
            s.get("processes")
                .and_then(|v| v.as_array())
                .and_then(|list| {
                    list.iter()
                        .find(|p| p.get("pid").and_then(|v| v.as_u64()) == Some(detail.pid as u64))
                        .cloned()
                })
        });
        let snap = snapshot.as_ref();
        let snap_name = snap
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("proceso");
        let display = snap_name.strip_suffix(".exe").unwrap_or(snap_name);
        let snap_exe = snap
            .and_then(|p| p.get("exe"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let cpu = fnum(snap.and_then(|p| p.get("cpu")));
        let rss = num(snap.and_then(|p| p.get("rss")));
        let private = num(snap.and_then(|p| p.get("private")));
        let protected = snap
            .and_then(|p| p.get("protected"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let prio_label = snap
            .and_then(|p| p.get("prio_label"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Normal");
        let busy = self.busy().is_some();
        let mut close = false;
        let mut retry = false;
        let mut act: Option<Action> = None;

        let frame = egui::Frame::new()
            .fill(PANEL)
            .stroke(mk_stroke(1.0, a(BRASS_DIM, 160)))
            .corner_radius(12.0)
            .inner_margin(Margin::same(18));
        let modal = egui::Modal::new(Id::new("detail-modal"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.horizontal(|ui| {
                    avatar(ui, snap_name, 30.0);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(display).size(15.0).strong().color(TEXT),
                            )
                            .truncate(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "PID {} · prioridad {} · {}{}",
                                detail.pid,
                                prio_label,
                                fmt_bytes(rss),
                                if protected { " · protegido" } else { "" }
                            ))
                            .size(10.0)
                            .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if btn(ui, "Cerrar", 72.0, 26.0, Btn::Ghost, true)
                            .on_hover_text("Cerrar la ficha (Esc)")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "CPU {:.1}%  ·  RAM {}  ·  privada {}",
                        cpu,
                        fmt_bytes(rss),
                        fmt_bytes(private)
                    ))
                    .size(10.0)
                    .monospace()
                    .color(MUTED),
                );
                ui.add_space(10.0);
                let (line, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    Sense::hover(),
                );
                ui.painter().rect_filled(line, 0.0, a(BORDER, 180));
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .max_height(430.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let width = ui.available_width();
                        ui.set_width(width);
                        if detail.loading {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.add(egui::Spinner::new().size(20.0).color(BRASS));
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("Consultando la ficha al motor…")
                                        .size(11.0)
                                        .color(MUTED),
                                );
                            });
                            ui.add_space(16.0);
                            return;
                        }
                        if let Some(data) = detail.data.as_deref() {
                            let known = jstr(data, &["known_description"]);
                            let description = known.or_else(|| {
                                jstr(
                                    data,
                                    &["description", "file_description", "details", "summary"],
                                )
                            });
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("QUÉ ES").size(9.0).strong().color(FAINT),
                            );
                            ui.add_space(3.0);
                            match description {
                                Some(text) => {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text).size(12.0).color(TEXT),
                                        )
                                        .wrap(),
                                    );
                                }
                                None => {
                                    detail_note(
                                        ui,
                                        width,
                                        "Sin descripción del fabricante",
                                        "El ejecutable no declara descripción. Identificalo por la ruta, el proceso padre y la firma digital.",
                                        true,
                                    );
                                }
                            }

                            if jbool(data, &["limited"]) == Some(true) {
                                detail_note(
                                    ui,
                                    width,
                                    "Ficha limitada",
                                    "Windows no deja leer ruta ni firma de este proceso sin admin. Elevá Domador para completar la ficha; el nombre y el padre ya son reales.",
                                    true,
                                );
                            }

                            let company = jstr(data, &["company", "company_name", "publisher"]);
                            let product = jstr(data, &["product", "product_name"]);
                            let version = jstr(data, &["version", "file_version", "product_version"]);
                            let mut maker = Vec::new();
                            if let Some(company) = company {
                                maker.push(company.to_string());
                            }
                            if let Some(product) = product {
                                if company.map(|c| !product.contains(c)).unwrap_or(true) {
                                    maker.push(product.to_string());
                                }
                            }
                            if let Some(version) = version {
                                maker.push(format!("v{version}"));
                            }
                            let maker_text = maker.join(" · ");
                            detail_field(
                                ui,
                                "FABRICANTE / PRODUCTO",
                                Some(maker_text.as_str()).filter(|s| !s.is_empty()),
                                false,
                            );
                            if let Some((signature, ok)) = signature_line(data) {
                                ui.add_space(9.0);
                                ui.label(RichText::new("FIRMA").size(9.0).strong().color(FAINT));
                                ui.add_space(2.0);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(signature)
                                            .size(11.5)
                                            .color(if ok { OK } else { WARN }),
                                    )
                                    .wrap(),
                                );
                            }
                            let path = jstr(data, &["path", "exe", "image_path"])
                                .filter(|s| !s.is_empty())
                                .or(if snap_exe.is_empty() { None } else { Some(snap_exe) });
                            detail_field(ui, "RUTA", path, true);
                            let parent = parent_text(data);
                            detail_field(ui, "PROCESO PADRE", parent.as_deref(), false);
                            let started = start_text(data);
                            detail_field(ui, "INICIO", started.as_deref(), false);
                            detail_field(
                                ui,
                                "VENTANA",
                                jstr(data, &["window_title", "title"]),
                                false,
                            );
                            detail_field(
                                ui,
                                "LÍNEA DE COMANDOS",
                                jstr(data, &["command_line", "cmdline", "cmd"]),
                                true,
                            );
                        } else {
                            let error = detail.error.clone().unwrap_or_default();
                            let message = if error.contains("404") {
                                "El motor todavía no expone la ficha de procesos (/api/process). Mientras tanto usá la ruta y el padre del snapshot."
                            } else if error.is_empty() {
                                "No pude consultar la ficha de este proceso."
                            } else {
                                "El motor no respondió la consulta de detalle."
                            };
                            ui.add_space(8.0);
                            detail_note(ui, width, "Sin ficha del fabricante", message, true);
                            detail_field(ui, "NOMBRE", Some(snap_name), false);
                            detail_field(
                                ui,
                                "RUTA (SNAPSHOT)",
                                if snap_exe.is_empty() { None } else { Some(snap_exe) },
                                true,
                            );
                            detail_field(ui, "PRIORIDAD", Some(prio_label), false);
                            if !error.is_empty() {
                                ui.add_space(9.0);
                                ui.label(
                                    RichText::new(format!("Detalle técnico: {error}"))
                                        .size(9.0)
                                        .monospace()
                                        .color(FAINT),
                                );
                            }
                            ui.add_space(12.0);
                            if btn(ui, "Cargar detalle", 124.0, 28.0, Btn::Secondary, true)
                                .on_hover_text("Vuelve a pedir la ficha al motor")
                                .clicked()
                            {
                                retry = true;
                            }
                        }
                    });

                ui.add_space(12.0);
                let (line, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    Sense::hover(),
                );
                ui.painter().rect_filled(line, 0.0, a(BORDER, 180));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if btn(ui, "Subir", 60.0, 28.0, Btn::Secondary, !busy && !protected)
                        .on_hover_text("Subir prioridad a Alta")
                        .clicked()
                    {
                        act = Some(Action::Priority(detail.pid));
                    }
                    if btn(ui, "Liberar", 70.0, 28.0, Btn::Secondary, !busy)
                        .on_hover_text("Liberar el working set sin cerrar la app")
                        .clicked()
                    {
                        act = Some(Action::Trim(detail.pid));
                    }
                    if btn(ui, "Restaurar", 78.0, 28.0, Btn::Ghost, !busy)
                        .on_hover_text("Volver a la prioridad original")
                        .clicked()
                    {
                        act = Some(Action::Restore(detail.pid));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if btn(ui, "Fin", 54.0, 28.0, Btn::Danger, !busy && !protected)
                            .on_hover_text("Terminar el proceso (pide confirmación)")
                            .clicked()
                        {
                            act = Some(Action::Kill(detail.pid, snap_name.to_string()));
                        }
                    });
                });
            });
        if modal.should_close() {
            close = true;
        }
        if retry {
            self.open_detail(detail.pid);
        }
        match act {
            Some(Action::Priority(pid)) => self.action(
                "/api/priority",
                json!({ "pid": pid, "cls": "high" }),
                "Subiendo prioridad",
                "Prioridad alta aplicada",
            ),
            Some(Action::Trim(pid)) => self.action(
                "/api/trim",
                json!({ "pid": pid }),
                "Liberando RAM",
                "Working set liberado",
            ),
            Some(Action::Restore(pid)) => self.action(
                "/api/restore",
                json!({ "pid": pid }),
                "Restaurando",
                "Prioridad original restaurada",
            ),
            Some(Action::Kill(pid, name)) => {
                self.close_detail();
                self.kill_confirm = Some((pid, name));
            }
            None => {}
        }
        if close {
            self.close_detail();
        }
    }
}

impl eframe::App for DomadorUi {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(500));
        // Los toasts no se quedan pegados: 6 segundos y se van.
        {
            let mut guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(toast) = &guard.toast {
                if toast.at.elapsed() > Duration::from_secs(6) {
                    guard.toast = None;
                }
            }
        }
        self.shortcuts(ctx);
        self.top(ctx);
        self.profile_bar(ctx);
        self.side(ctx);
        self.status_bar(ctx);
        self.primary(ctx);
        self.detail_modal(ctx);
        self.kill_dialog(ctx);
    }
}

// ------------------------------------------------------------- acciones ---

enum Action {
    Priority(u32),
    Trim(u32),
    Restore(u32),
    Kill(u32, String),
}

fn sort_rows(rows: &mut [&Value], sort: SortState) {
    rows.sort_by(|a, b| {
        let order = match sort.key {
            SortBy::Cpu => fnum(a.get("cpu"))
                .partial_cmp(&fnum(b.get("cpu")))
                .unwrap_or(std::cmp::Ordering::Equal),
            SortBy::Ram => num(a.get("rss")).cmp(&num(b.get("rss"))),
            SortBy::Name => text_of(a.get("name"))
                .to_lowercase()
                .cmp(&text_of(b.get("name")).to_lowercase()),
        };
        if sort.desc {
            order.reverse()
        } else {
            order
        }
    });
}

// ---------------------------------------------------------------- salud ---

fn memory_card(ui: &mut egui::Ui, width: f32, state: &Value) {
    let mem = state.pointer("/system/mem");
    let ram_used = num(state.pointer("/system/ram_used"));
    let ram_total = num(state.pointer("/system/ram_total"));
    let available = num(mem.and_then(|m| m.get("available")));
    let used = num(mem.and_then(|m| m.get("commit_used")));
    let limit = num(mem.and_then(|m| m.get("commit_limit"))).max(1);
    let commit_pct = (used as f64 / limit as f64 * 100.0).min(100.0);
    let ram_pct = if ram_total > 0 {
        ram_used as f64 / ram_total as f64 * 100.0
    } else {
        0.0
    };
    let ram_color = if ram_pct > 85.0 {
        COPPER_HI
    } else if ram_pct > 70.0 {
        BRASS
    } else {
        OK
    };
    card(ui, width, |ui| {
        section(ui, "Memoria", Some("sistema"));
        kv(ui, "Disponible", &fmt_bytes(available), OK);
        ui.add_space(11.0);
        kv(
            ui,
            "En uso",
            &format!(
                "{:.1}/{:.1} GB",
                ram_used as f64 / GB,
                ram_total as f64 / GB
            ),
            ram_color,
        );
        ui.add_space(4.0);
        meter(ui, (ram_pct / 100.0) as f32, ram_color);
        ui.add_space(12.0);
        kv(
            ui,
            "Commit",
            &format!("{:.1}/{:.1} GB", used as f64 / GB, limit as f64 / GB),
            if commit_pct > 85.0 { COPPER_HI } else { TEXT },
        );
        ui.add_space(4.0);
        meter(
            ui,
            (commit_pct / 100.0) as f32,
            if commit_pct > 85.0 {
                COPPER_HI
            } else {
                BRASS_DIM
            },
        );
        ui.add_space(12.0);
        kv(
            ui,
            "Procesos",
            &num(mem.and_then(|m| m.get("processes"))).to_string(),
            TEXT,
        );
        kv(
            ui,
            "Handles",
            &num(mem.and_then(|m| m.get("handles"))).to_string(),
            MUTED,
        );
        ui.add_space(10.0);
        ui.label(RichText::new("Kernel y caché").size(10.0).color(FAINT));
        ui.add_space(6.0);
        kv(
            ui,
            "Pool paginado",
            &fmt_bytes(num(mem.and_then(|m| m.get("pool_paged")))),
            MUTED,
        );
        kv(
            ui,
            "No paginado",
            &fmt_bytes(num(mem.and_then(|m| m.get("pool_nonpaged")))),
            MUTED,
        );
        kv(
            ui,
            "Caché",
            &fmt_bytes(num(mem.and_then(|m| m.get("cache")))),
            BRASS,
        );
    });
}

#[allow(dead_code)]
fn kernel_card(ui: &mut egui::Ui, width: f32, state: &Value) {
    let mem = state.pointer("/system/mem");
    card(ui, width, |ui| {
        section(ui, "Kernel y caché", None);
        kv(
            ui,
            "Pool paginado",
            &fmt_bytes(num(mem.and_then(|m| m.get("pool_paged")))),
            TEXT,
        );
        kv(
            ui,
            "No paginado",
            &fmt_bytes(num(mem.and_then(|m| m.get("pool_nonpaged")))),
            TEXT,
        );
        kv(
            ui,
            "Caché",
            &fmt_bytes(num(mem.and_then(|m| m.get("cache")))),
            BRASS,
        );
        ui.add_space(12.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "La caché no es RAM perdida: Windows la reutiliza cuando una app la pide.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        if let Some(fg_name) = state
            .pointer("/system/foreground_name")
            .and_then(|v| v.as_str())
        {
            if !fg_name.is_empty() {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    dot(ui, OK, 3.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("En foco: {fg_name}"))
                                .size(10.5)
                                .color(MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(fg_name);
                });
            }
        }
    });
}

fn gpu_card(ui: &mut egui::Ui, width: f32, state: &Value) {
    let Some(gpu) = state.get("gpu") else {
        return;
    };
    let available = gpu
        .get("available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    card(ui, width, |ui| {
        if !available {
            section(ui, "GPU", None);
            empty_state(
                ui,
                "Sin telemetría de GPU",
                "El motor no detectó una GPU con contadores expuestos. La memoria y la CPU siguen funcionando.",
            );
            return;
        }
        let name = gpu.get("name").and_then(|v| v.as_str()).unwrap_or("GPU");
        let driver = gpu.get("driver").and_then(|v| v.as_str()).unwrap_or("—");
        section(ui, "GPU", Some("telemetría del motor"));
        ui.add(egui::Label::new(RichText::new(name).size(12.0).strong().color(TEXT)).truncate())
            .on_hover_text(name);
        ui.add_space(10.0);

        let util = fnum(gpu.get("utilization"));
        let used = num(gpu.get("memory_used"));
        let total = num(gpu.get("memory_total")).max(1);
        let vram_pct = (used as f64 / total as f64 * 100.0).min(100.0);
        let temp = fnum(gpu.get("temperature"));
        let power = fnum(gpu.get("power"));
        let pstate = gpu.get("pstate").and_then(|v| v.as_str()).unwrap_or("—");

        kv(ui, "Uso", &format!("{util:.0}%"), BRASS);
        ui.add_space(4.0);
        meter(ui, (util / 100.0) as f32, BRASS);
        ui.add_space(11.0);
        kv(
            ui,
            "VRAM",
            &format!("{:.1}/{:.1} GB", used as f64 / GB, total as f64 / GB),
            if vram_pct > 90.0 { COPPER_HI } else { TEXT },
        );
        ui.add_space(4.0);
        meter(
            ui,
            (vram_pct / 100.0) as f32,
            if vram_pct > 90.0 {
                COPPER_HI
            } else {
                BRASS_DIM
            },
        );
        ui.add_space(11.0);
        kv(
            ui,
            "Temperatura",
            &format!("{temp:.0} °C"),
            if temp > 80.0 { COPPER_HI } else { OK },
        );
        if power > 0.0 {
            kv(ui, "Consumo", &format!("{power:.0} W"), MUTED);
        }
        kv(ui, "Estado", pstate, MUTED);
        kv(ui, "Driver", driver, FAINT);
    });
}

fn private_card(ui: &mut egui::Ui, width: f32, state: &Value) {
    let mut rows: Vec<&Value> = state
        .get("processes")
        .and_then(|v| v.as_array())
        .map(|list| list.iter().collect())
        .unwrap_or_default();
    rows.sort_by(|a, b| num(b.get("private")).cmp(&num(a.get("private"))));
    card(ui, width, |ui| {
        section(ui, "Top por memoria privada", Some("10 primeros"));
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Es la memoria que el working set no muestra: ahí suelen esconderse los WebView.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        let inner = ui.available_width();
        let col = if inner >= 520.0 { 92.0 } else { 76.0 };
        let gap = 8.0;
        let name_w = (inner - 2.0 * col - 2.0 * gap - 8.0).max(30.0);
        let (head, _) = ui.allocate_exact_size(Vec2::new(inner, 18.0), Sense::hover());
        let painter = ui.painter().with_clip_rect(head);
        painter.text(
            Pos2::new(head.left() + 4.0, head.center().y),
            Align2::LEFT_CENTER,
            "PROCESO",
            FontId::proportional(9.0),
            FAINT,
        );
        painter.text(
            Pos2::new(head.right() - 6.0 - col - gap, head.center().y),
            Align2::RIGHT_CENTER,
            "PRIVADA",
            FontId::proportional(9.0),
            FAINT,
        );
        painter.text(
            Pos2::new(head.right() - 6.0, head.center().y),
            Align2::RIGHT_CENTER,
            "WORKING SET",
            FontId::proportional(9.0),
            FAINT,
        );
        for (index, row) in rows.iter().take(10).enumerate() {
            let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let private = num(row.get("private"));
            let rss = num(row.get("rss"));
            let (rect, response) = ui.allocate_exact_size(Vec2::new(inner, 24.0), Sense::hover());
            if index % 2 == 1 {
                ui.painter().rect_filled(rect, 4.0, a(CARD_HOVER, 120));
            }
            let painter = ui.painter().with_clip_rect(rect);
            let label = ellipsize(ui, name, FontId::proportional(11.0), name_w);
            painter.text(
                Pos2::new(rect.left() + 4.0, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                TEXT,
            );
            painter.text(
                Pos2::new(rect.right() - 6.0 - col - gap, rect.center().y),
                Align2::RIGHT_CENTER,
                fmt_bytes(private),
                FontId::monospace(10.5),
                if private as f64 / GB > 1.0 {
                    COPPER_HI
                } else {
                    TEXT
                },
            );
            painter.text(
                Pos2::new(rect.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                fmt_bytes(rss),
                FontId::monospace(10.0),
                MUTED,
            );
            response.on_hover_text(name);
        }
    });
}

#[allow(dead_code)]
fn motor_card(ui: &mut egui::Ui, width: f32, state: Option<&Value>) {
    card(ui, width, |ui| {
        section(ui, "Motor", Some("127.0.0.1:8787"));
        let version = state
            .and_then(|s| s.pointer("/app/version"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let uptime = fnum(state.and_then(|s| s.pointer("/app/uptime")));
        let ncpu = num(state.and_then(|s| s.pointer("/app/ncpu")));
        let plan = state
            .and_then(|s| s.pointer("/profile/plan"))
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        let tick = fnum(state.and_then(|s| s.pointer("/app/tick_ms/p95")));
        let path = state
            .and_then(|s| s.pointer("/app/config_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        kv(ui, "Versión", version, TEXT);
        kv(ui, "Arranque", &fmt_uptime(uptime), MUTED);
        kv(ui, "Núcleos", &ncpu.to_string(), MUTED);
        kv(ui, "Plan de energía", plan, MUTED);
        if tick > 0.0 {
            kv(ui, "Tick p95", &format!("{tick:.1} ms"), MUTED);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Configuración").size(10.0).color(FAINT));
        let shown = ellipsize(
            ui,
            path,
            FontId::monospace(9.5),
            (ui.available_width() - 8.0).max(40.0),
        );
        ui.add(
            egui::Label::new(RichText::new(shown).monospace().size(9.5).color(MUTED)).truncate(),
        )
        .on_hover_text(path);
        ui.add_space(6.0);
        if btn(ui, "Copiar ruta", 96.0, 24.0, Btn::Ghost, true).clicked() {
            ui.ctx().copy_text(path.to_string());
        }
    });
}

fn shortcuts_card(ui: &mut egui::Ui, width: f32) {
    card(ui, width, |ui| {
        section(ui, "Atajos", None);
        for (label, keys) in [
            ("Buscar proceso", ["Ctrl", "F"]),
            ("Actualizar estado", ["Ctrl", "R"]),
            ("Procesos / Salud / Ajustes", ["Ctrl", "1·2·3"]),
            ("Cerrar diálogo", ["Esc", ""]),
        ] {
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 22.0),
                egui::Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new(label).size(11.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        for key in keys.iter().rev().filter(|k| !k.is_empty()) {
                            kbd(ui, key);
                        }
                    });
                },
            );
        }
    });
}

fn side_kv(ui: &mut egui::Ui, key: &str, value: &str) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 20.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [(width - 46.0).max(20.0), 20.0],
                egui::Label::new(RichText::new(key).size(10.5).color(MUTED)).truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(value)
                        .size(11.0)
                        .monospace()
                        .strong()
                        .color(TEXT),
                );
            });
        },
    );
}

fn detail_field(ui: &mut egui::Ui, label: &str, value: Option<&str>, mono: bool) {
    let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
        return;
    };
    ui.add_space(9.0);
    ui.label(RichText::new(label).size(9.0).strong().color(FAINT));
    ui.add_space(2.0);
    let text = if mono {
        RichText::new(value).monospace().size(9.5)
    } else {
        RichText::new(value).size(11.5)
    };
    ui.add(egui::Label::new(text.color(TEXT)).wrap());
}

fn detail_note(ui: &mut egui::Ui, width: f32, title: &str, body: &str, warn: bool) {
    let color = if warn { WARN } else { FAINT };
    let frame = egui::Frame::new()
        .fill(a(color, 14))
        .stroke(mk_stroke(1.0, a(color, 90)))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width((width - 24.0).max(60.0));
            ui.set_max_width((width - 24.0).max(60.0));
            ui.label(RichText::new(title).size(11.0).strong().color(color));
            ui.add_space(3.0);
            ui.add(egui::Label::new(RichText::new(body).size(10.5).color(MUTED)).wrap());
        });
    rivets(ui, frame.response.rect);
}

fn empty_state(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.add_space(10.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(13.0).strong().color(TEXT));
        ui.add_space(3.0);
        ui.add(
            egui::Label::new(RichText::new(body).size(10.5).color(MUTED))
                .wrap()
                .halign(Align::Center),
        );
    });
    ui.add_space(10.0);
}

// ------------------------------------------------------------------ legion ---

#[derive(Clone, Copy)]
struct Limit {
    value: i64,
    min: i64,
    max: i64,
    step: i64,
}

impl Limit {
    fn parse(value: Option<&Value>, fallback: i64) -> Self {
        let raw_min = value
            .and_then(|v| v.get("min"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let raw_max = value
            .and_then(|v| v.get("max"))
            .and_then(|v| v.as_i64())
            .unwrap_or(100);
        let (min, max) = (raw_min.min(raw_max), raw_min.max(raw_max));
        let step = value
            .and_then(|v| v.get("step"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .max(1);
        let value = value
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_i64())
            .unwrap_or(fallback);
        Self {
            value: value.clamp(min, max),
            min,
            max,
            step,
        }
    }
}

struct FanView {
    id: String,
    rpm: u32,
    max: u32,
}

struct Competitor {
    name: String,
    pid: u32,
    closed: bool,
}

/// LED del botón de encendido. Es sólo lectura: lo mueve el modo térmico por
/// el mismo registro del EC que usa Fn+Q, así que no tiene selector propio.
#[derive(Clone)]
struct LedInfo {
    color: Color32,
    name: &'static str,
    /// true = el backend no manda el dato y lo dedujimos del modo.
    derived: bool,
}

fn led_from_token(raw: &str) -> Option<LedInfo> {
    let token = raw.trim().to_ascii_lowercase();
    let (color, name) = match token.as_str() {
        "blue" | "azul" | "quiet" | "silent" => (Color32::from_rgb(98, 168, 255), "Azul"),
        "white" | "blanco" | "balance" | "balanced" | "standard" => {
            (Color32::from_rgb(238, 234, 222), "Blanco")
        }
        "red" | "rojo" | "performance" | "turbo" => (Color32::from_rgb(226, 88, 74), "Rojo"),
        "amber" | "ambar" | "ámbar" | "naranja" | "orange" => {
            (Color32::from_rgb(232, 176, 92), "Ámbar")
        }
        "off" | "apagado" | "disabled" => (Color32::from_rgb(110, 100, 90), "Apagado"),
        _ => return None,
    };
    Some(LedInfo {
        color,
        name,
        derived: false,
    })
}

/// Quiet = azul, Balanced = blanco, Performance = rojo, Custom = lo que diga
/// el firmware. Se usa cuando el backend no publica el campo del LED.
fn led_for_mode(mode: &str) -> LedInfo {
    let mode = mode.trim().to_ascii_lowercase();
    let (color, name) = match mode.as_str() {
        "quiet" | "silent" => (Color32::from_rgb(98, 168, 255), "Azul"),
        "balance" | "balanced" | "standard" | "" => (Color32::from_rgb(238, 234, 222), "Blanco"),
        "performance" | "turbo" => (Color32::from_rgb(226, 88, 74), "Rojo"),
        _ => (BRASS, "Según firmware"),
    };
    LedInfo {
        color,
        name,
        derived: true,
    }
}

fn parse_led(data: &Value, thermal_id: &str) -> LedInfo {
    // El backend puede publicar el LED como objeto (`power_led`) o como texto
    // suelto; si no publica nada, el color se deduce del modo térmico.
    if let Some(led) = data.get("power_led") {
        if let Some(raw) = led.as_str() {
            if let Some(info) = led_from_token(raw) {
                return info;
            }
        }
        if let Some(object) = led.as_object() {
            for key in ["color", "name", "label", "state", "mode"] {
                if let Some(raw) = object.get(key).and_then(|v| v.as_str()) {
                    if let Some(info) = led_from_token(raw) {
                        return info;
                    }
                }
            }
        }
    }
    let raw = jstr(data, &["led", "led_color", "led_mode"])
        .or_else(|| data.pointer("/led/color").and_then(|v| v.as_str()))
        .or_else(|| data.pointer("/thermal/led").and_then(|v| v.as_str()));
    match raw.and_then(led_from_token) {
        Some(info) => info,
        None => led_for_mode(thermal_id),
    }
}

/// Vista tipada de `/api/legion`. Todo lo que falte cae a un valor neutro:
/// la pestaña tiene que poder dibujarse con un backend a medio hacer.
struct LegionView {
    available: bool,
    model: String,
    bios: String,
    thermal_id: String,
    thermal_label: String,
    modes: Vec<(String, String)>,
    fans: Vec<FanView>,
    full_speed: bool,
    temp_cpu: Option<f64>,
    temp_gpu: Option<f64>,
    long: Limit,
    short: Limit,
    table: Vec<i64>,
    stock: Vec<i64>,
    competitors: Vec<Competitor>,
    chief: bool,
    note: Option<String>,
    warnings: Vec<String>,
    led: LedInfo,
}

fn int_list(value: Option<&Value>) -> Vec<i64> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default()
}

fn pretty(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl LegionView {
    fn from(data: &Value) -> Self {
        let mut modes: Vec<(String, String)> = data
            .get("modes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|mode| {
                        let id = jstr(mode, &["id", "mode"])?.to_string();
                        let label = jstr(mode, &["label"])
                            .map(str::to_string)
                            .unwrap_or_else(|| pretty(&id));
                        Some((id, label))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if modes.is_empty() {
            modes = vec![
                ("quiet".to_string(), "Silencioso".to_string()),
                ("balance".to_string(), "Equilibrado".to_string()),
                ("performance".to_string(), "Rendimiento".to_string()),
                ("custom".to_string(), "Personalizado".to_string()),
            ];
        }
        let thermal = data.get("thermal");
        let thermal_id = thermal
            .and_then(|t| jstr(t, &["id", "mode"]))
            .unwrap_or("")
            .to_string();
        let thermal_label = thermal
            .and_then(|t| jstr(t, &["label"]))
            .map(str::to_string)
            .or_else(|| {
                modes
                    .iter()
                    .find(|(id, _)| *id == thermal_id)
                    .map(|(_, label)| label.clone())
            })
            .unwrap_or_else(|| "—".to_string());
        let fans = data
            .get("fans")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|fan| {
                        let id = jstr(fan, &["id", "name"])?.to_string();
                        Some(FanView {
                            id,
                            rpm: num(fan.get("rpm")) as u32,
                            max: num(fan.get("max")) as u32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let competitors = data
            .get("competitors")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let name = jstr(c, &["name", "process"])?.to_string();
                        Some(Competitor {
                            name,
                            pid: num(c.get("pid")) as u32,
                            closed: jbool(c, &["closed", "killed"]).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            available: jbool(data, &["available"]).unwrap_or(true),
            model: jstr(data, &["model"]).unwrap_or("—").to_string(),
            bios: jstr(data, &["bios"]).unwrap_or("—").to_string(),
            led: parse_led(data, &thermal_id),
            thermal_id,
            thermal_label,
            modes,
            fans,
            full_speed: jbool(data, &["full_speed"]).unwrap_or(false),
            temp_cpu: data.pointer("/temps/cpu").and_then(|v| v.as_f64()),
            temp_gpu: data.pointer("/temps/gpu").and_then(|v| v.as_f64()),
            long: Limit::parse(data.pointer("/cpu/long"), 0),
            short: Limit::parse(data.pointer("/cpu/short"), 0),
            table: int_list(data.get("fan_table")),
            stock: int_list(data.get("fan_table_stock")),
            competitors,
            chief: jbool(data, &["chief"]).unwrap_or(false),
            note: jstr(data, &["error"]).map(str::to_string),
            warnings: data
                .get("warnings")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// Aviso de color con remaches: mismo lenguaje que las tarjetas.
fn notice(ui: &mut egui::Ui, width: f32, title: &str, body: &str, color: Color32) {
    let frame = egui::Frame::new()
        .fill(a(color, 16))
        .stroke(mk_stroke(1.0, a(color, 110)))
        .corner_radius(9.0)
        .inner_margin(Margin::symmetric(13, 10))
        .show(ui, |ui| {
            let inner = (width - 26.0).max(80.0);
            ui.set_width(inner);
            ui.set_max_width(inner);
            ui.horizontal(|ui| {
                dot(ui, color, 4.0);
                ui.label(RichText::new(title).size(12.0).strong().color(color));
            });
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(body).size(10.5).color(MUTED)).wrap());
        });
    rivets(ui, frame.response.rect);
}

fn admin_notice(ui: &mut egui::Ui, width: f32, busy: bool, intents: &mut LegionIntent) {
    let frame = egui::Frame::new()
        .fill(a(WARN, 16))
        .stroke(mk_stroke(1.0, a(WARN, 110)))
        .corner_radius(9.0)
        .inner_margin(Margin::symmetric(13, 10))
        .show(ui, |ui| {
            let inner = (width - 26.0).max(80.0);
            ui.set_width(inner);
            ui.set_max_width(inner);
            ui.horizontal(|ui| {
                dot(ui, WARN, 4.0);
                ui.label(RichText::new("Falta administrador").size(12.0).strong().color(WARN));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if btn(ui, "Elevar", 84.0, 24.0, Btn::Secondary, !busy)
                        .on_hover_text("Reinicia la UI con UAC; el control del EC exige elevación")
                        .clicked()
                    {
                        intents.elevate = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Leer y escribir el EC pide sesión elevada. Los controles de esta pestaña quedan apagados hasta que eleves la UI.",
                    )
                    .size(10.5)
                    .color(MUTED),
                )
                .wrap(),
            );
        });
    rivets(ui, frame.response.rect);
}

/// Botones de opción en filas que se reacomodan solas: nunca desbordan el
/// ancho disponible ni dependen de `ui.columns`.
fn option_grid(
    ui: &mut egui::Ui,
    items: &[(&str, bool)],
    min_cell: f32,
    height: f32,
    enabled: bool,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let avail = ui.available_width().max(80.0);
    let gap = 6.0;
    let mut per_row = items.len();
    while per_row > 1 {
        let cell = (avail - gap * (per_row as f32 - 1.0)) / per_row as f32;
        if cell >= min_cell {
            break;
        }
        per_row -= 1;
    }
    let cell = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32)
        .floor()
        .max(52.0);
    let mut clicked = None;
    let rows = items.len().div_ceil(per_row);
    for row in 0..rows {
        if row > 0 {
            ui.add_space(gap);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (idx, (label, active)) in items.iter().enumerate().skip(row * per_row).take(per_row)
            {
                let kind = if *active {
                    Btn::Primary
                } else {
                    Btn::Secondary
                };
                if btn(ui, label, cell, height, kind, enabled).clicked() && enabled {
                    clicked = Some(idx);
                }
            }
        });
    }
    clicked
}

/// Slider steampunk: pista hundida, muescas y perilla de latón.
fn brass_slider(
    ui: &mut egui::Ui,
    value: &mut i64,
    min: i64,
    max: i64,
    step: i64,
    width: f32,
    enabled: bool,
) -> Response {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width.max(60.0), 22.0), sense);
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 6.0, rect.center().y - 3.0),
        Pos2::new(rect.right() - 6.0, rect.center().y + 3.0),
    );
    let span = (max - min).max(1) as f32;
    let frac = ((*value - min) as f32 / span).clamp(0.0, 1.0);
    let painter = ui.painter();
    painter.rect_filled(track, 3.0, SUNKEN);
    painter.rect_stroke(track, 3.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    if track.width() > 24.0 {
        for notch in 0..=4 {
            let x = egui::lerp(track.x_range(), notch as f32 / 4.0);
            painter.vline(
                x,
                (track.top() + 1.0)..=(track.bottom() - 1.0),
                mk_stroke(1.0, a(BORDER, 120)),
            );
        }
    }
    let filled = Rect::from_min_max(
        track.min,
        Pos2::new(track.left() + track.width() * frac, track.bottom()),
    );
    if frac > 0.0 {
        painter.rect_filled(filled, 3.0, a(BRASS, if enabled { 120 } else { 50 }));
    }
    let knob = Pos2::new(track.left() + track.width() * frac, rect.center().y);
    let hot = enabled && (response.hovered() || response.dragged());
    painter.circle_filled(knob + Vec2::new(0.0, 1.0), 8.0, a(Color32::BLACK, 70));
    painter.circle_filled(
        knob,
        7.0,
        if !enabled {
            FAINT
        } else if hot {
            BRASS_HI
        } else {
            BRASS
        },
    );
    painter.circle_filled(knob, 2.2, INK);
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let mut response = response;
    if enabled && (response.dragged() || response.clicked()) {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            let raw = min as f32 + t * span;
            let stepped = ((raw - min as f32) / step as f32).round() * step as f32 + min as f32;
            let next = (stepped.round() as i64).clamp(min, max);
            if next != *value {
                *value = next;
                response.mark_changed();
            }
        }
    }
    response
}

/// Editor de la curva: diez columnas arrastrables, con la curva de fábrica
/// dibujada como fantasma para comparar.
fn curve_editor(ui: &mut egui::Ui, values: &mut [i64; 10], stock: &[i64], enabled: bool) -> bool {
    let width = ui.available_width().max(120.0);
    let height = 116.0;
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 8.0, SUNKEN);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 8.0, rect.bottom() - 20.0),
    );
    for frac in [0.25_f32, 0.5, 0.75] {
        let y = egui::lerp(plot.y_range(), 1.0 - frac);
        painter.hline(plot.x_range(), y, mk_stroke(1.0, a(BORDER, 70)));
    }
    let slot = plot.width() / 10.0;
    let bar_w = (slot * 0.52).clamp(3.0, 26.0);
    let hovered = response
        .hover_pos()
        .map(|pos| (((pos.x - plot.left()) / slot.max(0.01)).floor() as i32).clamp(0, 9) as usize);
    // Fantasma de la tabla de fábrica.
    if stock.len() >= 2 {
        let points: Vec<Pos2> = stock
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, v)| {
                let frac = (*v).clamp(1, 10) as f32 / 10.0;
                Pos2::new(
                    plot.left() + slot * (i as f32 + 0.5),
                    plot.bottom() - plot.height() * frac,
                )
            })
            .collect();
        painter.add(egui::Shape::line(points, mk_stroke(1.0, a(MUTED, 120))));
    }
    for i in 0..10 {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        let frac = values[i].clamp(1, 10) as f32 / 10.0;
        let h = (plot.height() * frac).max(2.0);
        let bar = Rect::from_min_max(
            Pos2::new(cx - bar_w * 0.5, plot.bottom() - h),
            Pos2::new(cx + bar_w * 0.5, plot.bottom()),
        );
        let hot = hovered == Some(i) && enabled;
        let fill = if !enabled {
            a(FAINT, 90)
        } else if hot {
            BRASS_HI
        } else {
            a(BRASS, 190)
        };
        painter.rect_filled(bar, 2.0, fill);
        painter.text(
            Pos2::new(cx, plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            format!("{}", i + 1),
            FontId::monospace(8.5),
            FAINT,
        );
        if hot {
            painter.text(
                Pos2::new(cx, rect.top() + 2.0),
                Align2::CENTER_TOP,
                format!("n{}", values[i]),
                FontId::monospace(9.5),
                BRASS_HI,
            );
        }
    }
    if !enabled {
        return false;
    }
    let mut changed = false;
    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let idx =
                (((pos.x - plot.left()) / slot.max(0.01)).floor() as i32).clamp(0, 9) as usize;
            let t = ((plot.bottom() - pos.y) / plot.height().max(1.0)).clamp(0.0, 1.0);
            // El EC pide niveles enteros de 1 a 10, no porcentajes.
            let next = (t * 9.0).round() as i64 + 1;
            let next = next.clamp(1, 10);
            if values[idx] != next {
                values[idx] = next;
                changed = true;
            }
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    changed
}

fn confirm_strip(
    ui: &mut egui::Ui,
    title: &str,
    body: &str,
    color: Color32,
    ok_label: &str,
    enabled: bool,
) -> (bool, bool) {
    let mut confirmed = false;
    let mut cancelled = false;
    let width = ui.available_width();
    let frame = egui::Frame::new()
        .fill(a(color, 12))
        .stroke(mk_stroke(1.0, a(color, 80)))
        .corner_radius(7.0)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            let inner = (width - 20.0).max(80.0);
            ui.set_width(inner);
            ui.set_max_width(inner);
            ui.label(RichText::new(title).size(11.0).strong().color(color));
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(body).size(10.0).color(MUTED)).wrap());
            ui.add_space(7.0);
            ui.horizontal(|ui| {
                if btn(ui, ok_label, 96.0, 26.0, Btn::Primary, enabled).clicked() && enabled {
                    confirmed = true;
                }
                if btn(ui, "Cancelar", 84.0, 26.0, Btn::Ghost, true).clicked() {
                    cancelled = true;
                }
            });
        });
    rivets(ui, frame.response.rect);
    (confirmed, cancelled)
}

#[allow(dead_code)]
fn temp_tile(ui: &mut egui::Ui, width: f32, label: &str, value: Option<f64>) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 8.0, SUNKEN);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.top() + 7.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(9.0),
        FAINT,
    );
    let (text, color) = match value {
        Some(t) => {
            let color = if t >= 90.0 {
                DANGER
            } else if t >= 78.0 {
                COPPER_HI
            } else if t >= 65.0 {
                BRASS
            } else {
                OK
            };
            (format!("{t:.0}°C"), color)
        }
        None => ("—".to_string(), FAINT),
    };
    painter.text(
        Pos2::new(rect.left() + 10.0, rect.top() + 20.0),
        Align2::LEFT_TOP,
        text,
        FontId::monospace(18.0),
        color,
    );
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 12.0),
        Pos2::new(rect.right() - 10.0, rect.bottom() - 8.0),
    );
    painter.rect_filled(track, 2.0, a(BORDER, 90));
    let frac = (value.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32;
    if frac > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, Vec2::new(track.width() * frac, track.height())),
            2.0,
            color,
        );
    }
    response.on_hover_text("Temperatura que reporta el EC (no la del sensor de Windows).");
}

fn fan_row(ui: &mut egui::Ui, name: &str, rpm: u32, max: u32) {
    let frac = if max > 0 {
        rpm as f32 / max as f32
    } else {
        0.0
    };
    let color = if frac >= 0.85 {
        COPPER_HI
    } else if frac >= 0.6 {
        BRASS
    } else {
        OK
    };
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 26.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [(width - 132.0).max(40.0), 26.0],
                egui::Label::new(RichText::new(name).size(11.0).color(MUTED)).truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(if max > 0 {
                        format!("/ {max} rpm")
                    } else {
                        "/ — rpm".to_string()
                    })
                    .size(9.5)
                    .monospace()
                    .color(FAINT),
                );
                ui.label(
                    RichText::new(format!("{rpm}"))
                        .size(15.0)
                        .monospace()
                        .strong()
                        .color(color),
                );
            });
        },
    );
    meter(ui, frac, color);
}

fn hardware_guide(ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Guía rápida · qué es el EC y qué temperatura es sana")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "EC = Embedded Controller. Es el chip chico de la motherboard que maneja ventiladores, modos térmicos, batería y el LED del botón. No es el CPU ni Windows: es firmware. Domador le habla por WMI, la interfaz que expone el BIOS. Si el BIOS no publica un método (por ejemplo, el segundo fan o una curva), ninguna app de usuario puede controlarlo sin un driver de kernel.",
                    )
                    .size(10.5)
                    .color(MUTED),
                )
                .wrap(),
            );
            ui.add_space(9.0);
            ui.label(RichText::new("Zonas para esta Legion (5800H + RTX 3070 Laptop)").size(11.0).strong().color(TEXT));
            ui.add_space(5.0);
            for (zone, color, text) in [
                ("Fresco", OK, "CPU/GPU por debajo de 70 °C: ideal en reposo o carga liviana."),
                ("Normal", BRASS, "CPU 70–85 °C y GPU 65–80 °C en juego: sano y esperable."),
                ("Caliente", WARN, "85–90 °C: picos cortos aceptables; sostenido ya castiga al silicio."),
                ("Límite", COPPER_HI, "CPU >90 °C o GPU >85 °C: throttling y menos FPS. Revisá polvo, pasta y entrada de aire."),
                ("Crítico", DANGER, "CPU >95 °C o GPU >90 °C: soltá la carga y revisá refrigeración."),
            ] {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, color, 3.0);
                    ui.label(RichText::new(zone).size(10.5).strong().color(color));
                    ui.label(RichText::new(text).size(10.0).color(MUTED));
                });
                ui.add_space(2.0);
            }
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "La mejor temperatura no es la más baja: es la que se mantiene estable sin throttling. En esta máquina, 75–85 °C de CPU bajo carga sostenida es una buena zona; 90 °C+ durante minutos es señal de que la refrigeración está al límite. El riesgo real es el calor sostenido, no un pico de dos segundos.",
                    )
                    .size(10.0)
                    .color(FAINT),
                )
                .wrap(),
            );
        });
}

fn legion_analysis_section(
    ui: &mut egui::Ui,
    _width: f32,
    adaptive: &Adaptive,
    tele: &Telemetry,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
) {
    egui::CollapsingHeader::new("Análisis · temperatura, fans y modo automático")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(6.0);
            let avail = ui.available_width();
            if avail >= 860.0 {
                let left = (avail * 0.36).floor().max(300.0);
                let right = (avail - left - 12.0).max(300.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.allocate_ui_with_layout(
                        Vec2::new(left, 0.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| legion_adaptive_card(ui, left, adaptive, lstate, intents),
                    );
                    ui.add_space(12.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(right, 0.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| legion_telemetry_card(ui, right, tele, lstate, intents),
                    );
                });
            } else {
                legion_adaptive_card(ui, avail, adaptive, lstate, intents);
                ui.add_space(12.0);
                legion_telemetry_card(ui, avail, tele, lstate, intents);
            }
        });
}

#[allow(dead_code)]
fn legion_equip_card(ui: &mut egui::Ui, width: f32, view: &LegionView, admin: bool) {
    card(ui, width, |ui| {
        section(ui, "Equipo", Some("EC"));
        eyebrow(ui, "Modelo");
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(&view.model)
                    .size(13.0)
                    .monospace()
                    .strong()
                    .color(TEXT),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        kv(ui, "BIOS", &view.bios, MUTED);
        ui.add_space(9.0);
        ui.horizontal(|ui| {
            if admin {
                badge(ui, "ADMIN", OK, a(OK, 26));
            } else {
                badge(ui, "SIN ADMIN", WARN, a(WARN, 24));
            }
            ui.add_space(5.0);
            if view.available {
                badge(ui, "WMI OK", OK, a(OK, 20));
            } else {
                badge(ui, "WMI N/D", FAINT, a(FAINT, 20));
            }
            if view.full_speed {
                ui.add_space(5.0);
                badge(ui, "FULL SPEED", COPPER_HI, a(COPPER_HI, 22));
            }
        });
        ui.add_space(11.0);
        let inner = ui.available_width();
        let gap = 8.0;
        let cell = ((inner - gap) / 2.0).floor().max(60.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            temp_tile(ui, cell, "CPU", view.temp_cpu);
            temp_tile(ui, cell, "GPU", view.temp_gpu);
        });
    });
}

fn led_row(ui: &mut egui::Ui, led: &LedInfo) {
    let width = ui.available_width().max(60.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 7.0, SUNKEN);
    painter.rect_stroke(rect, 7.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    let center = Pos2::new(rect.left() + 17.0, rect.center().y);
    painter.circle_filled(center, 8.0, a(led.color, 34));
    painter.circle_filled(center, 4.4, led.color);
    painter.circle_stroke(center, 8.0, mk_stroke(1.0, a(led.color, 140)));
    painter.text(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        "LED del botón",
        FontId::proportional(10.5),
        MUTED,
    );
    let value = if led.derived {
        format!("{} · deducido", led.name)
    } else {
        led.name.to_string()
    };
    painter.text(
        Pos2::new(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        FontId::monospace(10.5),
        led.color,
    );
    response.on_hover_text(
        "Quiet = azul · Balanced = blanco · Performance = rojo · Custom = lo que fije el firmware. Es informativo: lo mueve el modo térmico, no hay control aparte.",
    );
}

fn legion_thermal_card(
    ui: &mut egui::Ui,
    width: f32,
    view: &LegionView,
    intents: &mut LegionIntent,
    enabled: bool,
) {
    card(ui, width, |ui| {
        section(ui, "Modo térmico", Some(view.thermal_label.as_str()));
        let items: Vec<(&str, bool)> = view
            .modes
            .iter()
            .map(|(id, label)| (label.as_str(), id == &view.thermal_id))
            .collect();
        if let Some(idx) = option_grid(ui, &items, 86.0, 28.0, enabled) {
            if let Some((id, _)) = view.modes.get(idx) {
                intents.thermal = Some(id.clone());
            }
        }
        ui.add_space(9.0);
        kv(ui, "Activo", &view.thermal_label, BRASS);
        ui.add_space(7.0);
        led_row(ui, &view.led);
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                RichText::new(if view.led.derived {
                    "El EC todavía no publica el LED: el color se deduce del modo térmico. Domador escribe el mismo registro que Fn+Q, así que al cambiar el modo acá el LED del botón hace lo mismo."
                } else {
                    "El color lo reporta el EC. Domador escribe el mismo registro que Fn+Q: cambiar el modo acá mueve el LED del botón igual que el atajo del teclado."
                })
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        ui.add_space(3.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "El cambio es inmediato: el EC reescribe el registro de la máquina. Si Vantage, Legion Arena o Legion Toolkit están abiertos, van a pelear por el mismo valor.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
    });
}

fn legion_fans_card(
    ui: &mut egui::Ui,
    width: f32,
    view: &LegionView,
    intents: &mut LegionIntent,
    enabled: bool,
) {
    card(ui, width, |ui| {
        section(ui, "Ventiladores", Some("RPM"));
        let mut shown = 0usize;
        for fan in view.fans.iter().filter(|fan| fan.rpm > 0 || fan.max > 0) {
            fan_row(ui, &fan.id, fan.rpm, fan.max);
            ui.add_space(3.0);
            shown += 1;
        }
        if shown == 0 {
            ui.label(
                RichText::new("El BIOS no reporta ventiladores en este modelo.")
                    .size(10.5)
                    .color(FAINT),
            );
            return;
        }
        ui.add_space(7.0);
        let inner = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(inner, 26.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                let response = switch(ui, Id::new("legion-full-speed"), view.full_speed);
                if response.clicked() && enabled {
                    intents.full_speed = Some(!view.full_speed);
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Máxima refrigeración")
                        .size(11.5)
                        .strong()
                        .color(if view.full_speed { COPPER_HI } else { MUTED }),
                );
            },
        );
        ui.add_space(3.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Fuerza los dos ventiladores a tope mientras esté activo. Para jugar con curva propia, apagalo y aplicá la tabla de abajo: el EC acepta una cosa por vez.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
    });
}

fn legion_cpu_card(
    ui: &mut egui::Ui,
    width: f32,
    view: &LegionView,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
    enabled: bool,
) {
    card(ui, width, |ui| {
        section(ui, "Límites de CPU", Some("PPT"));
        let mut changed = false;
        changed |= limit_row(ui, "Largo (PL1)", &view.long, &mut lstate.cpu_long, enabled);
        ui.add_space(6.0);
        changed |= limit_row(
            ui,
            "Corto (PL2)",
            &view.short,
            &mut lstate.cpu_short,
            enabled,
        );
        if changed {
            lstate.cpu_edited = true;
        }
        ui.add_space(5.0);
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Rango {}–{} W · paso {} W. El EC acepta vatios enteros; por encima del máximo del BIOS recorta solo.",
                    view.long.min, view.long.max, view.long.step
                ))
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        ui.add_space(11.0);
        eyebrow(ui, "Presets");
        ui.add_space(5.0);
        let presets: [(&str, i64); 3] = [
            ("Suave 35 W", 35),
            ("Juego 45 W", 45),
            ("Rendimiento 65 W", 65),
        ];
        let items: Vec<(&str, bool)> = presets
            .iter()
            .map(|(label, watts)| (*label, lstate.cpu_long == *watts && !lstate.cpu_edited))
            .collect();
        if let Some(idx) = option_grid(ui, &items, 92.0, 26.0, enabled) {
            let target = presets[idx].1;
            let long = target.clamp(view.long.min, view.long.max);
            let short =
                ((target as f64 * 1.25).round() as i64).clamp(view.short.min, view.short.max);
            lstate.pending_cpu = Some((long, short, presets[idx].0.to_string()));
        }
        if lstate.cpu_edited {
            ui.add_space(7.0);
            if btn(
                ui,
                "Revisar ajuste manual",
                ui.available_width(),
                26.0,
                Btn::Secondary,
                enabled,
            )
            .clicked()
            {
                lstate.pending_cpu = Some((
                    lstate.cpu_long,
                    lstate.cpu_short,
                    "ajuste manual".to_string(),
                ));
            }
        }
        if let Some((long, short, why)) = lstate.pending_cpu.clone() {
            ui.add_space(10.0);
            let (ok, cancel) = confirm_strip(
                ui,
                "Confirmar límites",
                &format!(
                    "{long} W de largo · {short} W de corto ({why}). El EC lo aplica al instante y puede bajar el rendimiento sostenido."
                ),
                WARN,
                "Aplicar",
                enabled,
            );
            if ok {
                intents.cpu = Some((long, short));
                lstate.pending_cpu = None;
            }
            if cancel {
                lstate.pending_cpu = None;
            }
        }
    });
}

fn limit_row(
    ui: &mut egui::Ui,
    label: &str,
    limit: &Limit,
    value: &mut i64,
    enabled: bool,
) -> bool {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 20.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [(width - 96.0).max(40.0), 20.0],
                egui::Label::new(RichText::new(label).size(10.5).color(MUTED)).truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{value} W"))
                        .size(13.0)
                        .monospace()
                        .strong()
                        .color(if enabled { BRASS } else { FAINT }),
                );
            });
        },
    );
    ui.add_space(3.0);
    let response = brass_slider(ui, value, limit.min, limit.max, limit.step, width, enabled);
    response.changed()
}

fn legion_curve_card(
    ui: &mut egui::Ui,
    width: f32,
    view: &LegionView,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
    enabled: bool,
) {
    card(ui, width, |ui| {
        section(ui, "Curva de ventiladores", Some("10 puntos"));
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Punto 1 = régimen más frío, punto 10 = más caliente. La altura es el nivel de duty que Domador le pide al EC (1 = mínimo, 10 = tope); la línea tenue es la tabla de fábrica.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        ui.add_space(8.0);
        if curve_editor(ui, &mut lstate.curve, &view.stock, enabled) {
            lstate.curve_edited = true;
        }
        ui.add_space(10.0);
        let presets: [(&str, [i64; 10]); 4] = [
            ("Silencioso", [2, 2, 3, 3, 4, 4, 5, 6, 7, 8]),
            ("Equilibrado", [3, 3, 4, 4, 5, 5, 6, 7, 8, 9]),
            ("Agresivo", [4, 5, 5, 6, 7, 8, 8, 9, 10, 10]),
            ("Full blast", [10; 10]),
        ];
        let items: Vec<(&str, bool)> = presets
            .iter()
            .map(|(label, table)| (*label, lstate.curve == *table && !lstate.curve_edited))
            .collect();
        if let Some(idx) = option_grid(ui, &items, 84.0, 26.0, enabled) {
            lstate.curve = presets[idx].1;
            lstate.curve_edited = true;
        }
        ui.add_space(10.0);
        let inner = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(inner, 28.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                if btn(
                    ui,
                    "Aplicar curva",
                    128.0,
                    28.0,
                    Btn::Primary,
                    enabled && lstate.curve_edited,
                )
                .on_hover_text("Manda los diez puntos a /api/legion/fans")
                .clicked()
                {
                    intents.curve = Some(lstate.curve.to_vec());
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if lstate.curve_edited {
                        "Hay cambios sin aplicar"
                    } else {
                        "Curva en sincronía con el EC"
                    })
                    .size(10.0)
                    .color(if lstate.curve_edited { WARN } else { FAINT }),
                );
            },
        );
    });
}

fn competitor_row(ui: &mut egui::Ui, competitor: &Competitor, closed: bool, chief: bool) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 22.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            dot(ui, if closed { OK } else { WARN }, 3.0);
            ui.add_space(2.0);
            ui.add_sized(
                [(width - 156.0).max(40.0), 22.0],
                egui::Label::new(RichText::new(&competitor.name).size(11.0).color(if closed {
                    MUTED
                } else {
                    TEXT
                }))
                .truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if closed {
                    badge(ui, "CERRADO", OK, a(OK, 22));
                } else if chief {
                    badge(ui, "A CERRAR", WARN, a(WARN, 22));
                } else {
                    badge(ui, "ABIERTO", WARN, a(WARN, 22));
                }
                ui.label(
                    RichText::new(format!("PID {}", competitor.pid))
                        .size(9.5)
                        .monospace()
                        .color(FAINT),
                );
            });
        },
    );
}

fn legion_chief_card(
    ui: &mut egui::Ui,
    width: f32,
    view: &LegionView,
    closed: &[String],
    intents: &mut LegionIntent,
    enabled: bool,
) {
    card(ui, width, |ui| {
        section(ui, "Modo Jefe", Some("cerrojo del EC"));
        let inner = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(inner, 30.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                let response = switch(ui, Id::new("legion-chief"), view.chief);
                if response.clicked() && enabled {
                    intents.chief = Some(!view.chief);
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if view.chief {
                        "Domador manda"
                    } else {
                        "Modo Jefe apagado"
                    })
                    .size(12.5)
                    .strong()
                    .color(if view.chief { BRASS } else { MUTED }),
                );
            },
        );
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Domador activo = jefe de todo lo demás. Con el Modo Jefe prendido, Domador cierra Vantage, Legion Arena, Legion Toolkit (LLT), LFC, UXTU y Ryzen Controller: todos escriben el mismo registro del EC, y mientras estén abiertos se pisan entre sí y el modo térmico salta solo.",
                )
                .size(10.5)
                .color(MUTED),
            )
            .wrap(),
        );
        ui.add_space(10.0);
        eyebrow(ui, "Competidores detectados");
        ui.add_space(5.0);
        if view.competitors.is_empty() {
            ui.horizontal(|ui| {
                dot(ui, OK, 3.0);
                ui.label(
                    RichText::new("Ninguno corriendo: el EC queda sólo para Domador.")
                        .size(10.5)
                        .color(MUTED),
                );
            });
        } else {
            for competitor in &view.competitors {
                let was_closed = competitor.closed
                    || closed
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&competitor.name));
                competitor_row(ui, competitor, was_closed, view.chief);
                ui.add_space(2.0);
            }
        }
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "El cierre es de proceso, no de instalación: si Windows o el usuario los vuelve a abrir, Modo Jefe los cierra otra vez en el siguiente tick.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
    });
}

fn legion_restore_strip(
    ui: &mut egui::Ui,
    width: f32,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
    enabled: bool,
) {
    let frame = egui::Frame::new()
        .fill(a(COPPER, 10))
        .stroke(mk_stroke(1.0, a(COPPER, 90)))
        .corner_radius(9.0)
        .inner_margin(Margin::symmetric(13, 11))
        .show(ui, |ui| {
            let inner = (width - 26.0).max(80.0);
            ui.set_width(inner);
            ui.set_max_width(inner);
            if lstate.confirm_restore {
                let (ok, cancel) = confirm_strip(
                    ui,
                    "Volver a valores de fábrica",
                    "Se resetean modo térmico, curva, full speed y límites de CPU a lo que reporta el EC de fábrica. Es el botón de pánico si algo quedó raro.",
                    COPPER,
                    "Restaurar",
                    enabled,
                );
                if ok {
                    intents.restore = true;
                }
                if cancel {
                    lstate.confirm_restore = false;
                }
            } else {
                ui.horizontal(|ui| {
                    let text_w = (inner - 168.0).max(120.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(text_w, 0.0),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            ui.label(RichText::new("Restaurar stock").size(12.0).strong().color(TEXT));
                            ui.add_space(2.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Devuelve térmica, curva y límites a los valores de fábrica del EC.",
                                    )
                                    .size(10.0)
                                    .color(MUTED),
                                )
                                .wrap(),
                            );
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if btn(ui, "Restaurar stock", 148.0, 28.0, Btn::Danger, enabled)
                            .on_hover_text("POST /api/legion/restore · pide confirmación")
                            .clicked()
                        {
                            lstate.confirm_restore = true;
                        }
                    });
                });
            }
        });
    rivets(ui, frame.response.rect);
}

// ----------------------------------------------------------- modo auto EC ---

/// Estado de `/api/legion/adaptive`: el lazo que mira la temperatura y mueve
/// ventiladores y modo térmico por escalones con histéresis. La UI no decide
/// nada: muestra lo que hace el motor y le manda los límites que el usuario
/// toca.
#[derive(Clone)]
struct AdaptiveData {
    enabled: bool,
    target_c: i64,
    hot_c: i64,
    critical_c: i64,
    dwell_s: Option<f64>,
    cooldown_s: Option<f64>,
    state: String,
    last_action: Option<String>,
}

#[derive(Clone, Debug)]
enum AdaptiveErr {
    /// 404: el motor todavía no publica el lazo.
    Pending,
    Offline,
    Other(String),
}

impl AdaptiveErr {
    fn from_status(code: u16, body: String) -> Self {
        match code {
            404 => Self::Pending,
            _ => {
                let body: String = body.trim().chars().take(140).collect();
                if body.is_empty() {
                    Self::Other(format!("HTTP {code}"))
                } else {
                    Self::Other(format!("HTTP {code} · {body}"))
                }
            }
        }
    }

    fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Pending => "Backend pendiente",
            Self::Offline => "Motor sin conexión",
            Self::Other(_) => "El lazo devolvió un error",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::Pending => "GET /api/legion/adaptive responde 404: el motor todavía no publica el piloto automático. La tarjeta queda armada con los valores por defecto (82 / 85 / 92 °C) y se habilita sola cuando exista el lazo.".into(),
            Self::Offline => "No hay respuesta en el motor local. Arrancalo con `domador run --no-open` y reintentá.".into(),
            Self::Other(text) => text.clone(),
        }
    }

    fn color(&self) -> Color32 {
        match self {
            Self::Pending => WARN,
            Self::Offline => DANGER,
            Self::Other(_) => COPPER_HI,
        }
    }
}

#[derive(Clone, Default)]
struct Adaptive {
    data: Option<Arc<AdaptiveData>>,
    error: Option<AdaptiveErr>,
}

fn get_adaptive() -> Result<AdaptiveData, AdaptiveErr> {
    match ureq::get(&format!("{}/api/legion/adaptive", base())).call() {
        Ok(response) => {
            let value: Value = response
                .into_json()
                .map_err(|err| AdaptiveErr::Other(format!("respuesta inválida: {err}")))?;
            Ok(AdaptiveData {
                enabled: jbool(&value, &["enabled", "on", "active"]).unwrap_or(false),
                target_c: sample_num(&value, &["target_c", "target"]).unwrap_or(82.0) as i64,
                hot_c: sample_num(&value, &["hot_c", "hot"]).unwrap_or(85.0) as i64,
                critical_c: sample_num(&value, &["critical_c", "critical"]).unwrap_or(92.0) as i64,
                dwell_s: sample_num(&value, &["dwell_s", "dwell", "hysteresis_s"]),
                cooldown_s: sample_num(&value, &["cooldown_s", "cooldown"]),
                state: jstr(&value, &["state", "level", "stage"])
                    .unwrap_or("normal")
                    .to_ascii_lowercase(),
                last_action: jstr(&value, &["last_action", "action"]).map(str::to_string),
            })
        }
        Err(ureq::Error::Status(code, response)) => Err(AdaptiveErr::from_status(
            code,
            response.into_string().unwrap_or_default(),
        )),
        Err(_) => Err(AdaptiveErr::Offline),
    }
}

/// Relee el lazo en un hilo aparte y lo guarda en el estado compartido.
fn refresh_adaptive(shared: &Arc<Mutex<Shared>>) {
    let result = get_adaptive();
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    match result {
        Ok(data) => {
            guard.adaptive.data = Some(Arc::new(data));
            guard.adaptive.error = None;
        }
        Err(err) => guard.adaptive.error = Some(err),
    }
}

// ------------------------------------------------------------- telemetría ---

/// Azul pátina para el segundo ventilador: mantiene la paleta de latón y
/// cobre, pero separa las dos líneas de RPM.
const PATINA: Color32 = Color32::from_rgb(122, 176, 196);

#[derive(Clone, Copy)]
struct Stats {
    min: f64,
    max: f64,
    avg: f64,
    last: Option<f64>,
}

fn stat_of(samples: &[TeleSample], pick: impl Fn(&TeleSample) -> Option<f64>) -> Option<Stats> {
    let mut n = 0usize;
    let mut sum = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for sample in samples {
        if let Some(value) = pick(sample).filter(|v| v.is_finite()) {
            n += 1;
            sum += value;
            min = min.min(value);
            max = max.max(value);
        }
    }
    if n == 0 {
        return None;
    }
    Some(Stats {
        min,
        max,
        avg: sum / n as f64,
        last: samples.iter().rev().find_map(|s| pick(s)),
    })
}

/// Segundos con la serie por encima del límite. Un hueco mayor a 30 s no se
/// cuenta como tiempo caliente: el muestreo normal es de segundos.
fn seconds_over(
    samples: &[TeleSample],
    pick: impl Fn(&TeleSample) -> Option<f64>,
    limit: f64,
) -> f64 {
    let mut total = 0.0;
    for pair in samples.windows(2) {
        let dt = (pair[1].ts - pair[0].ts).clamp(0.0, 30.0);
        if let (Some(a), Some(b)) = (pick(&pair[0]), pick(&pair[1])) {
            if (a + b) * 0.5 > limit {
                total += dt;
            }
        }
    }
    total
}

/// Pendiente de la temperatura de CPU en °C por minuto (mínimos cuadrados).
fn cpu_slope(samples: &[TeleSample]) -> Option<f64> {
    let points: Vec<(f64, f64)> = samples
        .iter()
        .filter_map(|s| s.cpu.map(|cpu| (s.ts, cpu)))
        .collect();
    if points.len() < 6 {
        return None;
    }
    let t0 = points[0].0;
    let n = points.len() as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (ts, cpu) in &points {
        let x = (ts - t0) / 60.0;
        sx += x;
        sy += cpu;
        sxx += x * x;
        sxy += x * cpu;
    }
    let den = n * sxx - sx * sx;
    if den.abs() < 1e-9 {
        return None;
    }
    Some((n * sxy - sx * sy) / den)
}

fn avg_opt<T>(rows: &[T], pick: impl Fn(&T) -> Option<f64>) -> Option<f64> {
    let mut n = 0.0;
    let mut sum = 0.0;
    for row in rows {
        if let Some(value) = pick(row) {
            n += 1.0;
            sum += value;
        }
    }
    (n > 0.0).then(|| sum / n)
}

struct FanInsight {
    rpm_low: f64,
    rpm_high: f64,
    cpu_low: f64,
    cpu_high: f64,
    gpu_low: Option<f64>,
    gpu_high: Option<f64>,
}

/// ¿Se nota el aire? Parte las muestras por RPM (mitad baja contra mitad
/// alta) y compara la temperatura media: si con más RPM baja, el aire está
/// haciendo su trabajo.
fn fan_insight(samples: &[TeleSample]) -> Option<FanInsight> {
    let mut rows: Vec<(f64, f64, Option<f64>)> = Vec::new();
    for sample in samples {
        let rpm = match (sample.fan1, sample.fan2) {
            (Some(a), Some(b)) => a + b,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => continue,
        };
        if let Some(cpu) = sample.cpu.filter(|v| v.is_finite()) {
            rows.push((rpm, cpu, sample.gpu));
        }
    }
    if rows.len() < 8 {
        return None;
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mid = rows.len() / 2;
    let (low, high) = rows.split_at(mid);
    let rpm_low = low.iter().map(|r| r.0).sum::<f64>() / low.len().max(1) as f64;
    let rpm_high = high.iter().map(|r| r.0).sum::<f64>() / high.len().max(1) as f64;
    Some(FanInsight {
        rpm_low,
        rpm_high,
        cpu_low: avg_opt(low, |r| Some(r.1))?,
        cpu_high: avg_opt(high, |r| Some(r.1))?,
        gpu_low: avg_opt(low, |r| r.2),
        gpu_high: avg_opt(high, |r| r.2),
    })
}

fn nice_bounds(min: f64, max: f64) -> (f64, f64) {
    if !(min.is_finite() && max.is_finite()) {
        return (0.0, 1.0);
    }
    if (max - min).abs() < 1e-6 {
        let pad = max.abs().max(1.0) * 0.1;
        return (min - pad, max + pad);
    }
    let pad = (max - min) * 0.12;
    let (lo, hi) = (min - pad, max + pad);
    let raw = (hi - lo) / 3.0;
    let magnitude = 10f64.powf(raw.log10().floor());
    let norm = raw / magnitude;
    let step = if norm >= 5.0 {
        5.0
    } else if norm >= 2.0 {
        2.0
    } else {
        1.0
    } * magnitude;
    ((lo / step).floor() * step, (hi / step).ceil() * step)
}

fn fmt_axis(value: f64) -> String {
    if value.abs() >= 1000.0 {
        format!("{:.1}k", value / 1000.0)
    } else {
        format!("{value:.0}")
    }
}

fn fmt_v(value: Option<f64>, digits: usize, unit: &str) -> String {
    match value {
        Some(v) if v.is_finite() => {
            if unit.is_empty() {
                format!("{v:.digits$}")
            } else {
                format!("{v:.digits$} {unit}")
            }
        }
        _ => "—".to_string(),
    }
}

fn fmt_dur(seconds: f64) -> String {
    let secs = seconds.max(0.0).round() as u64;
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min {:02} s", secs / 60, secs % 60)
    }
}

fn compact_secs(seconds: f64) -> String {
    let secs = seconds.max(0.0).round() as u64;
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h {:02} min", secs / 3600, (secs % 3600) / 60)
    }
}

/// Bloque de estado con botón: mismo lenguaje que los avisos, pero adentro de
/// una tarjeta y con acción propia (Reintentar / pedir de nuevo).
fn tele_state(
    ui: &mut egui::Ui,
    title: &str,
    body: &str,
    color: Color32,
    action: &str,
    enabled: bool,
) -> bool {
    let width = ui.available_width();
    let mut clicked = false;
    let frame = egui::Frame::new()
        .fill(a(color, 14))
        .stroke(mk_stroke(1.0, a(color, 90)))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(11, 9))
        .show(ui, |ui| {
            let inner = (width - 22.0).max(80.0);
            ui.set_width(inner);
            ui.set_max_width(inner);
            ui.horizontal(|ui| {
                dot(ui, color, 3.5);
                ui.label(RichText::new(title).size(11.5).strong().color(color));
            });
            ui.add_space(3.0);
            ui.add(egui::Label::new(RichText::new(body).size(10.0).color(MUTED)).wrap());
            ui.add_space(7.0);
            if btn(ui, action, 104.0, 26.0, Btn::Secondary, enabled).clicked() && enabled {
                clicked = true;
            }
        });
    rivets(ui, frame.response.rect);
    clicked
}

fn num_cell(ui: &mut egui::Ui, width: f32, text: &str, color: Color32, size: f32, strong: bool) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 18.0),
        egui::Layout::right_to_left(Align::Center),
        |ui| {
            let mut rich = RichText::new(text).size(size).monospace().color(color);
            if strong {
                rich = rich.strong();
            }
            ui.add(egui::Label::new(rich).truncate());
        },
    );
}

fn telemetry_kv(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    let width = ui.available_width();
    let value_width = (width * 0.62).clamp(110.0, 320.0);
    let label_width = (width - value_width - 8.0).max(40.0);
    ui.allocate_ui_with_layout(
        Vec2::new(width, 19.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [label_width, 19.0],
                egui::Label::new(RichText::new(label).size(10.5).color(MUTED)).truncate(),
            );
            ui.allocate_ui_with_layout(
                Vec2::new(value_width, 19.0),
                egui::Layout::right_to_left(Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(value)
                                .size(11.0)
                                .monospace()
                                .strong()
                                .color(color),
                        )
                        .truncate(),
                    );
                },
            );
        },
    );
}

/// Tabla compacta de la ventana: actual / mínimo / promedio / máximo.
fn stat_table(ui: &mut egui::Ui, rows: &[(&str, Color32, Option<Stats>, usize)]) {
    let width = ui.available_width().max(200.0);
    let gap = 6.0;
    let cell = ((width - 90.0) / 4.0 - gap).clamp(38.0, 74.0);
    let label_width = (width - 4.0 * cell - 4.0 * gap).max(52.0);
    ui.allocate_ui_with_layout(
        Vec2::new(width, 16.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_sized(
                [label_width, 16.0],
                egui::Label::new(RichText::new("SERIE").size(8.5).strong().color(FAINT)).truncate(),
            );
            for title in ["ACTUAL", "MÍN", "PROM", "MÁX"] {
                num_cell(ui, cell, title, FAINT, 8.5, false);
            }
        },
    );
    ui.add_space(3.0);
    for (label, color, stats, digits) in rows {
        let (last, min, avg, max) = match stats {
            Some(s) => (s.last, Some(s.min), Some(s.avg), Some(s.max)),
            None => (None, None, None, None),
        };
        ui.allocate_ui_with_layout(
            Vec2::new(width, 18.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.add_sized(
                    [label_width, 18.0],
                    egui::Label::new(RichText::new(*label).size(10.5).color(MUTED)).truncate(),
                );
                num_cell(ui, cell, &fmt_v(last, *digits, ""), *color, 10.5, true);
                num_cell(ui, cell, &fmt_v(min, *digits, ""), MUTED, 10.5, false);
                num_cell(ui, cell, &fmt_v(avg, *digits, ""), TEXT, 10.5, true);
                num_cell(ui, cell, &fmt_v(max, *digits, ""), *color, 10.5, false);
            },
        );
    }
}

/// Gráfico de líneas dibujado a mano con el painter: grilla, límite, leyenda
/// con el último valor y cruz de lectura. Sin dependencias nuevas.
fn tele_chart(
    ui: &mut egui::Ui,
    height: f32,
    xs: &[f32],
    series: &[(&str, Color32, &[Option<f64>])],
    unit: &str,
    limit: Option<f64>,
    left_label: &str,
    hover: &mut Option<usize>,
) {
    let width = ui.available_width().max(180.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 8.0, SUNKEN);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + 18.0),
        Pos2::new(
            (rect.right() - 38.0).max(rect.left() + 80.0),
            rect.bottom() - 15.0,
        ),
    );
    if plot.width() < 40.0 || plot.height() < 24.0 {
        return;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (_, _, values) in series.iter() {
        for value in values.iter().flatten() {
            if value.is_finite() {
                lo = lo.min(*value);
                hi = hi.max(*value);
            }
        }
    }
    let (lo, hi) = if lo.is_finite() && hi.is_finite() {
        nice_bounds(lo, hi)
    } else {
        (0.0, 1.0)
    };
    let span = (hi - lo).max(1e-6);
    let y_of =
        |value: f64| plot.bottom() - (((value - lo) / span).clamp(0.0, 1.0) as f32) * plot.height();

    for band in 0..=3 {
        let frac = band as f32 / 3.0;
        let y = plot.bottom() - plot.height() * frac;
        painter.hline(
            plot.x_range(),
            y,
            mk_stroke(1.0, a(BORDER, if band == 0 { 110 } else { 55 })),
        );
        painter.text(
            Pos2::new(plot.right() + 4.0, y),
            Align2::LEFT_CENTER,
            fmt_axis(lo + span * frac as f64),
            FontId::monospace(8.0),
            FAINT,
        );
    }
    if let Some(limit) = limit {
        if limit > lo && limit < hi {
            let y = y_of(limit);
            painter.hline(plot.x_range(), y, mk_stroke(1.0, a(DANGER, 110)));
            painter.text(
                Pos2::new(plot.right() - 2.0, y - 2.0),
                Align2::RIGHT_BOTTOM,
                format!("{limit:.0}"),
                FontId::monospace(8.0),
                a(DANGER, 190),
            );
        }
    }
    painter.text(
        Pos2::new(rect.right() - 6.0, rect.top() + 8.0),
        Align2::RIGHT_CENTER,
        unit,
        FontId::monospace(8.5),
        FAINT,
    );

    // Leyenda con el último valor de cada serie.
    let font = FontId::proportional(9.0);
    let mut lx = plot.left() + 2.0;
    for (label, color, values) in series.iter() {
        if lx + 16.0 > plot.right() {
            break;
        }
        painter.hline(lx..=(lx + 9.0), rect.top() + 8.0, mk_stroke(2.0, *color));
        lx += 12.0;
        let last = values.iter().rev().flatten().next().copied();
        let text = match last {
            Some(v) if unit == "rpm" => format!("{label} {v:.0}"),
            Some(v) => format!("{label} {v:.1}{unit}"),
            None => (*label).to_string(),
        };
        painter.text(
            Pos2::new(lx, rect.top() + 8.0),
            Align2::LEFT_CENTER,
            &text,
            font.clone(),
            *color,
        );
        lx += text_width(ui, &text, font.clone()) + 12.0;
    }

    // Series: se corta la línea cuando falta el dato.
    for (_, color, values) in series.iter() {
        let mut segment: Vec<Pos2> = Vec::new();
        for (i, value) in values.iter().enumerate() {
            match value.filter(|v| v.is_finite()) {
                Some(v) => {
                    let x = plot.left()
                        + plot.width() * xs.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                    segment.push(Pos2::new(x, y_of(v)));
                }
                None => {
                    if segment.len() >= 2 {
                        painter.add(egui::Shape::line(
                            std::mem::take(&mut segment),
                            mk_stroke(1.4, *color),
                        ));
                    } else {
                        segment.clear();
                    }
                }
            }
        }
        if segment.len() >= 2 {
            painter.add(egui::Shape::line(segment, mk_stroke(1.4, *color)));
        }
    }

    painter.text(
        Pos2::new(plot.left() + 1.0, rect.bottom() - 2.0),
        Align2::LEFT_BOTTOM,
        left_label,
        FontId::proportional(8.5),
        FAINT,
    );
    painter.text(
        Pos2::new(plot.right(), rect.bottom() - 2.0),
        Align2::RIGHT_BOTTOM,
        "ahora",
        FontId::proportional(8.5),
        FAINT,
    );

    if let Some(pos) = response.hover_pos() {
        if plot.contains(pos) && !xs.is_empty() {
            let frac = ((pos.x - plot.left()) / plot.width().max(1.0)).clamp(0.0, 1.0);
            let mut best = 0usize;
            let mut best_d = f32::INFINITY;
            for (i, x) in xs.iter().enumerate() {
                let d = (*x - frac).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            *hover = Some(best);
            let x = plot.left() + plot.width() * xs.get(best).copied().unwrap_or(0.0);
            painter.vline(x, plot.y_range(), mk_stroke(1.0, a(BRASS, 110)));
            for (_, color, values) in series.iter() {
                if let Some(Some(v)) = values.get(best) {
                    if v.is_finite() {
                        painter.circle_filled(Pos2::new(x, y_of(*v)), 2.6, *color);
                    }
                }
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }
}

fn tele_tile(
    ui: &mut egui::Ui,
    width: f32,
    label: &str,
    stats: Option<Stats>,
    scale: f64,
    digits: usize,
    unit: &str,
    color: Color32,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width.max(50.0), 60.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 8.0, SUNKEN);
    painter.rect_stroke(rect, 8.0, mk_stroke(1.0, BORDER_SOFT), StrokeKind::Inside);
    painter.text(
        Pos2::new(rect.left() + 9.0, rect.top() + 6.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(9.0),
        FAINT,
    );
    let value = stats.and_then(|s| s.last);
    let (text, ink) = match value {
        Some(v) if v.is_finite() => (fmt_v(Some(v), digits, unit), color),
        _ => ("—".to_string(), FAINT),
    };
    painter.text(
        Pos2::new(rect.left() + 9.0, rect.top() + 18.0),
        Align2::LEFT_TOP,
        &text,
        FontId::monospace(15.0),
        ink,
    );
    if let Some(s) = stats {
        painter.text(
            Pos2::new(rect.left() + 9.0, rect.top() + 38.0),
            Align2::LEFT_TOP,
            format!("{:.digits$} / {:.digits$}", s.min, s.max),
            FontId::monospace(8.5),
            FAINT,
        );
    }
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + 9.0, rect.bottom() - 9.0),
        Pos2::new(rect.right() - 9.0, rect.bottom() - 6.0),
    );
    painter.rect_filled(track, 1.5, a(BORDER, 90));
    let frac = (value.unwrap_or(0.0) / scale.max(1e-6)).clamp(0.0, 1.0) as f32;
    if frac > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, Vec2::new(track.width() * frac, track.height())),
            1.5,
            a(ink, 200),
        );
    }
}

fn legion_telemetry_card(
    ui: &mut egui::Ui,
    width: f32,
    tele: &Telemetry,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
) {
    card(ui, width, |ui| {
        let refreshed = match tele.fetched {
            Some(at) => format!("hace {}", compact_secs(at.elapsed().as_secs_f64())),
            None => "sin lecturas".to_string(),
        };
        section(ui, "Telemetría", Some(refreshed.as_str()));

        let ranges: [u32; 3] = [10, 30, 60];
        let items: [(&str, bool); 3] = [
            ("10 min", lstate.tele_range == 10),
            ("30 min", lstate.tele_range == 30),
            ("60 min", lstate.tele_range == 60),
        ];
        if let Some(idx) = option_grid(ui, &items, 70.0, 26.0, true) {
            lstate.tele_range = ranges[idx];
        }
        ui.add_space(7.0);
        ui.horizontal_wrapped(|ui| {
            if btn(ui, "Actualizar", 92.0, 26.0, Btn::Secondary, !tele.loading).clicked() {
                intents.tele_fetch = true;
            }
            if btn(ui, "Exportar CSV", 108.0, 26.0, Btn::Secondary, true).clicked() {
                intents.tele_export = true;
            }
            if switch(ui, Id::new("legion-tele-live"), lstate.tele_auto).clicked() {
                lstate.tele_auto = !lstate.tele_auto;
            }
            ui.label(
                RichText::new(if lstate.tele_auto {
                    "En vivo"
                } else {
                    "Manual"
                })
                .size(10.0)
                .color(if lstate.tele_auto { MUTED } else { FAINT }),
            );
        });
        ui.add_space(9.0);

        if let Some(err) = &tele.error {
            if tele_state(
                ui,
                err.title(),
                &err.body(),
                err.color(),
                "Reintentar",
                !tele.loading,
            ) {
                intents.tele_fetch = true;
            }
            return;
        }
        let Some(data) = tele.data.as_ref() else {
            ui.add(
                egui::Label::new(
                    RichText::new(if tele.loading {
                        "Leyendo el histórico del motor…"
                    } else {
                        "Todavía no hay histórico en memoria: pedilo con Actualizar."
                    })
                    .size(10.5)
                    .color(FAINT),
                )
                .wrap(),
            );
            return;
        };
        if data.samples.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "El motor responde pero no hay muestras en esta ventana. Dejalo unos segundos y reintentá.",
                    )
                    .size(10.5)
                    .color(FAINT),
                )
                .wrap(),
            );
            return;
        }

        let samples = &data.samples;
        let t0 = samples.first().map(|s| s.ts).unwrap_or(0.0);
        let t1 = samples.last().map(|s| s.ts).unwrap_or(t0);
        let span = (t1 - t0).max(1.0);
        let xs: Vec<f32> = samples
            .iter()
            .map(|s| (((s.ts - t0) / span) as f32).clamp(0.0, 1.0))
            .collect();
        let cpu: Vec<Option<f64>> = samples.iter().map(|s| s.cpu).collect();
        let gpu: Vec<Option<f64>> = samples.iter().map(|s| s.gpu).collect();
        let fan1: Vec<Option<f64>> = samples.iter().map(|s| s.fan1).collect();
        let fan2: Vec<Option<f64>> = samples.iter().map(|s| s.fan2).collect();
        let cpu_stats = stat_of(samples, |s| s.cpu);
        let gpu_stats = stat_of(samples, |s| s.gpu);
        let fan1_stats = stat_of(samples, |s| s.fan1);
        let fan2_stats = stat_of(samples, |s| s.fan2);
        let has_fans = fan1_stats.is_some() || fan2_stats.is_some();

        let rpm_scale = fan1
            .iter()
            .chain(fan2.iter())
            .filter_map(|v| *v)
            .fold(3600.0_f64, f64::max);
        let inner = ui.available_width();
        let gap = 7.0;
        let cell = ((inner - gap * 3.0) / 4.0).floor().max(58.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            tele_tile(ui, cell, "CPU", cpu_stats, 100.0, 1, "°C", BRASS);
            tele_tile(ui, cell, "GPU", gpu_stats, 100.0, 1, "°C", COPPER_HI);
            tele_tile(ui, cell, "FAN 1", fan1_stats, rpm_scale, 0, "rpm", PATINA);
            tele_tile(ui, cell, "FAN 2", fan2_stats, rpm_scale, 0, "rpm", OK);
        });

        let range_label = format!("-{} min", data.minutes);
        let mut hover: Option<usize> = None;
        ui.add_space(10.0);
        eyebrow(ui, "Temperaturas");
        ui.add_space(4.0);
        tele_chart(
            ui,
            116.0,
            &xs,
            &[("CPU", BRASS, &cpu), ("GPU", COPPER_HI, &gpu)],
            "°C",
            Some(90.0),
            &range_label,
            &mut hover,
        );
        if has_fans {
            ui.add_space(8.0);
            eyebrow(ui, "Ventiladores");
            ui.add_space(4.0);
            tele_chart(
                ui,
                86.0,
                &xs,
                &[("Fan 1", PATINA, &fan1), ("Fan 2", OK, &fan2)],
                "rpm",
                None,
                &range_label,
                &mut hover,
            );
        }
        if let Some(sample) = hover.and_then(|i| samples.get(i)) {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "hace {} · CPU {} · GPU {} · F1 {}{}",
                        compact_secs((t1 - sample.ts).max(0.0)),
                        fmt_v(sample.cpu, 1, "°C"),
                        fmt_v(sample.gpu, 1, "°C"),
                        fmt_v(sample.fan1, 0, "rpm"),
                        if sample.fan2.is_some() {
                            format!(" · F2 {}", fmt_v(sample.fan2, 0, "rpm"))
                        } else {
                            String::new()
                        }
                    ))
                    .size(9.5)
                    .monospace()
                    .color(MUTED),
                )
                .wrap(),
            );
        }

        ui.add_space(10.0);
        let cpu_over = seconds_over(samples, |s| s.cpu, 90.0);
        let gpu_over = seconds_over(samples, |s| s.gpu, 90.0);
        let pct = |secs: f64| if span > 0.0 { secs / span * 100.0 } else { 0.0 };
        telemetry_kv(
            ui,
            "CPU mín / prom / máx",
            &format!(
                "{} / {} / {} °C",
                fmt_v(cpu_stats.map(|s| s.min), 1, ""),
                fmt_v(cpu_stats.map(|s| s.avg), 1, ""),
                fmt_v(cpu_stats.map(|s| s.max), 1, "")
            ),
            BRASS,
        );
        telemetry_kv(
            ui,
            "GPU mín / prom / máx",
            &format!(
                "{} / {} / {} °C",
                fmt_v(gpu_stats.map(|s| s.min), 1, ""),
                fmt_v(gpu_stats.map(|s| s.avg), 1, ""),
                fmt_v(gpu_stats.map(|s| s.max), 1, "")
            ),
            COPPER_HI,
        );
        telemetry_kv(
            ui,
            "Sobre 90 °C",
            &format!(
                "CPU {} · GPU {}",
                fmt_dur(cpu_over) + &format!(" ({:.0} %)", pct(cpu_over)),
                fmt_dur(gpu_over) + &format!(" ({:.0} %)", pct(gpu_over))
            ),
            if cpu_over > 0.0 || gpu_over > 0.0 {
                WARN
            } else {
                OK
            },
        );
        telemetry_kv(
            ui,
            "Ventana",
            &format!(
                "{} muestras · {} de {} min",
                samples.len(),
                fmt_dur(span),
                data.minutes
            ),
            MUTED,
        );
    });
}

#[allow(dead_code)]
fn legion_telemetry_card_full(
    ui: &mut egui::Ui,
    width: f32,
    tele: &Telemetry,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
) {
    card(ui, width, |ui| {
        let refreshed = match tele.fetched {
            Some(at) => format!("hace {}", compact_secs(at.elapsed().as_secs_f64())),
            None => "sin lecturas".to_string(),
        };
        section(ui, "Telemetría", Some(refreshed.as_str()));

        let ranges: [u32; 3] = [10, 30, 60];
        let items: [(&str, bool); 3] = [
            ("10 min", lstate.tele_range == 10),
            ("30 min", lstate.tele_range == 30),
            ("60 min", lstate.tele_range == 60),
        ];
        if let Some(idx) = option_grid(ui, &items, 70.0, 26.0, true) {
            lstate.tele_range = ranges[idx];
        }
        ui.add_space(7.0);
        ui.horizontal(|ui| {
            if btn(ui, "Actualizar", 92.0, 26.0, Btn::Secondary, !tele.loading)
                .on_hover_text("GET /api/legion/history?minutes=N ahora mismo")
                .clicked()
            {
                intents.tele_fetch = true;
            }
            ui.add_space(4.0);
            if btn(ui, "Exportar CSV", 108.0, 26.0, Btn::Secondary, true)
                .on_hover_text(
                    "Pide /api/legion/export.csv; si el motor no lo publica, guarda las muestras en memoria",
                )
                .clicked()
            {
                intents.tele_export = true;
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if switch(ui, Id::new("legion-tele-live"), lstate.tele_auto).clicked() {
                    lstate.tele_auto = !lstate.tele_auto;
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if lstate.tele_auto { "En vivo" } else { "Manual" })
                        .size(10.0)
                        .color(if lstate.tele_auto { MUTED } else { FAINT }),
                );
            });
        });
        ui.add_space(9.0);

        if let Some(err) = &tele.error {
            if tele_state(
                ui,
                err.title(),
                &err.body(),
                err.color(),
                "Reintentar",
                !tele.loading,
            ) {
                intents.tele_fetch = true;
            }
            ui.add_space(8.0);
        }

        let Some(data) = tele.data.as_ref() else {
            if tele.error.is_none() {
                ui.add(
                    egui::Label::new(
                        RichText::new(if tele.loading {
                            "Leyendo el histórico del motor…"
                        } else {
                            "Todavía no hay histórico en memoria: pedilo con Actualizar."
                        })
                        .size(10.5)
                        .color(FAINT),
                    )
                    .wrap(),
                );
            }
            return;
        };
        if data.samples.is_empty() {
            if tele_state(
                ui,
                "Histórico vacío",
                "El motor responde pero no hay muestras en esta ventana: el buffer arranca con la primera lectura. Dejá la pestaña abierta unos segundos y reintentá.",
                WARN,
                "Reintentar",
                !tele.loading,
            ) {
                intents.tele_fetch = true;
            }
            return;
        }

        let samples = &data.samples;
        let t0 = samples.first().map(|s| s.ts).unwrap_or(0.0);
        let t1 = samples.last().map(|s| s.ts).unwrap_or(t0);
        let span = (t1 - t0).max(1.0);
        let xs: Vec<f32> = samples
            .iter()
            .map(|s| (((s.ts - t0) / span) as f32).clamp(0.0, 1.0))
            .collect();
        let cpu: Vec<Option<f64>> = samples.iter().map(|s| s.cpu).collect();
        let gpu: Vec<Option<f64>> = samples.iter().map(|s| s.gpu).collect();
        let fan1: Vec<Option<f64>> = samples.iter().map(|s| s.fan1).collect();
        let fan2: Vec<Option<f64>> = samples.iter().map(|s| s.fan2).collect();
        let cpu_stats = stat_of(samples, |s| s.cpu);
        let gpu_stats = stat_of(samples, |s| s.gpu);
        let fan1_stats = stat_of(samples, |s| s.fan1);
        let fan2_stats = stat_of(samples, |s| s.fan2);

        // Valores actuales.
        let rpm_scale = fan1
            .iter()
            .chain(fan2.iter())
            .filter_map(|v| *v)
            .fold(3600.0_f64, f64::max);
        let inner = ui.available_width();
        let gap = 7.0;
        let cell = ((inner - gap * 3.0) / 4.0).floor().max(64.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            tele_tile(ui, cell, "CPU", cpu_stats, 100.0, 1, "°C", BRASS);
            tele_tile(ui, cell, "GPU", gpu_stats, 100.0, 1, "°C", COPPER_HI);
            tele_tile(ui, cell, "FAN 1", fan1_stats, rpm_scale, 0, "rpm", PATINA);
            tele_tile(ui, cell, "FAN 2", fan2_stats, rpm_scale, 0, "rpm", OK);
        });

        // Gráficos.
        let range_label = format!("-{} min", data.minutes);
        let mut hover: Option<usize> = None;
        ui.add_space(11.0);
        eyebrow(ui, "Temperaturas");
        ui.add_space(4.0);
        tele_chart(
            ui,
            136.0,
            &xs,
            &[("CPU", BRASS, &cpu), ("GPU", COPPER_HI, &gpu)],
            "°C",
            Some(90.0),
            &range_label,
            &mut hover,
        );
        ui.add_space(9.0);
        eyebrow(ui, "Ventiladores");
        ui.add_space(4.0);
        tele_chart(
            ui,
            112.0,
            &xs,
            &[("Fan 1", PATINA, &fan1), ("Fan 2", OK, &fan2)],
            "rpm",
            None,
            &range_label,
            &mut hover,
        );
        if let Some(sample) = hover.and_then(|i| samples.get(i)) {
            ui.add_space(5.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "hace {} · CPU {} · GPU {} · F1 {} · F2 {}{}",
                        compact_secs((t1 - sample.ts).max(0.0)),
                        fmt_v(sample.cpu, 1, "°C"),
                        fmt_v(sample.gpu, 1, "°C"),
                        fmt_v(sample.fan1, 0, "rpm"),
                        fmt_v(sample.fan2, 0, "rpm"),
                        match sample.mode.as_deref() {
                            Some(mode) if !mode.is_empty() => format!(" · {}", pretty(mode)),
                            _ => String::new(),
                        }
                    ))
                    .size(9.5)
                    .monospace()
                    .color(MUTED),
                )
                .wrap(),
            );
        }

        // Resumen de la ventana.
        ui.add_space(11.0);
        eyebrow(ui, "Resumen de la ventana");
        ui.add_space(4.0);
        stat_table(
            ui,
            &[
                ("CPU °C", BRASS, cpu_stats, 1),
                ("GPU °C", COPPER_HI, gpu_stats, 1),
                ("Fan 1 rpm", PATINA, fan1_stats, 0),
                ("Fan 2 rpm", OK, fan2_stats, 0),
                ("GPU W", MUTED, stat_of(samples, |s| s.gpu_w), 1),
                ("GPU MHz", MUTED, stat_of(samples, |s| s.gpu_mhz), 0),
            ],
        );

        // Tiempo sobre 90 °C.
        let cpu_over = seconds_over(samples, |s| s.cpu, 90.0);
        let gpu_over = seconds_over(samples, |s| s.gpu, 90.0);
        let pct = |secs: f64| if span > 0.0 { secs / span * 100.0 } else { 0.0 };
        ui.add_space(10.0);
        eyebrow(ui, "Tiempo sobre 90 °C");
        ui.add_space(4.0);
        telemetry_kv(
            ui,
            "CPU",
            &format!("{} · {:.0} %", fmt_dur(cpu_over), pct(cpu_over)),
            if cpu_over > 0.0 { DANGER } else { OK },
        );
        telemetry_kv(
            ui,
            "GPU",
            &format!("{} · {:.0} %", fmt_dur(gpu_over), pct(gpu_over)),
            if gpu_over > 0.0 { DANGER } else { OK },
        );

        // Análisis: RPM contra temperatura y tendencia.
        ui.add_space(10.0);
        eyebrow(ui, "Análisis de la ventana");
        ui.add_space(4.0);
        if let Some(insight) = fan_insight(samples) {
            telemetry_kv(
                ui,
                "RPM bajo / alto",
                &format!("{:.0} / {:.0}", insight.rpm_low, insight.rpm_high),
                MUTED,
            );
            telemetry_kv(
                ui,
                "CPU con RPM bajo / alto",
                &format!("{:.1} / {:.1} °C", insight.cpu_low, insight.cpu_high),
                MUTED,
            );
            if let (Some(lo), Some(hi)) = (insight.gpu_low, insight.gpu_high) {
                telemetry_kv(
                    ui,
                    "GPU con RPM bajo / alto",
                    &format!("{lo:.1} / {hi:.1} °C"),
                    MUTED,
                );
            }
            ui.add_space(5.0);
            let delta = insight.cpu_high - insight.cpu_low;
            let (text, color) = if delta <= -1.5 {
                (
                    format!(
                        "Los ventiladores están haciendo su trabajo: con las RPM altas el CPU queda {:.1} °C más frío.",
                        -delta
                    ),
                    OK,
                )
            } else if delta >= 1.5 {
                (
                    format!(
                        "Con las RPM altas el CPU queda {:.1} °C más caliente: el aire no alcanza. Revisá la curva, el modo térmico o la entrada de aire.",
                        delta
                    ),
                    COPPER_HI,
                )
            } else {
                (
                    "Sin diferencia clara entre RPM bajas y altas en esta ventana: hace falta más carga o más tiempo para separar las dos mitades."
                        .to_string(),
                    WARN,
                )
            };
            ui.add(egui::Label::new(RichText::new(text).size(10.0).color(color)).wrap());
            ui.add_space(4.0);
        } else {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Hacen falta al menos ocho muestras con RPM y temperatura para comparar ventiladores contra temperatura.",
                    )
                    .size(10.0)
                    .color(FAINT),
                )
                .wrap(),
            );
            ui.add_space(4.0);
        }
        if let Some(slope) = cpu_slope(samples) {
            let (word, color) = if slope >= 0.15 {
                ("subiendo", COPPER_HI)
            } else if slope <= -0.15 {
                ("bajando", OK)
            } else {
                ("estable", MUTED)
            };
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Tendencia del CPU: {slope:+.2} °C/min en la ventana ({word})."
                    ))
                    .size(10.0)
                    .color(color),
                )
                .wrap(),
            );
        }

        // Cierre de sesión: qué se guardó y con qué ritmo.
        ui.add_space(10.0);
        eyebrow(ui, "Sesión");
        ui.add_space(4.0);
        let period = if samples.len() > 1 {
            span / (samples.len() - 1) as f64
        } else {
            0.0
        };
        telemetry_kv(
            ui,
            "Muestras",
            &format!("{} · cada {:.0} s", samples.len(), period.max(0.0)),
            MUTED,
        );
        telemetry_kv(
            ui,
            "Cobertura",
            &format!("{} de {} min", fmt_dur(span), data.minutes),
            MUTED,
        );
        if let Some(mode) = samples.iter().rev().find_map(|s| s.mode.as_deref()) {
            telemetry_kv(ui, "Modo (última muestra)", &pretty(mode), BRASS);
        }
    });
}

fn adaptive_state_style(state: &str) -> (&'static str, Color32) {
    let state = state.trim().to_ascii_lowercase();
    if state.starts_with("crit") {
        ("Crítico", DANGER)
    } else if state.starts_with("hot") || state.starts_with("warm") || state.starts_with("cal") {
        ("Caliente", WARN)
    } else if state.is_empty() {
        ("—", FAINT)
    } else {
        ("Normal", OK)
    }
}

fn temp_limit_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut i64,
    min: i64,
    max: i64,
    enabled: bool,
) -> bool {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        Vec2::new(width, 20.0),
        egui::Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_sized(
                [(width - 96.0).max(40.0), 20.0],
                egui::Label::new(RichText::new(label).size(10.5).color(MUTED)).truncate(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{value} °C"))
                        .size(13.0)
                        .monospace()
                        .strong()
                        .color(if enabled { BRASS } else { FAINT }),
                );
            });
        },
    );
    ui.add_space(3.0);
    brass_slider(ui, value, min, max, 1, width, enabled).changed()
}

fn legion_adaptive_card(
    ui: &mut egui::Ui,
    width: f32,
    adaptive: &Adaptive,
    lstate: &mut LegionUi,
    intents: &mut LegionIntent,
) {
    card(ui, width, |ui| {
        let data = adaptive.data.as_deref();
        let pending = adaptive
            .error
            .as_ref()
            .map(|e| e.pending())
            .unwrap_or(false);
        let live = data.is_some() && !pending;
        let (state_text, state_color) = match data {
            Some(data) if !pending => adaptive_state_style(&data.state),
            _ => ("—", FAINT),
        };
        section(ui, "AUTO / Inteligente", Some(state_text));

        if let Some(err) = &adaptive.error {
            if tele_state(
                ui,
                err.title(),
                &err.body(),
                err.color(),
                "Reintentar",
                true,
            ) {
                intents.adaptive_fetch = true;
            }
            ui.add_space(8.0);
        }

        let inner = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(inner, 28.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                if switch(ui, Id::new("legion-adaptive"), lstate.ad_enabled).clicked() && live {
                    lstate.ad_enabled = !lstate.ad_enabled;
                    lstate.ad_edited = false;
                    intents.adaptive = Some(json!({ "enabled": lstate.ad_enabled }));
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if lstate.ad_enabled {
                        "Domador decide solo"
                    } else {
                        "AUTO apagado"
                    })
                    .size(12.0)
                    .strong()
                    .color(if lstate.ad_enabled { BRASS } else { MUTED }),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    badge(
                        ui,
                        &state_text.to_uppercase(),
                        state_color,
                        a(state_color, 22),
                    );
                });
            },
        );
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "El motor mira la temperatura del EC y sube escalones solo: objetivo → caliente → crítico, con histéresis para no oscilar. La UI sólo muestra el estado y manda los límites; la decisión es del backend.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
        ui.add_space(9.0);

        eyebrow(ui, "Escalones");
        ui.add_space(2.0);
        if temp_limit_row(ui, "Objetivo", &mut lstate.ad_target, 60, 100, live) {
            lstate.ad_edited = true;
        }
        if temp_limit_row(ui, "Caliente", &mut lstate.ad_hot, 65, 105, live) {
            lstate.ad_edited = true;
        }
        if temp_limit_row(ui, "Crítico", &mut lstate.ad_critical, 70, 110, live) {
            lstate.ad_edited = true;
        }
        if !(lstate.ad_target < lstate.ad_hot && lstate.ad_hot <= lstate.ad_critical) {
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Los escalones tienen que subir: objetivo < caliente ≤ crítico. Así se aplican.",
                    )
                    .size(9.5)
                    .color(WARN),
                )
                .wrap(),
            );
        }
        ui.add_space(7.0);
        let ordered = lstate.ad_target < lstate.ad_hot && lstate.ad_hot <= lstate.ad_critical;
        if btn(
            ui,
            "Aplicar límites",
            124.0,
            28.0,
            Btn::Primary,
            live && lstate.ad_edited && ordered,
        )
        .on_hover_text("POST /api/legion/adaptive con target_c, hot_c y critical_c")
        .clicked()
        {
            intents.adaptive = Some(json!({
                "target_c": lstate.ad_target,
                "hot_c": lstate.ad_hot,
                "critical_c": lstate.ad_critical,
            }));
            lstate.ad_edited = false;
        }

        ui.add_space(9.0);
        eyebrow(ui, "Histéresis");
        ui.add_space(3.0);
        telemetry_kv(
            ui,
            "Dwell (sostener)",
            &match data.and_then(|d| d.dwell_s) {
                Some(v) => format!("{v:.0} s"),
                None => "— sin dato".to_string(),
            },
            MUTED,
        );
        telemetry_kv(
            ui,
            "Cooldown (bajar)",
            &match data.and_then(|d| d.cooldown_s) {
                Some(v) => format!("{v:.0} s"),
                None => "— sin dato".to_string(),
            },
            MUTED,
        );
        ui.add_space(8.0);
        eyebrow(ui, "Última acción");
        ui.add_space(3.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    data.and_then(|d| d.last_action.clone())
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or_else(|| {
                            if live {
                                "Sin acciones todavía: el lazo espera que la temperatura cruce un escalón.".to_string()
                            } else {
                                "—".to_string()
                            }
                        }),
                )
                .size(10.5)
                .color(MUTED),
            )
            .wrap(),
        );
    });
}

fn legion_preview(ui: &mut egui::Ui, width: f32) {
    card(ui, width, |ui| {
        section(ui, "Panel en espera", Some("/api/legion"));
        ui.label(
            RichText::new("La pestaña ya está armada y se llena sola cuando el motor publique el panel del EC:")
                .size(11.0)
                .color(MUTED),
        );
        ui.add_space(8.0);
        for (title, body) in [
            (
                "Equipo y térmicas",
                "Modelo, BIOS y temperatura de CPU/GPU leídos por el EC.",
            ),
            (
                "Ventiladores y modo térmico",
                "RPM de cada ventilador contra su máximo, y selector Silencioso / Equilibrado / Rendimiento / Custom.",
            ),
            (
                "Límites y curva",
                "PL1/PL2 con presets de 35, 45 y 65 W, más la curva de diez puntos (niveles 1 a 10) con full blast y restaurar stock.",
            ),
            (
                "Modo Jefe",
                "Cierra Vantage, Legion Arena, LLT, LFC, UXTU y Ryzen Controller para que no peleen por el EC.",
            ),
        ] {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, 10.0), Sense::hover());
                ui.painter().rect_filled(rect, 1.5, BRASS_DIM);
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(11.0).strong().color(TEXT));
                    ui.add(
                        egui::Label::new(RichText::new(body).size(10.0).color(MUTED)).wrap(),
                    );
                });
            });
            ui.add_space(6.0);
        }
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Mientras tanto Procesos, Salud y Ajustes funcionan igual: esta pestaña sólo agrega lecturas.",
                )
                .size(9.5)
                .color(FAINT),
            )
            .wrap(),
        );
    });
}

fn icon() -> Option<Arc<egui::IconData>> {
    let img = image::load_from_memory(include_bytes!("../../assets/icon-256.png"))
        .ok()?
        .to_rgba8();
    Some(Arc::new(egui::IconData {
        rgba: img.as_raw().clone(),
        width: img.width(),
        height: img.height(),
    }))
}

fn main() -> eframe::Result<()> {
    // Domador es una herramienta de administración: por defecto abre con UAC.
    // `DOMADOR_NO_ELEVATE=1` queda sólo para pruebas automatizadas.
    if !domador::win::is_admin() && std::env::var_os("DOMADOR_NO_ELEVATE").is_none() {
        relaunch_ui_admin();
        return Ok(());
    }
    ensure_server();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Domador")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([1024.0, 660.0])
        .with_maximized(true);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Domador",
        options,
        Box::new(|cc| Ok(Box::new(DomadorUi::new(cc)))),
    )
}
