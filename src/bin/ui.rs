//! UI nativa de Domador (egui/eframe).
//!
//! No usa Edge ni WebView: habla con el motor local por HTTP y dibuja con
//! OpenGL. El motor sigue siendo el mismo `domador.exe`; esta ventana es
//! descartable, igual que la UI web, pero pesa una fracción.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use serde_json::{json, Value};

const PORT: u16 = 8787;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

// Paleta cálida: el león dorado manda; el verde frío queda sólo para estados.
const C_BG: egui::Color32 = egui::Color32::from_rgb(18, 13, 10);
const C_PANEL: egui::Color32 = egui::Color32::from_rgb(26, 19, 15);
const C_CARD: egui::Color32 = egui::Color32::from_rgb(36, 26, 20);
const C_BORDER: egui::Color32 = egui::Color32::from_rgb(75, 52, 36);
const C_TEXT: egui::Color32 = egui::Color32::from_rgb(250, 240, 226);
const C_MUTED: egui::Color32 = egui::Color32::from_rgb(198, 175, 154);
const C_ACCENT: egui::Color32 = egui::Color32::from_rgb(245, 183, 92);
const C_CORAL: egui::Color32 = egui::Color32::from_rgb(233, 139, 106);
const C_GREEN: egui::Color32 = egui::Color32::from_rgb(143, 214, 168);
const C_RED: egui::Color32 = egui::Color32::from_rgb(240, 138, 122);

#[derive(Default)]
struct Shared {
    state: Option<Arc<Value>>,
    error: Option<String>,
    busy: Option<String>,
    toast: Option<(String, bool)>,
}

struct DomadorUi {
    shared: Arc<Mutex<Shared>>,
    tab: Tab,
    search: String,
    sort: SortBy,
    selected: Option<u32>,
    kill_confirm: Option<(u32, String)>,
    logo: Option<egui::TextureHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Cpu,
    Ram,
    Name,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Processes,
    Health,
    Settings,
}

impl Default for Tab {
    fn default() -> Self {
        Self::Processes
    }
}

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

fn poll_thread(shared: Arc<Mutex<Shared>>) {
    loop {
        match get_state() {
            Ok(value) => {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.state = Some(Arc::new(value));
                guard.error = None;
            }
            Err(err) => {
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.error = Some(err);
            }
        }
        thread::sleep(Duration::from_millis(700));
    }
}

fn act_bg(shared: Arc<Mutex<Shared>>, path: &'static str, body: Value, label: &'static str) {
    {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.busy = Some(label.into());
    }
    thread::spawn(move || {
        let result = post(path, body);
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.busy = None;
        match result {
            Ok(value) => {
                let msg = value
                    .get("freed")
                    .and_then(|v| v.as_u64())
                    .map(|b| format!("Listo: {:.0} MB liberados", b as f64 / MB))
                    .unwrap_or_else(|| "Listo".into());
                guard.toast = Some((msg, true));
            }
            Err(err) => guard.toast = Some((err, false)),
        }
    });
}

impl DomadorUi {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = C_BG;
        visuals.window_fill = C_PANEL;
        visuals.faint_bg_color = egui::Color32::from_rgb(31, 23, 18);
        visuals.extreme_bg_color = egui::Color32::from_rgb(11, 8, 6);
        visuals.widgets.noninteractive.bg_fill = C_PANEL;
        visuals.widgets.noninteractive.bg_stroke.color = C_BORDER;
        visuals.widgets.inactive.bg_fill = C_CARD;
        visuals.widgets.inactive.bg_stroke.color = C_BORDER;
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(57, 40, 28);
        visuals.widgets.hovered.bg_stroke.color = C_ACCENT;
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(76, 51, 32);
        visuals.selection.bg_fill = egui::Color32::from_rgb(112, 74, 42);
        visuals.override_text_color = Some(C_TEXT);
        cc.egui_ctx.set_visuals(visuals);

        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(9.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(21.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(12.5));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(11.5));
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
            sort: SortBy::Cpu,
            selected: None,
            kill_confirm: None,
            logo,
        }
    }

    fn state(&self) -> Option<Arc<Value>> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone()
    }

    fn refresh(&self) -> Option<Arc<Value>> {
        match get_state() {
            Ok(value) => {
                let arc = Arc::new(value);
                let mut guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                guard.state = Some(arc.clone());
                guard.error = None;
                Some(arc)
            }
            Err(_) => self.state(),
        }
    }

    fn toast(&self) -> Option<(String, bool)> {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .toast
            .clone()
    }

    fn action(&self, path: &'static str, body: Value, label: &'static str) {
        act_bg(self.shared.clone(), path, body, label);
    }

    fn profile(&self, name: &str) {
        let label: &'static str = match name {
            "full_power" => "Activando Full Power",
            "gaming" => "Activando modo Juego",
            "ai" => "Activando modo IA",
            _ => "Equilibrando",
        };
        self.action("/api/profile", json!({"name": name}), label);
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

        egui::TopBottomPanel::top("top")
            .exact_height(76.0)
            .frame(
                egui::Frame::none()
                    .fill(C_PANEL)
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.image((logo.id(), egui::vec2(42.0, 42.0)));
                    }
                    ui.vertical(|ui| {
                        ui.add_space(1.0);
                        ui.label(
                            egui::RichText::new("DOMADOR")
                                .strong()
                                .size(19.0)
                                .color(C_TEXT),
                        );
                        ui.label(
                            egui::RichText::new("control de procesos")
                                .size(10.5)
                                .color(C_MUTED),
                        );
                    });
                    ui.add_space(10.0);
                    let (bg, fg, text) = if admin {
                        (egui::Color32::from_rgb(24, 50, 35), C_GREEN, "ADMIN")
                    } else {
                        (egui::Color32::from_rgb(52, 37, 22), C_ACCENT, "SIN ADMIN")
                    };
                    egui::Frame::none()
                        .fill(bg)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(9, 4))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(text).size(10.0).strong().color(fg));
                        });
                    if !admin && ui.button("Activar admin").clicked() {
                        self.relaunch_admin();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("v{version}"))
                                .size(10.0)
                                .color(C_MUTED),
                        );
                        ui.add_space(14.0);
                        metric_chip(
                            ui,
                            "RAM",
                            &format!("{ram:.0}%"),
                            if ram > 85.0 { C_CORAL } else { C_ACCENT },
                        );
                        ui.add_space(6.0);
                        metric_chip(
                            ui,
                            "CPU",
                            &format!("{cpu:.0}%"),
                            if cpu > 70.0 { C_CORAL } else { C_GREEN },
                        );
                    });
                });
            });

        self.profile_bar(ctx);
    }

    fn profile_bar(&mut self, ctx: &egui::Context) {
        let active = self
            .state()
            .and_then(|s| {
                s.pointer("/profile/active")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "balanced".into());
        egui::TopBottomPanel::top("profiles")
            .exact_height(52.0)
            .frame(
                egui::Frame::none()
                    .fill(C_BG)
                    .inner_margin(egui::Margin::symmetric(20, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("PERFIL")
                            .size(9.5)
                            .strong()
                            .color(C_MUTED),
                    );
                    ui.add_space(4.0);
                    profile_chip(
                        ui,
                        &active,
                        "full_power",
                        "⚡ Full Power",
                        C_ACCENT,
                        || self.profile("full_power"),
                    );
                    profile_chip(ui, &active, "gaming", "🎮 Juego", C_CORAL, || {
                        self.profile("gaming")
                    });
                    profile_chip(
                        ui,
                        &active,
                        "ai",
                        "🤖 IA",
                        egui::Color32::from_rgb(170, 160, 240),
                        || self.profile("ai"),
                    );
                    profile_chip(ui, &active, "balanced", "⚖ Equilibrado", C_GREEN, || {
                        self.profile("balanced")
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(profile) = self.state().and_then(|s| s.get("profile").cloned())
                        {
                            let plan = profile.get("plan").and_then(|v| v.as_str()).unwrap_or("");
                            if !plan.is_empty() {
                                ui.label(egui::RichText::new(plan).size(10.0).color(C_MUTED));
                            }
                        }
                    });
                });
            });
    }

    fn side(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(186.0)
            .frame(
                egui::Frame::none()
                    .fill(C_PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 16)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);
                nav_button(ui, &mut self.tab, Tab::Processes, "  Procesos");
                nav_button(ui, &mut self.tab, Tab::Health, "  Salud");
                nav_button(ui, &mut self.tab, Tab::Settings, "  Ajustes");
                ui.add_space(22.0);
                ui.separator();
                ui.add_space(12.0);
                if let Some(state) = self.state() {
                    let counts = state.get("counts");
                    let procs = counts
                        .and_then(|c| c.get("processes"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let rules = counts
                        .and_then(|c| c.get("rules"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let throttled = counts
                        .and_then(|c| c.get("throttled"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    ui.label(
                        egui::RichText::new("ESTADO")
                            .size(9.5)
                            .color(C_MUTED)
                            .strong(),
                    );
                    side_kv(ui, "Procesos", &procs.to_string());
                    side_kv(ui, "Reglas", &rules.to_string());
                    side_kv(ui, "Regulados", &throttled.to_string());
                    if let Some(profile) = state.get("profile") {
                        let active = profile
                            .get("active")
                            .and_then(|v| v.as_str())
                            .unwrap_or("balanced");
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new("PERFIL ACTIVO")
                                .size(9.5)
                                .color(C_MUTED)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(active.replace('_', " ").to_uppercase())
                                .size(11.0)
                                .color(C_ACCENT),
                        );
                    }
                    let runaways = state
                        .get("runaways")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if runaways > 0 {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(format!("⚠ {runaways} colgado(s)"))
                                .size(11.0)
                                .color(C_CORAL),
                        );
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new("127.0.0.1:8787")
                            .size(9.0)
                            .color(C_MUTED),
                    );
                });
            });
    }

    fn primary(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(9, 13, 18))
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ctx, |ui| match self.tab {
                Tab::Processes => self.processes(ui),
                Tab::Health => self.health(ui),
                Tab::Settings => self.settings(ui),
            });
    }

    fn processes(&mut self, ui: &mut egui::Ui) {
        let state = self.state();
        let procs = state
            .as_ref()
            .and_then(|s| s.get("processes"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Procesos").size(20.0));
            ui.add_space(12.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("buscar proceso…")
                    .desired_width(240.0),
            );
            ui.label(
                egui::RichText::new(format!("{} activos", procs.len()))
                    .size(10.5)
                    .color(C_MUTED),
            );
            ui.add_space(8.0);
            egui::ComboBox::from_id_salt("sort-by")
                .selected_text(match self.sort {
                    SortBy::Cpu => "CPU",
                    SortBy::Ram => "RAM",
                    SortBy::Name => "Nombre",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort, SortBy::Cpu, "CPU");
                    ui.selectable_value(&mut self.sort, SortBy::Ram, "RAM");
                    ui.selectable_value(&mut self.sort, SortBy::Name, "Nombre");
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Actualizar").clicked() {
                    self.refresh();
                }
                ui.label(egui::RichText::new("ordenar por").size(9.5).color(C_MUTED));
            });
        });
        ui.add_space(8.0);
        let needle = self.search.to_lowercase();
        let mut filtered: Vec<&Value> = procs
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| n.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .collect();
        filtered.sort_by(|a, b| match self.sort {
            SortBy::Cpu => {
                let av = a.get("cpu").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let bv = b.get("cpu").and_then(|v| v.as_f64()).unwrap_or(0.0);
                bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
            }
            SortBy::Ram => {
                let av = a.get("rss").and_then(|v| v.as_u64()).unwrap_or(0);
                let bv = b.get("rss").and_then(|v| v.as_u64()).unwrap_or(0);
                bv.cmp(&av)
            }
            SortBy::Name => a
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase()
                .cmp(
                    &b.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase(),
                ),
        });

        egui::Frame::none()
            .fill(C_PANEL)
            .corner_radius(9.0)
            .stroke(egui::Stroke::new(1.0, C_BORDER))
            .inner_margin(egui::Margin::same(1))
            .show(ui, |ui| {
                let available = ui.available_height() - 46.0;
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .max_scroll_height(available.max(180.0))
                    .column(Column::initial(280.0).at_least(170.0))
                    .column(Column::initial(64.0).at_least(58.0))
                    .column(Column::initial(70.0).at_least(58.0))
                    .column(Column::initial(84.0).at_least(72.0))
                    .column(Column::initial(96.0).at_least(78.0))
                    .column(Column::remainder().at_least(230.0))
                    .header(27.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("Proceso");
                        });
                        header.col(|ui| {
                            ui.strong("PID");
                        });
                        header.col(|ui| {
                            ui.strong("CPU");
                        });
                        header.col(|ui| {
                            ui.strong("RAM");
                        });
                        header.col(|ui| {
                            ui.strong("Prioridad");
                        });
                        header.col(|ui| {
                            ui.strong("Acciones");
                        });
                    })
                    .body(|mut body| {
                        for p in filtered.iter().take(300) {
                            let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let cpu = p.get("cpu").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let rss =
                                p.get("rss").and_then(|v| v.as_u64()).unwrap_or(0) as f64 / MB;
                            let prio = p
                                .get("prio_label")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .unwrap_or("Normal");
                            let selected = self.selected == Some(pid);
                            body.row(26.0, |mut row| {
                                row.set_selected(selected);
                                row.col(|ui| {
                                    let text = egui::RichText::new(name)
                                        .size(12.0)
                                        .color(if selected { C_ACCENT } else { C_TEXT });
                                    if ui
                                        .add(egui::Label::new(text).sense(egui::Sense::click()))
                                        .clicked()
                                    {
                                        self.selected = Some(pid);
                                    }
                                });
                                row.col(|ui| {
                                    num_label(ui, &pid.to_string(), C_MUTED);
                                });
                                row.col(|ui| {
                                    num_label(ui, &format!("{cpu:.1}%"), cpu_color(cpu));
                                });
                                row.col(|ui| {
                                    num_label(ui, &format!("{rss:.0} MB"), C_TEXT);
                                });
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(prio).size(10.5).color(C_MUTED));
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .small_button("Subir")
                                            .on_hover_text("Subir prioridad a Alta")
                                            .clicked()
                                        {
                                            self.action(
                                                "/api/priority",
                                                json!({"pid": pid, "cls": "high"}),
                                                "Subiendo prioridad",
                                            );
                                        }
                                        if ui
                                            .small_button("Liberar")
                                            .on_hover_text("Liberar working set de este proceso")
                                            .clicked()
                                        {
                                            self.action(
                                                "/api/trim",
                                                json!({"pid": pid}),
                                                "Liberando RAM",
                                            );
                                        }
                                        if ui
                                            .small_button("↺")
                                            .on_hover_text("Restaurar prioridad original")
                                            .clicked()
                                        {
                                            self.action(
                                                "/api/restore",
                                                json!({"pid": pid}),
                                                "Restaurando",
                                            );
                                        }
                                        if ui
                                            .small_button("Fin")
                                            .on_hover_text("Terminar proceso")
                                            .clicked()
                                        {
                                            self.kill_confirm = Some((pid, name.to_string()));
                                        }
                                    });
                                });
                            });
                        }
                    });
            });
        if let Some(state) = state {
            if let Some(runaways) = state.get("runaways").and_then(|v| v.as_array()) {
                if !runaways.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("Procesos colgados detectados")
                            .strong()
                            .color(egui::Color32::from_rgb(240, 170, 100)),
                    );
                    for r in runaways {
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let pid = r.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                        let minutes = r.get("minutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        ui.label(format!("{name}  PID {pid}  {minutes:.0} min"));
                    }
                }
            }
        }
    }

    fn health(&mut self, ui: &mut egui::Ui) {
        let state = self.state();
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Salud").size(20.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("🧹 Aliviar memoria de fondo").clicked() {
                    self.action("/api/trim-idle", json!({}), "Liberando memoria de fondo");
                }
            });
        });
        ui.label(
            egui::RichText::new("Memoria real, GPU y procesos que no ves en el Task Manager.")
                .size(10.5)
                .color(C_MUTED),
        );
        ui.add_space(8.0);
        let Some(state) = state else {
            ui.label("Esperando al motor…");
            return;
        };
        let mem = state.pointer("/system/mem");
        let gb = |v: Option<&Value>| {
            v.and_then(|x| x.as_f64())
                .map(|b| format!("{:.1} GB", b / GB))
                .unwrap_or_else(|| "—".into())
        };
        let num = |v: Option<&Value>| v.and_then(|x| x.as_u64()).unwrap_or(0);

        ui.columns(2, |cols| {
            health_card(&mut cols[0], "MEMORIA", |ui| {
                let used = num(mem.and_then(|m| m.get("commit_used")));
                let limit = num(mem.and_then(|m| m.get("commit_limit"))).max(1);
                let pct = (used as f64 / limit as f64 * 100.0).min(100.0);
                health_row(
                    ui,
                    "Disponible",
                    &gb(mem.and_then(|m| m.get("available"))),
                    C_GREEN,
                );
                health_row(
                    ui,
                    "Commit",
                    &format!("{:.1} / {:.1} GB", used as f64 / GB, limit as f64 / GB),
                    if pct > 85.0 { C_CORAL } else { C_TEXT },
                );
                ui.add(
                    egui::ProgressBar::new((pct / 100.0) as f32)
                        .desired_height(5.0)
                        .fill(if pct > 85.0 { C_CORAL } else { C_ACCENT }),
                );
                health_row(
                    ui,
                    "Procesos",
                    &num(mem.and_then(|m| m.get("processes"))).to_string(),
                    C_TEXT,
                );
            });
            health_card(&mut cols[1], "KERNEL Y CACHE", |ui| {
                health_row(
                    ui,
                    "Pool paginado",
                    &gb(mem.and_then(|m| m.get("pool_paged"))),
                    C_TEXT,
                );
                health_row(
                    ui,
                    "Pool no paginado",
                    &gb(mem.and_then(|m| m.get("pool_nonpaged"))),
                    C_TEXT,
                );
                health_row(ui, "Cache", &gb(mem.and_then(|m| m.get("cache"))), C_ACCENT);
                health_row(
                    ui,
                    "Handles",
                    &num(mem.and_then(|m| m.get("handles"))).to_string(),
                    C_MUTED,
                );
            });
        });
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Sólo toca procesos sin ventana visible, con CPU baja y working set grande. No mata nada.").size(9.5).color(C_MUTED));

        if let Some(gpu) = state.get("gpu") {
            let available = gpu
                .get("available")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if available {
                ui.add_space(10.0);
                let name = gpu.get("name").and_then(|v| v.as_str()).unwrap_or("GPU");
                let util = gpu
                    .get("utilization")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let used = gpu.get("memory_used").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = gpu
                    .get("memory_total")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let temp = gpu
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                health_card(ui, &format!("GPU · {name}"), |ui| {
                    ui.horizontal(|ui| {
                        health_row(ui, "Uso", &format!("{util:.0}%"), C_ACCENT);
                        ui.add_space(18.0);
                        health_row(
                            ui,
                            "VRAM",
                            &format!("{:.1}/{:.1} GB", used as f64 / GB, total as f64 / GB),
                            C_TEXT,
                        );
                        ui.add_space(18.0);
                        health_row(
                            ui,
                            "Temp",
                            &format!("{temp:.0} °C"),
                            if temp > 80.0 { C_CORAL } else { C_GREEN },
                        );
                    });
                });
            }
        }

        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("Top por memoria privada")
                .strong()
                .size(12.0),
        );
        ui.label(
            egui::RichText::new(
                "Es la memoria que el working set no muestra; los WebView suelen esconderse acá.",
            )
            .size(9.5)
            .color(C_MUTED),
        );
        ui.add_space(4.0);
        let mut rows: Vec<&Value> = state
            .get("processes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        rows.sort_by(|a, b| {
            let av = a.get("private").and_then(|v| v.as_u64()).unwrap_or(0);
            let bv = b.get("private").and_then(|v| v.as_u64()).unwrap_or(0);
            bv.cmp(&av)
        });
        for row in rows.iter().take(10) {
            let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let private = row.get("private").and_then(|v| v.as_u64()).unwrap_or(0);
            let rss = row.get("rss").and_then(|v| v.as_u64()).unwrap_or(0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{name:.30}")).size(11.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.2} GB priv · {:.2} GB rss",
                            private as f64 / GB,
                            rss as f64 / GB
                        ))
                        .size(10.0)
                        .monospace()
                        .color(C_MUTED),
                    );
                });
            });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("Ajustes").size(20.0));
        ui.add_space(8.0);
        let state = self.state();
        let enabled = state
            .as_ref()
            .and_then(|s| s.pointer("/probalance/enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut on = enabled;
        if ui.checkbox(&mut on, "ProBalanza").changed() {
            self.action(
                "/api/probalance",
                json!({"enabled": on}),
                "Guardando ProBalanza",
            );
        }
        ui.label(
            egui::RichText::new(
                "Baja la prioridad de procesos que se comen la CPU y la restaura cuando se calman.",
            )
            .size(10.0)
            .color(egui::Color32::from_rgb(110, 130, 145)),
        );
        ui.add_space(14.0);
        let admin = state
            .as_ref()
            .and_then(|s| s.pointer("/app/admin"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ui.label(
            egui::RichText::new(if admin {
                "Corriendo con privilegios de administrador"
            } else {
                "Sin administrador: algunos procesos del sistema rechazan cambios"
            })
            .size(11.0),
        );
        if !admin && ui.button("Reiniciar como administrador").clicked() {
            self.relaunch_admin();
        }
        ui.add_space(18.0);
        ui.label(egui::RichText::new("Motor").strong());
        if let Some(path) = state
            .as_ref()
            .and_then(|s| s.pointer("/app/config_path"))
            .and_then(|v| v.as_str())
        {
            ui.label(
                egui::RichText::new(path)
                    .monospace()
                    .size(9.5)
                    .color(egui::Color32::from_rgb(105, 125, 140)),
            );
        }
    }

    fn kill_dialog(&mut self, ctx: &egui::Context) {
        let Some((pid, name)) = self.kill_confirm.clone() else {
            return;
        };
        let mut close = false;
        egui::Window::new("Terminar proceso")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("¿Terminar {name} (PID {pid})?"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancelar").clicked() {
                        close = true;
                    }
                    if ui.button("Terminar").clicked() {
                        self.action("/api/kill", json!({"pid": pid}), "Terminando proceso");
                        close = true;
                    }
                });
            });
        if close {
            self.kill_confirm = None;
        }
    }

    fn toast_bar(&self, ctx: &egui::Context) {
        let (toast, busy, error) = {
            let guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            (guard.toast.clone(), guard.busy.clone(), guard.error.clone())
        };
        if let Some((msg, ok)) = toast {
            egui::TopBottomPanel::bottom("toast")
                .exact_height(30.0)
                .frame(
                    egui::Frame::none()
                        .fill(if ok {
                            egui::Color32::from_rgb(13, 36, 25)
                        } else {
                            egui::Color32::from_rgb(48, 20, 20)
                        })
                        .inner_margin(egui::Margin::symmetric(14, 6)),
                )
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(msg).size(10.5).color(if ok {
                        egui::Color32::from_rgb(170, 240, 200)
                    } else {
                        egui::Color32::from_rgb(255, 175, 175)
                    }));
                });
        } else if let Some(busy) = busy {
            egui::TopBottomPanel::bottom("busy")
                .exact_height(28.0)
                .show(ctx, |ui| ui.label(egui::RichText::new(busy).size(10.0)));
        } else if let Some(error) = error {
            egui::TopBottomPanel::bottom("error")
                .exact_height(28.0)
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new(error)
                            .size(10.0)
                            .color(egui::Color32::from_rgb(240, 160, 130)),
                    )
                });
        }
    }
}

impl eframe::App for DomadorUi {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(650));
        self.top(ctx);
        self.side(ctx);
        self.primary(ctx);
        self.toast_bar(ctx);
        self.kill_dialog(ctx);
        // Refrescar el latido del motor de vez en cuando aunque no haya UI web.
        if self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .is_none()
        {
            self.refresh();
        }
    }
}

fn nav_button(ui: &mut egui::Ui, tab: &mut Tab, value: Tab, text: &str) {
    let selected = *tab == value;
    let button = egui::Button::new(egui::RichText::new(text).size(13.0).color(if selected {
        C_ACCENT
    } else {
        C_MUTED
    }))
    .fill(if selected {
        egui::Color32::from_rgb(48, 35, 24)
    } else {
        egui::Color32::TRANSPARENT
    })
    .corner_radius(7.0);
    if ui.add_sized([158.0, 34.0], button).clicked() {
        *tab = value;
    }
}

fn metric_chip(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    egui::Frame::none()
        .fill(C_CARD)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(label).size(9.5).color(C_MUTED));
                ui.label(egui::RichText::new(value).size(12.0).strong().color(color));
            });
        });
}

fn profile_chip(
    ui: &mut egui::Ui,
    active: &str,
    key: &str,
    text: &str,
    color: egui::Color32,
    on_click: impl FnOnce(),
) {
    let selected = active == key;
    let button = egui::Button::new(egui::RichText::new(text).size(12.0).strong().color(
        if selected {
            egui::Color32::from_rgb(35, 23, 14)
        } else {
            color
        },
    ))
    .fill(if selected { color } else { C_CARD })
    .corner_radius(7.0);
    if ui.add(button).clicked() {
        on_click();
    }
}

fn health_card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(C_CARD)
        .corner_radius(10.0)
        .stroke(egui::Stroke::new(1.0, C_BORDER))
        .inner_margin(egui::Margin::symmetric(14, 11))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).size(9.5).strong().color(C_MUTED));
            ui.add_space(5.0);
            body(ui);
        });
}

fn health_row(ui: &mut egui::Ui, key: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(10.5).color(C_MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).size(12.0).strong().color(color));
        });
    });
}

fn side_kv(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(10.5).color(C_MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(value).size(11.5).strong().color(C_TEXT));
        });
    });
}

fn num_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(
            egui::RichText::new(text)
                .size(11.0)
                .monospace()
                .color(color),
        );
    });
}

fn cpu_color(cpu: f64) -> egui::Color32 {
    if cpu >= 25.0 {
        C_CORAL
    } else if cpu >= 5.0 {
        C_ACCENT
    } else {
        C_GREEN
    }
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
        .with_min_inner_size([1080.0, 700.0]);
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
