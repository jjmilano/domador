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

const PORT: u16 = 8787;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

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

#[derive(Default)]
struct Shared {
    state: Option<Arc<Value>>,
    error: Option<String>,
    busy: Option<String>,
    toast: Option<Toast>,
    last_ok: Option<Instant>,
    detail: Option<Detail>,
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
}

#[derive(Clone, Copy)]
enum Icon {
    List,
    Pulse,
    Gear,
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
}

// ------------------------------------------------------------------- HTTP ---

fn base() -> String {
    format!("http://127.0.0.1:{PORT}")
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
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
        return;
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("domador.exe")));
    if let Some(exe) = exe {
        let _ = Command::new(exe)
            .args(["run", "--no-open"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    for _ in 0..30 {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
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
        Self {
            shared,
            tab: Tab::Processes,
            search: String::new(),
            sort: SortState {
                key: SortBy::Cpu,
                desc: true,
            },
            selected: None,
            kill_confirm: None,
            logo,
            focus_search: false,
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
        // Parar el motor primero: si sigue vivo, la UI elevada se conecta al
        // mismo servidor sin privilegios y el badge no cambia.
        let _ = post("/api/shutdown", json!({}));
        thread::sleep(Duration::from_millis(800));
        let exe = std::env::current_exe().unwrap_or_default();
        let exe = exe.to_string_lossy().replace('\'', "''");
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Start-Process -FilePath '{exe}' -Verb RunAs"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
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
        let uptime = state
            .as_ref()
            .and_then(|s| s.pointer("/app/uptime"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
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

                ui.add_space(18.0);
                let line = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(Vec2::new(line, 1.0), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, a(BORDER, 160));
                ui.add_space(14.0);

                if !compact {
                    eyebrow(ui, "Motor");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        dot(ui, if online { OK } else { DANGER }, 3.5);
                        ui.label(
                            RichText::new(if online { "En línea" } else { "Sin conexión" })
                                .size(11.0)
                                .color(if online { TEXT } else { DANGER }),
                        );
                    });
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(if online {
                            format!("arriba {}", fmt_uptime(uptime))
                        } else {
                            "arrancalo con domador run".to_string()
                        })
                        .size(9.5)
                        .color(FAINT),
                    );
                    ui.add_space(14.0);
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
                        RichText::new(format!("127.0.0.1:{PORT}"))
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
        let (find, refresh, one, two, three, escape) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && i.key_pressed(egui::Key::F),
                cmd && i.key_pressed(egui::Key::R),
                cmd && i.key_pressed(egui::Key::Num1),
                cmd && i.key_pressed(egui::Key::Num2),
                cmd && i.key_pressed(egui::Key::Num3),
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
                    |ui| kernel_card(ui, col, &state),
                );
            });
        } else {
            memory_card(ui, width, &state);
            ui.add_space(12.0);
            kernel_card(ui, width, &state);
        }

        ui.add_space(12.0);
        if width >= 1120.0 {
            // GPU y memoria privada comparten fila: dos tarjetas acotadas, sin
            // que ninguna crezca más allá del ancho disponible.
            let col = ((width - 12.0) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| gpu_card(ui, col, &state),
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| private_card(ui, col, &state),
                );
            });
        } else {
            gpu_card(ui, width, &state);
            ui.add_space(12.0);
            private_card(ui, width, &state);
        }
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
                            section(ui, "Administrador", None);
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
        if width >= 720.0 {
            let col = ((width - 12.0) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| motor_card(ui, col, state.as_deref()),
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(col, 0.0),
                    egui::Layout::top_down(Align::Min),
                    |ui| shortcuts_card(ui, col),
                );
            });
        } else {
            motor_card(ui, width, state.as_deref());
            ui.add_space(12.0);
            shortcuts_card(ui, width);
        }
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
    });
}

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
