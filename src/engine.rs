use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;

use crate::config;
use crate::model::*;
use crate::power;
use crate::sys::{mem_info, Sampler};
use crate::win;

type Key = (u32, i64);
type KeySet = HashSet<Key>;

const MB: u64 = 1024 * 1024;

// ------------------------------------------------------------------ MemGuard

/// Umbrales de presión de memoria. Sólo avisa y, opcionalmente, alivia
/// working sets de fondo: nunca termina procesos.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct MemGuard {
    pub enabled: bool,
    /// Porcentaje de commit usado que dispara el aviso.
    pub warn_commit_pct: f64,
    /// Porcentaje de RAM física disponible por debajo del cual avisa.
    pub warn_available_pct: f64,
    /// Alivio automático (working sets de fondo) cuando hay presión.
    pub relief: bool,
    /// Mínimo entre alivios automáticos.
    pub relief_interval_s: u32,
    /// Mínimo entre avisos repetidos del mismo nivel.
    pub warn_cooldown_s: u32,
}

impl Default for MemGuard {
    fn default() -> Self {
        Self {
            enabled: true,
            warn_commit_pct: 88.0,
            warn_available_pct: 7.0,
            relief: false,
            relief_interval_s: 900,
            warn_cooldown_s: 900,
        }
    }
}

/// Foto de la presión de memoria del último tick. `level` es "ok", "warn" o
/// "critical"; el resto son los números que lo justifican.
#[derive(Serialize, Clone, Debug)]
pub struct MemPressure {
    pub level: String,
    pub reasons: Vec<String>,
    pub commit_pct: f64,
    pub available_pct: f64,
    pub available: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub commit_free: u64,
    pub cache: u64,
    pub pool_paged: u64,
    pub pool_nonpaged: u64,
    pub handles: u32,
    pub processes: u32,
    pub warned: bool,
    pub warned_at: f64,
    pub reliefs: u32,
    pub last_relief_at: f64,
    pub last_relief_freed: u64,
}

impl Default for MemPressure {
    fn default() -> Self {
        Self {
            level: "ok".into(),
            reasons: Vec::new(),
            commit_pct: 0.0,
            available_pct: 0.0,
            available: 0,
            commit_used: 0,
            commit_limit: 0,
            commit_free: 0,
            cache: 0,
            pool_paged: 0,
            pool_nonpaged: 0,
            handles: 0,
            processes: 0,
            warned: false,
            warned_at: 0.0,
            reliefs: 0,
            last_relief_at: 0.0,
            last_relief_freed: 0,
        }
    }
}

/// Agregado de procesos de una misma familia (Chrome/WebView/Electron).
#[derive(Serialize, Clone, Debug, Default)]
pub struct FamilyInfo {
    pub family: String,
    pub label: String,
    pub count: usize,
    pub cpu: f64,
    pub rss: u64,
    pub private: u64,
    pub protected: usize,
    pub applied: usize,
    pub throttled: usize,
    pub pids: Vec<u32>,
    pub pids_truncated: bool,
}

/// Familia de un proceso según su nombre de imagen. Es una lista explícita:
/// no se adivina por ruta ni por árbol de procesos.
pub fn family_of(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "chrome.exe" | "msedge.exe" | "brave.exe" | "opera.exe" | "vivaldi.exe"
        | "chromium.exe" | "thorium.exe" | "yandex.exe" => "chromium",
        "msedgewebview2.exe" | "webviewhost.exe" => "webview",
        "code.exe"
        | "code - insiders.exe"
        | "cursor.exe"
        | "windsurf.exe"
        | "discord.exe"
        | "slack.exe"
        | "teams.exe"
        | "ms-teams.exe"
        | "spotify.exe"
        | "notion.exe"
        | "obsidian.exe"
        | "postman.exe"
        | "figma.exe"
        | "whatsapp.exe"
        | "signal.exe"
        | "element.exe"
        | "bitwarden.exe"
        | "1password.exe"
        | "linear.exe"
        | "loom.exe"
        | "github desktop.exe"
        | "gitkraken.exe"
        | "insomnia.exe" => "electron",
        _ => return None,
    })
}

pub fn family_label(family: &str) -> Option<&'static str> {
    Some(match family {
        "chromium" => "Chrome/Edge",
        "webview" => "WebView2",
        "electron" => "Electron",
        _ => return None,
    })
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn fmt_mb(bytes: u64) -> String {
    if bytes >= 1024 * MB {
        format!("{:.1} GB", bytes as f64 / (1024.0 * MB as f64))
    } else {
        format!("{} MB", bytes / MB)
    }
}

fn wildcard(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == text;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut match_i) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_i = ti;
            pi += 1;
        } else if let Some(star_pos) = star {
            pi = star_pos + 1;
            match_i += 1;
            ti = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Clone)]
struct Meta {
    ts: Instant,
    nice: u32,
    affinity: Vec<u32>,
    eco: bool,
    exe: String,
}

struct Original {
    name: String,
    nice: Option<u32>,
    affinity: Option<Vec<u32>>,
    eco: Option<bool>,
}

struct PbRecord {
    pid: u32,
    name: String,
    orig_nice: u32,
    orig_eco: Option<bool>,
    eco_done: bool,
}

struct FgBoost {
    pid: u32,
    key: Key,
    orig_nice: u32,
    target: u32,
    name: String,
}

pub struct Engine {
    pub config: Config,
    pub config_path: PathBuf,
    pub rows: Vec<ProcessRow>,
    pub system: SystemInfo,
    pub history: VecDeque<HistoryPoint>,
    pub log_lines: VecDeque<LogEntry>,
    sampler: Sampler,
    meta: HashMap<Key, Meta>,
    exe_cache: HashMap<Key, String>,
    applied: HashMap<Key, Original>,
    pb: HashMap<Key, PbRecord>,
    pb_cooldown: HashMap<Key, Instant>,
    samples: HashMap<Key, VecDeque<f64>>,
    rules_done: KeySet,
    visible: HashSet<u32>,
    runaway_since: HashMap<Key, Instant>,
    runaway_warned: HashSet<Key>,
    runaways: Vec<RunawayInfo>,
    pub plans: Vec<PowerPlan>,
    pub active_plan: Option<String>,
    pub profile: ProfileState,
    profile_applied: Vec<u32>,
    gpu: GpuInfo,
    gpu_at: Instant,
    started: Instant,
    tick_ms: VecDeque<f64>,
    last_tick_ms: f64,
    pub ncpu: usize,
    pub admin: bool,
    eco_supported: bool,
    last_plan_scan: Instant,
    last_power_check: Instant,
    last_trim: Instant,
    fg_boost: Option<FgBoost>,
    wd_last_launch: HashMap<String, Instant>,
    wd_missing: HashMap<String, Instant>,
    wd_warned: HashSet<String>,
    pub memguard: MemGuard,
    pub mem_pressure: MemPressure,
    last_relief_attempt: Instant,
    pub dirty: bool,
}

impl Engine {
    pub fn new(config_path: PathBuf) -> Self {
        let config = config::load(&config_path);
        let memguard = load_memguard(&config_path);
        let sampler = Sampler::new();
        let ncpu = sampler.ncpu();
        let admin = win::is_admin();
        let mut engine = Self {
            config,
            config_path,
            rows: Vec::new(),
            system: SystemInfo::default(),
            history: VecDeque::with_capacity(150),
            log_lines: VecDeque::with_capacity(400),
            sampler,
            meta: HashMap::new(),
            exe_cache: HashMap::new(),
            applied: HashMap::new(),
            pb: HashMap::new(),
            pb_cooldown: HashMap::new(),
            samples: HashMap::new(),
            rules_done: HashSet::new(),
            visible: HashSet::new(),
            runaway_since: HashMap::new(),
            runaway_warned: HashSet::new(),
            runaways: Vec::new(),
            plans: Vec::new(),
            active_plan: None,
            profile: ProfileState::default(),
            profile_applied: Vec::new(),
            gpu: GpuInfo::default(),
            gpu_at: Instant::now() - Duration::from_secs(60),
            started: Instant::now(),
            tick_ms: VecDeque::with_capacity(180),
            last_tick_ms: 0.0,
            ncpu,
            admin,
            eco_supported: true,
            last_plan_scan: Instant::now() - Duration::from_secs(3600),
            last_power_check: Instant::now() - Duration::from_secs(60),
            last_trim: Instant::now() - Duration::from_secs(3600),
            fg_boost: None,
            wd_last_launch: HashMap::new(),
            wd_missing: HashMap::new(),
            wd_warned: HashSet::new(),
            memguard,
            mem_pressure: MemPressure::default(),
            last_relief_attempt: Instant::now() - Duration::from_secs(3600),
            dirty: false,
        };
        engine.profile = engine.config.profile.clone();
        engine.refresh_plans();
        engine.log(
            "info",
            format!(
                "{} {} · {} núcleos · {}",
                APP_NAME,
                VERSION,
                engine.ncpu,
                if admin {
                    "con admin"
                } else {
                    "sin admin (algunos procesos rechazan cambios)"
                }
            ),
        );
        if engine.memguard.enabled {
            engine.log(
                "info",
                format!(
                    "MemGuard: aviso commit ≥ {:.0}% o disponible ≤ {:.0}% · alivio {}",
                    engine.memguard.warn_commit_pct,
                    engine.memguard.warn_available_pct,
                    if engine.memguard.relief {
                        "automático"
                    } else {
                        "manual"
                    }
                ),
            );
        }
        engine
    }

    pub fn save(&mut self) {
        self.config.profile = self.profile.clone();
        if let Err(err) = save_config(&self.config_path, &self.config, &self.memguard) {
            self.log("warn", format!("no pude guardar config: {err}"));
        }
        self.dirty = false;
    }

    pub fn log(&mut self, level: &str, msg: impl Into<String>) {
        if self.log_lines.len() >= 400 {
            self.log_lines.pop_front();
        }
        self.log_lines.push_back(LogEntry {
            ts: now_epoch(),
            level: level.to_string(),
            msg: msg.into(),
        });
    }

    // ---------------------------------------------------------------- tick

    pub fn tick(&mut self) {
        let started = Instant::now();
        let mut rows = self.sampler.refresh();

        // Una sola copia de reglas y de la lista de protegidos por tick: antes
        // se clonaba la lista completa de reglas por cada proceso.
        let rules = self.config.rules.clone();
        let protect = self.config.probalance.protect.clone();
        for row in rows.iter_mut() {
            let rule = match_rule_fast(&rules, row, &mut self.exe_cache);
            row.rule_id = rule.as_ref().map(|r| r.id.clone());
            row.rule = rule;
            row.protected = protect.iter().any(|p| p.eq_ignore_ascii_case(&row.name));
        }

        let foreground = win::foreground_pid();
        self.enrich(&mut rows, foreground);

        let (cpu, percpu, used, total, ram) = self.sampler.system();
        let foreground_name =
            foreground.and_then(|pid| rows.iter().find(|r| r.pid == pid).map(|r| r.name.clone()));

        self.rows = rows;
        self.system = SystemInfo {
            cpu,
            percpu,
            ram: ram,
            ram_used: used,
            ram_total: total,
            mem: mem_info(),
            foreground,
            foreground_name,
            last_tick: now_epoch(),
        };
        if self.history.len() >= 150 {
            self.history.pop_front();
        }
        self.history.push_back(HistoryPoint {
            t: now_epoch(),
            cpu,
            ram,
        });

        // ProBalanza necesita la lista de ventanas visibles; el watchdog de
        // colgados también. Se calcula una sola vez por tick (antes se podían
        // hacer dos EnumWindows idénticos).
        let pb = &self.config.probalance;
        let need_visible = (pb.enabled && pb.skip_interactive) || self.config.runaway.enabled;
        if need_visible {
            self.visible = win::visible_window_pids();
        } else {
            self.visible.clear();
        }

        self.prune();
        self.apply_new_rules();
        self.pb_step();
        self.fg_step();
        self.watchdog_step();
        self.runaway_step();
        self.trim_step();
        self.memguard_step();
        self.power_step();

        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        self.last_tick_ms = elapsed;
        if self.tick_ms.len() >= 180 {
            self.tick_ms.pop_front();
        }
        self.tick_ms.push_back(elapsed);
    }

    fn prune(&mut self) {
        if self.rows.len() < 10 {
            return;
        }
        let alive: KeySet = self.rows.iter().map(|r| r.key()).collect();
        self.applied.retain(|k, _| alive.contains(k));
        self.pb.retain(|k, _| alive.contains(k));
        self.rules_done.retain(|k| alive.contains(k));
        if self.meta.len() > 900 {
            self.meta.retain(|k, _| alive.contains(k));
        }
        if self.samples.len() > 900 {
            self.samples.retain(|k, _| alive.contains(k));
        }
        if self.pb_cooldown.len() > 900 {
            self.pb_cooldown.retain(|k, _| alive.contains(k));
        }
        if self.exe_cache.len() > 1500 {
            self.exe_cache.retain(|k, _| alive.contains(k));
        }
    }

    fn enrich(&mut self, rows: &mut [ProcessRow], foreground: Option<u32>) {
        if rows.is_empty() {
            return;
        }
        let mut special: KeySet = self.applied.keys().copied().collect();
        special.extend(self.pb.keys().copied());
        let top_count = rows.len().min(50);
        let top_keys: KeySet = rows[..top_count].iter().map(|r| r.key()).collect();
        let ai_patterns = self.config.ai.patterns.clone();
        let ai_pids: HashSet<u32> = rows
            .iter()
            .filter(|r| ai_patterns.iter().any(|p| wildcard(p, &r.name)))
            .map(|r| r.pid)
            .collect();
        let own = std::process::id();
        let special_pids: HashSet<u32> = rows
            .iter()
            .filter(|r| {
                special.contains(&r.key())
                    || Some(r.pid) == foreground
                    || r.pid == own
                    || ai_pids.contains(&r.pid)
            })
            .map(|r| r.pid)
            .collect();

        let budget = Instant::now() + Duration::from_millis(90);
        let mut done: HashSet<u32> = HashSet::new();
        for row in rows.iter_mut() {
            let top = top_keys.contains(&row.key());
            if !top && !special_pids.contains(&row.pid) {
                continue;
            }
            if !done.insert(row.pid) {
                continue;
            }
            let key = row.key();
            let cached = self.meta.get(&key).cloned();
            let ttl = if top { 6.0 } else { 20.0 };
            if let Some(meta) = &cached {
                if meta.ts.elapsed().as_secs_f64() <= ttl {
                    apply_meta(row, meta, self.ncpu);
                    continue;
                }
            }
            if Instant::now() > budget {
                if let Some(meta) = &cached {
                    apply_meta(row, meta, self.ncpu);
                }
                continue;
            }
            let Some(nice) = win::get_priority_class(row.pid) else {
                if let Some(meta) = &cached {
                    apply_meta(row, meta, self.ncpu);
                }
                continue;
            };
            let affinity = win::get_affinity(row.pid).unwrap_or_default();
            let eco = win::get_eco(row.pid).unwrap_or(false);
            let exe = if row.exe.is_empty() {
                win::process_exe(row.pid).unwrap_or_default()
            } else {
                row.exe.clone()
            };
            let meta = Meta {
                ts: Instant::now(),
                nice,
                affinity,
                eco,
                exe,
            };
            self.meta.insert(key, meta.clone());
            if !meta.exe.is_empty() {
                self.exe_cache.insert(key, meta.exe.clone());
            }
            apply_meta(row, &meta, self.ncpu);
        }
    }

    /// Nombre y fecha de creación del último tick. La ficha de procesos la usa
    /// como respaldo cuando el proceso no se deja abrir (SYSTEM, PPL) y para
    /// fechar la instancia sin abrir un handle.
    pub fn process_seed(&self, pid: u32) -> Option<(String, f64)> {
        self.rows
            .iter()
            .find(|row| row.pid == pid)
            .map(|row| (row.name.clone(), row.ct))
    }

    pub fn process_meta(&mut self, pid: u32) -> Result<MetaRow, String> {
        let nice = win::get_priority_class(pid).ok_or("no pude leer la prioridad")?;
        let affinity = win::get_affinity(pid).unwrap_or_default();
        let eco = win::get_eco(pid).unwrap_or(false);
        let exe = win::process_exe(pid).unwrap_or_default();
        let row = self.rows.iter().find(|r| r.pid == pid);
        let ct = row.map(|r| r.ct).unwrap_or(0.0);
        if let Some(r) = row {
            self.meta.insert(
                r.key(),
                Meta {
                    ts: Instant::now(),
                    nice,
                    affinity: affinity.clone(),
                    eco,
                    exe: exe.clone(),
                },
            );
        }
        Ok(MetaRow {
            pid,
            ct,
            nice,
            prio: win::priority_key(nice),
            prio_label: win::priority_label(nice),
            affinity: affinity.clone(),
            affinity_label: fmt_cpus(&affinity, self.ncpu),
            eco,
            exe,
        })
    }

    // ---------------------------------------------------------------- reglas

    fn apply_new_rules(&mut self) {
        let actions: Vec<(Key, u32, String, Rule)> = self
            .rows
            .iter()
            .filter_map(|row| {
                row.rule
                    .as_ref()
                    .map(|rule| (row.key(), row.pid, row.name.clone(), rule.clone()))
            })
            .collect();
        for (key, pid, name, rule) in actions {
            if !self.rules_done.insert(key) {
                continue;
            }
            match self.apply_rule(pid, &rule) {
                Ok(()) => self.log(
                    "rule",
                    format!("regla «{}» aplicada a {} (PID {})", rule.pattern, name, pid),
                ),
                Err(err) => {
                    self.rules_done.remove(&key);
                    self.log(
                        "warn",
                        format!(
                            "la regla «{}» falló en {} (PID {}): {}",
                            rule.pattern, name, pid, err
                        ),
                    )
                }
            }
        }
    }

    fn apply_rule(&mut self, pid: u32, rule: &Rule) -> Result<(), String> {
        if let Some(priority) = &rule.priority {
            self.set_priority(pid, priority)?;
        }
        if let Some(affinity) = &rule.affinity {
            self.set_affinity(pid, affinity)?;
        }
        if let Some(eco) = rule.eco {
            self.set_eco(pid, eco)?;
        }
        if let Some(io) = &rule.io {
            self.set_io(pid, io)?;
        }
        if let Some(mem) = &rule.mem {
            self.set_memory_priority(pid, mem)?;
        }
        Ok(())
    }

    pub fn apply_rule_now(&mut self, id: &str) -> Result<u32, String> {
        let rule = self
            .config
            .rules
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or("Regla no encontrada")?;
        let path_rule = rule.pattern.contains('\\') || rule.pattern.contains('/');
        let mut targets: Vec<(u32, String)> = Vec::new();
        for index in 0..self.rows.len() {
            let row = self.rows[index].clone();
            let mut matched = rule_matches_plain(&rule, &row);
            if !matched && path_rule && row.exe.is_empty() {
                if let Some(found) = win::process_exe(row.pid) {
                    self.exe_cache.insert(row.key(), found.clone());
                    let mut probe = row.clone();
                    probe.exe = found;
                    matched = rule_matches_plain(&rule, &probe);
                }
            }
            if matched {
                targets.push((row.pid, row.name));
            }
        }
        let mut count = 0;
        for (pid, name) in targets {
            match self.apply_rule(pid, &rule) {
                Ok(()) => count += 1,
                Err(err) => self.log("warn", format!("{name} (PID {pid}): {err}")),
            }
        }
        self.log(
            "rule",
            format!("regla «{}» aplicada a {count} proceso(s)", rule.pattern),
        );
        Ok(count)
    }

    // ---------------------------------------------------------------- cambios

    fn key_for_pid(&self, pid: u32) -> Key {
        self.rows
            .iter()
            .find(|r| r.pid == pid)
            .map(|r| r.key())
            .unwrap_or((pid, 0))
    }

    fn name_for(&self, pid: u32) -> String {
        self.rows
            .iter()
            .find(|r| r.pid == pid)
            .map(|r| r.name.clone())
            .or_else(|| {
                win::process_exe(pid).map(|p| {
                    Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or(p)
                })
            })
            .unwrap_or_else(|| format!("PID {pid}"))
    }

    fn remember(&mut self, pid: u32) -> Key {
        let key = self.key_for_pid(pid);
        if !self.applied.contains_key(&key) {
            self.applied.insert(
                key,
                Original {
                    name: self.name_for(pid),
                    nice: win::get_priority_class(pid),
                    affinity: win::get_affinity(pid),
                    eco: win::get_eco(pid),
                },
            );
        }
        key
    }

    pub fn set_priority(&mut self, pid: u32, cls: &str) -> Result<(), String> {
        let class =
            win::priority_class_from_key(cls).ok_or_else(|| "Prioridad inválida".to_string())?;
        self.remember(pid);
        win::set_priority_class(pid, class)?;
        let key = self.key_for_pid(pid);
        self.meta.remove(&key);
        self.pb.remove(&key);
        self.log(
            "info",
            format!(
                "{} (PID {}) → prioridad {}",
                self.name_for(pid),
                pid,
                win::priority_label(class)
            ),
        );
        Ok(())
    }

    pub fn set_affinity(&mut self, pid: u32, cpus: &[u32]) -> Result<(), String> {
        if cpus.is_empty() {
            return Err("Elegí al menos un núcleo".into());
        }
        if cpus.iter().any(|c| *c as usize >= self.ncpu) {
            return Err(format!("Núcleos válidos: 0 a {}", self.ncpu - 1));
        }
        self.remember(pid);
        win::set_affinity(pid, cpus)?;
        self.meta.remove(&self.key_for_pid(pid));
        self.log(
            "info",
            format!(
                "{} (PID {}) → afinidad {}",
                self.name_for(pid),
                pid,
                fmt_cpus(cpus, self.ncpu)
            ),
        );
        Ok(())
    }

    pub fn set_eco(&mut self, pid: u32, enabled: bool) -> Result<(), String> {
        self.remember(pid);
        if let Err(err) = win::set_eco(pid, enabled) {
            if err.contains("87") {
                self.eco_supported = false;
            }
            return Err(err);
        }
        self.meta.remove(&self.key_for_pid(pid));
        self.log(
            "info",
            format!(
                "{} (PID {}) → eficiencia {}",
                self.name_for(pid),
                pid,
                if enabled { "sí" } else { "no" }
            ),
        );
        Ok(())
    }

    pub fn set_io(&mut self, pid: u32, level: &str) -> Result<(), String> {
        let value = match level {
            "very_low" => 0,
            "low" => 1,
            "normal" => 2,
            "high" => 3,
            _ => return Err("Prioridad de E/S inválida".into()),
        };
        self.remember(pid);
        win::set_io_priority(pid, value)?;
        self.meta.remove(&self.key_for_pid(pid));
        self.log(
            "info",
            format!("{} (PID {}) → E/S {}", self.name_for(pid), pid, level),
        );
        Ok(())
    }

    pub fn set_memory_priority(&mut self, pid: u32, level: &str) -> Result<(), String> {
        let value = match level {
            "very_low" => 1,
            "low" => 2,
            "medium" => 3,
            "below_normal" => 4,
            "normal" => 5,
            _ => return Err("Prioridad de memoria inválida".into()),
        };
        self.remember(pid);
        win::set_memory_priority(pid, value)?;
        self.meta.remove(&self.key_for_pid(pid));
        self.log(
            "info",
            format!("{} (PID {}) → memoria {}", self.name_for(pid), pid, level),
        );
        Ok(())
    }

    pub fn trim(&mut self, pid: u32) -> Result<u64, String> {
        // Medición directa por handle. Antes se refrescaba el sampler, lo que
        // costaba un snapshot NT completo y ensuciaba el delta de CPU del
        // tick siguiente (los % se disparaban después de cada trim).
        let before = win::process_memory(pid).map(|(rss, _)| rss).unwrap_or(0);
        win::trim_working_set(pid)?;
        let after = win::process_memory(pid).map(|(rss, _)| rss).unwrap_or(0);
        let freed = before.saturating_sub(after);
        self.log(
            "info",
            format!(
                "RAM liberada en {} (PID {}): {}",
                self.name_for(pid),
                pid,
                fmt_mb(freed)
            ),
        );
        Ok(freed)
    }

    /// Recorta una lista de PIDs midiendo antes/después con
    /// `GetProcessMemoryInfo`, sin tocar el sampler ni el historial de CPU.
    fn trim_targets(&mut self, targets: &[(u32, u64)]) -> (u64, u32) {
        let mut trimmed: Vec<(u32, u64)> = Vec::with_capacity(targets.len());
        for (pid, fallback) in targets {
            let before = win::process_memory(*pid)
                .map(|(rss, _)| rss)
                .unwrap_or(*fallback);
            if win::trim_working_set(*pid).is_ok() {
                trimmed.push((*pid, before));
            }
        }
        let mut freed = 0u64;
        let mut count = 0u32;
        for (pid, before) in trimmed {
            let after = win::process_memory(pid)
                .map(|(rss, _)| rss)
                .unwrap_or(before);
            let delta = before.saturating_sub(after);
            if delta > 0 {
                freed += delta;
                count += 1;
            }
        }
        (freed, count)
    }

    pub fn trim_all(&mut self) -> (u64, u32) {
        let min = (self.config.trim.min_mb as u64) * MB;
        let own = std::process::id();
        let targets: Vec<(u32, u64)> = self
            .rows
            .iter()
            .filter(|r| r.rss >= min && r.pid != own && !r.protected)
            .filter(|r| Some(r.pid) != self.system.foreground)
            .map(|r| (r.pid, r.rss))
            .collect();
        let (freed, count) = self.trim_targets(&targets);
        self.last_trim = Instant::now();
        self.log(
            "info",
            format!(
                "SmartTrim: {} liberados en {count} proceso(s)",
                fmt_mb(freed)
            ),
        );
        (freed, count)
    }

    pub fn trim_background(&mut self) -> (u64, u32) {
        let fg = self.system.foreground;
        let visible = if self.visible.is_empty() {
            win::visible_window_pids()
        } else {
            self.visible.clone()
        };
        let own = std::process::id();
        let min = 192 * MB;
        let mut targets: Vec<(u32, u64)> = self
            .rows
            .iter()
            .filter(|r| r.pid != own && !r.protected)
            .filter(|r| r.cpu < 0.5)
            .filter(|r| r.rss >= min)
            .filter(|r| Some(r.pid) != fg && !visible.contains(&r.pid))
            .filter(|r| {
                !matches!(
                    r.name.to_ascii_lowercase().as_str(),
                    "explorer.exe"
                        | "dwm.exe"
                        | "audiodg.exe"
                        | "memory compression"
                        | "msmpeng.exe"
                )
            })
            .map(|r| (r.pid, r.rss))
            .collect();
        targets.sort_by(|a, b| b.1.cmp(&a.1));
        targets.truncate(10);

        let (freed, count) = self.trim_targets(&targets);
        self.log(
            "info",
            format!(
                "Alivio de memoria: {} liberados en {count} proceso(s) de fondo",
                fmt_mb(freed)
            ),
        );
        (freed, count)
    }

    pub fn restore(&mut self, pid: u32) -> Result<(), String> {
        let key = self.key_for_pid(pid);
        let original = self.applied.remove(&key);
        if let Some(original) = original {
            let mut errors = Vec::new();
            if let Some(nice) = original.nice {
                if let Err(err) = win::set_priority_class(pid, nice) {
                    errors.push(err);
                }
            }
            if let Some(affinity) = &original.affinity {
                if let Err(err) = win::set_affinity(pid, affinity) {
                    errors.push(err);
                }
            }
            if let Some(eco) = original.eco {
                if let Err(err) = win::set_eco(pid, eco) {
                    errors.push(err);
                }
            }
            let _ = win::set_io_priority(pid, 2);
            self.meta.remove(&key);
            self.log("info", format!("{} (PID {pid}) restaurado", original.name));
            if !errors.is_empty() {
                self.log(
                    "warn",
                    format!("restauración parcial: {}", errors.join("; ")),
                );
            }
            return Ok(());
        }
        if self.pb.contains_key(&key) {
            self.pb_restore(key, "pedido manual");
            return Ok(());
        }
        Err("No hay cambios guardados para ese proceso".into())
    }

    pub fn restore_all(&mut self) -> u32 {
        let mut pids: Vec<u32> = self.applied.keys().map(|(pid, _)| *pid).collect();
        pids.extend(self.pb.values().map(|r| r.pid));
        pids.sort_unstable();
        pids.dedup();
        let mut count = 0;
        for pid in pids {
            if self.restore(pid).is_ok() {
                count += 1;
            }
        }
        self.log("info", format!("restaurados {count} proceso(s)"));
        count
    }

    pub fn kill(&mut self, pid: u32) -> Result<(), String> {
        let name = self.name_for(pid);
        win::terminate_process(pid)?;
        self.log(
            "warn",
            format!("{name} (PID {pid}) terminado por el usuario"),
        );
        Ok(())
    }

    // ---------------------------------------------------------------- ProBalanza

    fn pb_step(&mut self) {
        if !self.config.probalance.enabled {
            return;
        }
        let cfg = self.config.probalance.clone();
        let foreground = self.system.foreground;
        let own = std::process::id();
        let visible = self.visible.clone();
        let mut actions: Vec<(String, Key, u32, String, f64)> = Vec::new();

        for row in &self.rows {
            let key = row.key();
            let pid = row.pid;
            if pid == own {
                continue;
            }
            if self.pb.contains_key(&key) {
                if cfg.skip_foreground && Some(pid) == foreground {
                    actions.push((
                        "restore".into(),
                        key,
                        pid,
                        "volvió a primer plano".into(),
                        0.0,
                    ));
                    continue;
                }
                if cfg.skip_interactive && visible.contains(&pid) {
                    actions.push((
                        "restore".into(),
                        key,
                        pid,
                        "tiene ventana visible".into(),
                        0.0,
                    ));
                    continue;
                }
                let samples = self
                    .samples
                    .entry(key)
                    .or_insert_with(|| VecDeque::with_capacity(60));
                samples.push_back(row.cpu);
                if samples.len() > 60 {
                    samples.pop_front();
                }
                if should_restore(samples, cfg.restore_below, cfg.restore_s) {
                    actions.push((
                        "restore".into(),
                        key,
                        pid,
                        format!("{:.0}% de CPU", row.cpu),
                        0.0,
                    ));
                }
                continue;
            }
            if row.protected || pid <= 4 {
                continue;
            }
            if cfg.skip_foreground && Some(pid) == foreground {
                continue;
            }
            if cfg.skip_interactive && visible.contains(&pid) {
                continue;
            }
            if self
                .pb_cooldown
                .get(&key)
                .map(|t| t.elapsed().as_secs_f64() < cfg.cooldown_s as f64)
                .unwrap_or(false)
            {
                continue;
            }
            if row
                .rule
                .as_ref()
                .map(|r| r.priority.is_some())
                .unwrap_or(false)
            {
                continue;
            }
            let Some(nice) = row.nice else { continue };
            if win::priority_rank(nice) <= win::priority_rank(BELOW_NORMAL_PRIORITY_CLASS) {
                continue;
            }
            if row.cpu < 1.0 {
                continue;
            }
            let samples = self
                .samples
                .entry(key)
                .or_insert_with(|| VecDeque::with_capacity(60));
            samples.push_back(row.cpu);
            if samples.len() > 60 {
                samples.pop_front();
            }
            if should_throttle(
                samples,
                cfg.threshold,
                cfg.sustain_s,
                cfg.fast_threshold,
                cfg.fast_sustain_s,
            ) {
                let window: Vec<f64> = samples
                    .iter()
                    .rev()
                    .take(cfg.sustain_s.max(1) as usize)
                    .copied()
                    .collect();
                let avg = window.iter().sum::<f64>() / window.len() as f64;
                actions.push(("throttle".into(), key, pid, row.name.clone(), avg));
            }
        }

        for (kind, key, pid, name, avg) in actions {
            if kind == "throttle" {
                self.pb_throttle(key, pid, &name, avg);
            } else {
                self.pb_restore(key, &name);
            }
        }
    }

    fn pb_throttle(&mut self, key: Key, pid: u32, name: &str, avg: f64) {
        let cfg = self.config.probalance.clone();
        let Some(original) = win::get_priority_class(pid) else {
            return;
        };
        if win::priority_rank(original) <= win::priority_rank(BELOW_NORMAL_PRIORITY_CLASS) {
            return;
        }
        let target_key = if cfg.priority == "idle" {
            "idle"
        } else {
            "below_normal"
        };
        let Some(target) = win::priority_class_from_key(target_key) else {
            return;
        };
        if let Err(err) = win::set_priority_class(pid, target) {
            self.log(
                "warn",
                format!("ProBalanza no pudo bajar {name} (PID {pid}): {err}"),
            );
            return;
        }
        let mut eco_done = false;
        let mut orig_eco = None;
        if cfg.eco {
            orig_eco = win::get_eco(pid);
            if win::set_eco(pid, true).is_ok() {
                eco_done = true;
            }
        }
        self.meta.remove(&key);
        // Empezar a contar la calma desde el throttle, no desde las muestras
        // que había antes: si no, se restaura demasiado rápido y vuelve a
        // oscilar.
        if let Some(queue) = self.samples.get_mut(&key) {
            queue.clear();
        }
        self.pb.insert(
            key,
            PbRecord {
                pid,
                name: name.to_string(),
                orig_nice: original,
                orig_eco,
                eco_done,
            },
        );
        self.log(
            "pb",
            format!(
                "ProBalanza bajó {name} (PID {pid}) a {} · {avg:.0}% CPU sostenido",
                win::priority_label(target)
            ),
        );
    }

    fn pb_restore(&mut self, key: Key, why: &str) {
        let Some(record) = self.pb.remove(&key) else {
            return;
        };
        // Si el PID ya no es el mismo proceso (Windows reutiliza PIDs), no
        // tocar la prioridad de un proceso ajeno.
        if !self.rows.iter().any(|row| row.key() == key) {
            self.meta.remove(&key);
            return;
        }
        let mut error = None;
        if let Err(err) = win::set_priority_class(record.pid, record.orig_nice) {
            error = Some(err);
        }
        if record.eco_done {
            if let Some(orig) = record.orig_eco {
                let _ = win::set_eco(record.pid, orig);
            }
        }
        self.meta.remove(&key);
        if let Some(err) = error {
            self.log(
                "warn",
                format!("ProBalanza no pudo restaurar PID {}: {err}", record.pid),
            );
            return;
        }
        self.log(
            "pb",
            format!(
                "ProBalanza restauró {} (PID {}) · {why}",
                record.name, record.pid
            ),
        );
        let cooldown = self.config.probalance.cooldown_s;
        if cooldown > 0 {
            self.pb_cooldown.insert(key, Instant::now());
        }
    }

    fn restore_all_pb(&mut self, why: &str) {
        let keys: Vec<Key> = self.pb.keys().copied().collect();
        for key in keys {
            self.pb_restore(key, why);
        }
    }

    // ---------------------------------------------------------------- primer plano

    fn fg_step(&mut self) {
        let cfg = self.config.foreground.clone();
        if !cfg.enabled {
            if self.fg_boost.is_some() {
                self.fg_release();
            }
            return;
        }
        let Some(target) = win::priority_class_from_key(&cfg.priority) else {
            return;
        };
        let foreground = self.system.foreground;
        let row = foreground.and_then(|pid| self.rows.iter().find(|r| r.pid == pid).cloned());
        if self.fg_boost.is_some()
            && row.as_ref().map(|r| r.pid) != self.fg_boost.as_ref().map(|b| b.pid)
        {
            self.fg_release();
        }
        let Some(row) = row else { return };
        if row.pid == std::process::id() || row.protected {
            return;
        }
        if row
            .rule
            .as_ref()
            .map(|r| r.priority.is_some())
            .unwrap_or(false)
        {
            return;
        }
        let Some(nice) = row.nice else { return };
        if win::priority_rank(nice) >= win::priority_rank(target) {
            return;
        }
        if self.fg_boost.is_some() {
            return;
        }
        if let Err(err) = win::set_priority_class(row.pid, target) {
            self.log(
                "warn",
                format!("primer plano: no pude subir {}: {err}", row.name),
            );
            return;
        }
        self.fg_boost = Some(FgBoost {
            pid: row.pid,
            key: row.key(),
            orig_nice: nice,
            target,
            name: row.name.clone(),
        });
        self.log(
            "info",
            format!(
                "primer plano: {} (PID {}) → {}",
                row.name,
                row.pid,
                win::priority_label(target)
            ),
        );
    }

    fn fg_release(&mut self) {
        let Some(record) = self.fg_boost.take() else {
            return;
        };
        if let Some(current) = win::get_priority_class(record.pid) {
            if current == record.target {
                let _ = win::set_priority_class(record.pid, record.orig_nice);
                self.meta.remove(&record.key);
                self.log(
                    "info",
                    format!(
                        "primer plano: {} (PID {}) vuelve a su prioridad",
                        record.name, record.pid
                    ),
                );
            }
        }
    }

    // ---------------------------------------------------------------- watchdog

    fn runaway_step(&mut self) {
        let cfg = self.config.runaway.clone();
        if !cfg.enabled {
            self.runaway_since.clear();
            self.runaway_warned.clear();
            self.runaways.clear();
            return;
        }
        let alive: KeySet = self.rows.iter().map(|r| r.key()).collect();
        self.runaway_since.retain(|k, _| alive.contains(k));
        self.runaway_warned.retain(|k| alive.contains(k));
        self.runaways.clear();

        let own = std::process::id();
        let foreground = self.system.foreground;
        let visible = if self.visible.is_empty() {
            win::visible_window_pids()
        } else {
            self.visible.clone()
        };
        let rows: Vec<(Key, u32, String, f64, bool)> = self
            .rows
            .iter()
            .map(|row| (row.key(), row.pid, row.name.clone(), row.cpu, row.protected))
            .collect();

        for (key, pid, name, cpu, protected) in rows {
            if pid == own
                || pid <= 4
                || protected
                || Some(pid) == foreground
                || visible.contains(&pid)
            {
                self.runaway_since.remove(&key);
                continue;
            }
            if cpu < cfg.cpu_pct {
                self.runaway_since.remove(&key);
                self.runaway_warned.remove(&key);
                continue;
            }
            let since = *self.runaway_since.entry(key).or_insert_with(Instant::now);
            let minutes = since.elapsed().as_secs_f64() / 60.0;
            if minutes < cfg.minutes as f64 {
                continue;
            }
            self.runaways.push(RunawayInfo {
                pid,
                name: name.clone(),
                cpu,
                minutes,
                action: cfg.action.clone(),
            });
            if !self.runaway_warned.insert(key) {
                continue;
            }
            self.log(
                "warn",
                format!(
                    "proceso colgado: {name} (PID {pid}) lleva {minutes:.0} min con {cpu:.1}% de CPU"
                ),
            );
            if cfg.action == "kill" {
                match win::terminate_process(pid) {
                    Ok(()) => {
                        self.log("warn", format!("watchdog terminó {name} (PID {pid})"));
                        self.runaway_since.remove(&key);
                    }
                    Err(err) => self.log(
                        "warn",
                        format!("watchdog no pudo terminar {name} (PID {pid}): {err}"),
                    ),
                }
            }
        }
    }

    fn watchdog_step(&mut self) {
        let rules: Vec<Rule> = self
            .config
            .rules
            .iter()
            .filter(|r| r.enabled && (r.watchdog.keep_alive || r.watchdog.max_instances > 0))
            .cloned()
            .collect();
        for rule in rules {
            let matches: Vec<(u32, String, f64)> = self
                .rows
                .iter()
                .filter(|row| self.rule_matches_quiet(&rule, row))
                .map(|r| (r.pid, r.name.clone(), r.ct))
                .collect();
            if rule.watchdog.max_instances > 0
                && matches.len() > rule.watchdog.max_instances as usize
            {
                let mut sorted = matches.clone();
                sorted.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
                let extra = sorted.len() - rule.watchdog.max_instances as usize;
                for (pid, name, _) in sorted.into_iter().take(extra) {
                    if pid == std::process::id() {
                        continue;
                    }
                    if let Err(err) = win::terminate_process(pid) {
                        self.log("warn", format!("watchdog: no pude cerrar PID {pid}: {err}"));
                    } else {
                        self.log(
                            "warn",
                            format!("watchdog: cerré instancia extra de {name} (PID {pid})"),
                        );
                    }
                }
            }
            if rule.watchdog.keep_alive && matches.is_empty() {
                let pattern = rule.pattern.trim();
                if !pattern.contains('\\') && !pattern.contains('/') {
                    continue;
                }
                let exe = PathBuf::from(pattern);
                if !exe.exists() {
                    if self.wd_warned.insert(rule.id.clone()) {
                        self.log("warn", format!("watchdog: no encuentro {pattern}"));
                    }
                    continue;
                }
                let missing = self
                    .wd_missing
                    .entry(rule.id.clone())
                    .or_insert_with(Instant::now);
                if missing.elapsed().as_secs_f64() < 5.0 {
                    continue;
                }
                if let Some(last) = self.wd_last_launch.get(&rule.id) {
                    if last.elapsed().as_secs_f64() < 30.0 {
                        continue;
                    }
                }
                let mut command = Command::new(&exe);
                if let Some(parent) = exe.parent() {
                    command.current_dir(parent);
                }
                match command.spawn() {
                    Ok(_) => {
                        self.wd_last_launch.insert(rule.id.clone(), Instant::now());
                        self.wd_missing.remove(&rule.id);
                        self.log("rule", format!("watchdog: relancé {}", exe.display()));
                    }
                    Err(err) => self.log("warn", format!("watchdog: no pude relanzar: {err}")),
                }
            }
        }
    }

    fn rule_matches_quiet(&self, rule: &Rule, row: &ProcessRow) -> bool {
        let pattern = rule.pattern.trim();
        if pattern.contains('\\') || pattern.contains('/') {
            let normalized = pattern.replace('/', "\\");
            let base = normalized.rsplit('\\').next().unwrap_or(&normalized);
            wildcard(base, &row.name) && (row.exe.is_empty() || wildcard(&normalized, &row.exe))
        } else {
            wildcard(pattern, &row.name)
        }
    }

    // ---------------------------------------------------------------- SmartTrim

    fn trim_step(&mut self) {
        if !self.config.trim.enabled {
            return;
        }
        if self.last_trim.elapsed().as_secs_f64() < self.config.trim.interval_s.max(30) as f64 {
            return;
        }
        if self.system.ram < self.config.trim.threshold {
            return;
        }
        self.trim_all();
    }

    // ---------------------------------------------------------------- MemGuard

    /// Evalúa la presión de memoria del tick, avisa (con cooldown) y, sólo si
    /// el usuario lo pidió, alivia working sets de fondo. Nunca termina nada.
    fn memguard_step(&mut self) {
        let mem = self.system.mem.clone();
        let total = self.system.ram_total.max(1);
        let commit_pct = if mem.commit_limit > 0 {
            mem.commit_used as f64 / mem.commit_limit as f64 * 100.0
        } else {
            0.0
        };
        let available_pct = mem.available as f64 / total as f64 * 100.0;

        let warn_commit = self.memguard.warn_commit_pct.clamp(50.0, 99.0);
        let crit_commit = (warn_commit + 7.0).min(99.0);
        let warn_avail = self.memguard.warn_available_pct.clamp(0.5, 40.0);
        let crit_avail = (warn_avail * 0.5).max(0.5);

        let mut reasons: Vec<String> = Vec::new();
        let mut level = "ok";
        if commit_pct >= crit_commit {
            level = "critical";
            reasons.push(format!(
                "commit {commit_pct:.0}% (crítico ≥ {crit_commit:.0}%)"
            ));
        } else if commit_pct >= warn_commit {
            level = "warn";
            reasons.push(format!(
                "commit {commit_pct:.0}% (aviso ≥ {warn_commit:.0}%)"
            ));
        }
        if available_pct > 0.0 && available_pct <= crit_avail {
            level = "critical";
            reasons.push(format!("disponible {available_pct:.0}% de la RAM"));
        } else if available_pct > 0.0 && available_pct <= warn_avail {
            if level == "ok" {
                level = "warn";
            }
            reasons.push(format!("disponible {available_pct:.0}% de la RAM"));
        }

        let previous = self.mem_pressure.level.clone();
        let pressure = &mut self.mem_pressure;
        pressure.level = level.to_string();
        pressure.reasons = reasons;
        pressure.commit_pct = (commit_pct * 10.0).round() / 10.0;
        pressure.available_pct = (available_pct * 10.0).round() / 10.0;
        pressure.available = mem.available;
        pressure.commit_used = mem.commit_used;
        pressure.commit_limit = mem.commit_limit;
        pressure.commit_free = mem.commit_limit.saturating_sub(mem.commit_used);
        pressure.cache = mem.cache;
        pressure.pool_paged = mem.pool_paged;
        pressure.pool_nonpaged = mem.pool_nonpaged;
        pressure.handles = mem.handles;
        pressure.processes = mem.processes;

        if !self.memguard.enabled {
            self.mem_pressure.warned = false;
            return;
        }

        let now = now_epoch();
        if level != "ok" {
            let cooldown = self.memguard.warn_cooldown_s.max(30) as f64;
            let escalated = previous != "ok" && previous != level;
            if escalated
                || !self.mem_pressure.warned
                || now - self.mem_pressure.warned_at >= cooldown
            {
                self.mem_pressure.warned = true;
                self.mem_pressure.warned_at = now;
                let why = self.mem_pressure.reasons.join(" · ");
                self.log(
                    "warn",
                    format!(
                        "MemGuard: presión de memoria {level} · {why} · commit {} de {} · disponible {} · cache {}",
                        fmt_mb(mem.commit_used),
                        fmt_mb(mem.commit_limit),
                        fmt_mb(mem.available),
                        fmt_mb(mem.cache)
                    ),
                );
            }
            if self.memguard.relief {
                let interval = self.memguard.relief_interval_s.max(60) as f64;
                if self.last_relief_attempt.elapsed().as_secs_f64() >= interval {
                    self.last_relief_attempt = Instant::now();
                    let (freed, count) = self.trim_background();
                    self.mem_pressure.reliefs = self.mem_pressure.reliefs.saturating_add(1);
                    self.mem_pressure.last_relief_at = now;
                    self.mem_pressure.last_relief_freed = freed;
                    self.log(
                        "info",
                        format!(
                            "MemGuard: alivio automático en {count} proceso(s) de fondo · {}",
                            fmt_mb(freed)
                        ),
                    );
                }
            }
        } else if previous != "ok" {
            self.mem_pressure.warned = false;
            self.log("info", "MemGuard: presión de memoria normalizada");
        }
    }

    /// Ajusta los umbrales de MemGuard. Compatible con configs viejas: lo que
    /// falta en el JSON queda como estaba.
    pub fn set_memguard(&mut self, patch: &Value) -> Result<MemGuard, String> {
        let cfg = &mut self.memguard;
        if let Some(v) = patch.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = patch.get("warn_commit_pct").and_then(|v| v.as_f64()) {
            cfg.warn_commit_pct = v.clamp(50.0, 99.0);
        }
        if let Some(v) = patch.get("warn_available_pct").and_then(|v| v.as_f64()) {
            cfg.warn_available_pct = v.clamp(0.5, 40.0);
        }
        if let Some(v) = patch.get("relief").and_then(|v| v.as_bool()) {
            cfg.relief = v;
        }
        if let Some(v) = patch.get("relief_interval_s").and_then(|v| v.as_u64()) {
            cfg.relief_interval_s = (v as u32).clamp(60, 86_400);
        }
        if let Some(v) = patch.get("warn_cooldown_s").and_then(|v| v.as_u64()) {
            cfg.warn_cooldown_s = (v as u32).clamp(60, 86_400);
        }
        let result = cfg.clone();
        if !result.enabled {
            self.mem_pressure.warned = false;
        }
        self.log(
            "info",
            format!(
                "MemGuard {} · aviso commit ≥ {:.0}% o disponible ≤ {:.0}% · alivio {}",
                if result.enabled { "activo" } else { "apagado" },
                result.warn_commit_pct,
                result.warn_available_pct,
                if result.relief {
                    "automático"
                } else {
                    "manual"
                }
            ),
        );
        self.save();
        Ok(result)
    }

    /// Alivio manual (working sets de fondo, sin tocar ventanas visibles).
    pub fn relief_now(&mut self) -> (u64, u32) {
        self.last_relief_attempt = Instant::now();
        let (freed, count) = self.trim_background();
        self.mem_pressure.reliefs = self.mem_pressure.reliefs.saturating_add(1);
        self.mem_pressure.last_relief_at = now_epoch();
        self.mem_pressure.last_relief_freed = freed;
        (freed, count)
    }

    /// Agregado por familia (Chrome/Edge, WebView2, Electron) sin alterar la
    /// lista de procesos que ya consume la UI.
    pub fn families(&self) -> Vec<FamilyInfo> {
        let mut groups: HashMap<&'static str, FamilyInfo> = HashMap::new();
        for row in &self.rows {
            let Some(family) = family_of(&row.name) else {
                continue;
            };
            let entry = groups.entry(family).or_insert_with(|| FamilyInfo {
                family: family.to_string(),
                label: family_label(family).unwrap_or(family).to_string(),
                ..FamilyInfo::default()
            });
            entry.count += 1;
            entry.cpu += row.cpu;
            entry.rss += row.rss;
            entry.private += row.private;
            entry.protected += row.protected as usize;
            entry.applied += row.applied as usize;
            entry.throttled += row.pb as usize;
            if entry.pids.len() < 256 {
                entry.pids.push(row.pid);
            } else {
                entry.pids_truncated = true;
            }
        }
        let mut list: Vec<FamilyInfo> = groups.into_values().collect();
        list.sort_by(|a, b| b.rss.cmp(&a.rss).then_with(|| a.family.cmp(&b.family)));
        for group in &mut list {
            group.cpu = (group.cpu * 10.0).round() / 10.0;
        }
        list
    }

    /// Aplica una acción a todos los procesos de una familia. No termina
    /// procesos: para eso está `/api/kill` con un PID explícito.
    pub fn family_action(
        &mut self,
        family: &str,
        action: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let family = family.trim().to_ascii_lowercase();
        let label = family_label(&family)
            .ok_or_else(|| format!("familia desconocida: {family}"))?
            .to_string();
        let targets: Vec<(u32, String, u64)> = self
            .rows
            .iter()
            .filter(|row| family_of(&row.name) == Some(family.as_str()))
            .filter(|row| row.pid != std::process::id())
            .map(|row| (row.pid, row.name.clone(), row.rss))
            .collect();

        let need_str = |key: &str| -> Result<String, String> {
            body.get(key)
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())
                .ok_or_else(|| format!("Falta {key}"))
        };

        let mut done = 0u32;
        let mut freed = 0u64;
        let mut errors: Vec<String> = Vec::new();
        match action {
            "priority" => {
                let cls = need_str("cls")?;
                for (pid, name, _) in &targets {
                    match self.set_priority(*pid, &cls) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "eco" => {
                let enabled = body
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .ok_or("Falta enabled")?;
                for (pid, name, _) in &targets {
                    match self.set_eco(*pid, enabled) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "io" => {
                let level = need_str("level")?;
                for (pid, name, _) in &targets {
                    match self.set_io(*pid, &level) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "mem" => {
                let level = need_str("level")?;
                for (pid, name, _) in &targets {
                    match self.set_memory_priority(*pid, &level) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "affinity" => {
                let cpus: Vec<u32> = body
                    .get("cpus")
                    .and_then(|v| v.as_array())
                    .ok_or("Falta cpus")?
                    .iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u32))
                    .collect();
                if cpus.is_empty() {
                    return Err("Falta cpus".into());
                }
                for (pid, name, _) in &targets {
                    match self.set_affinity(*pid, &cpus) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "restore" => {
                for (pid, name, _) in &targets {
                    match self.restore(*pid) {
                        Ok(()) => done += 1,
                        Err(err) => push_error(&mut errors, format!("{name} (PID {pid}): {err}")),
                    }
                }
            }
            "trim" => {
                let list: Vec<(u32, u64)> = targets
                    .iter()
                    .map(|(pid, _, rss)| (*pid, *rss))
                    .collect();
                let (bytes, count) = self.trim_targets(&list);
                freed = bytes;
                done = count;
            }
            other => {
                return Err(format!(
                    "acción de familia no soportada: {other} (priority, eco, io, mem, affinity, trim, restore)"
                ))
            }
        }

        if targets.is_empty() {
            return Ok(json!({
                "ok": true, "family": family, "label": label, "action": action,
                "count": 0, "freed": 0, "errors": [], "note": "no hay procesos de esa familia"
            }));
        }
        self.log(
            "info",
            format!("familia {label}: {action} aplicado a {done} proceso(s)"),
        );
        Ok(json!({
            "ok": true, "family": family, "label": label, "action": action,
            "count": done, "freed": freed, "errors": errors
        }))
    }

    // ---------------------------------------------------------------- energía

    pub fn refresh_plans(&mut self) {
        match power::list_plans() {
            Ok(plans) => {
                self.plans = plans;
                self.active_plan = power::active_plan()
                    .or_else(|| self.plans.iter().find(|p| p.active).map(|p| p.guid.clone()));
                if self.profile.plan.is_empty() {
                    if let Some(active) = &self.active_plan {
                        if let Some(plan) = self.plans.iter().find(|p| &p.guid == active) {
                            self.profile.plan = plan.name.clone();
                            self.profile.guid = plan.guid.clone();
                        }
                    }
                }
            }
            Err(err) => self.log("warn", format!("no pude leer los planes: {err}")),
        }
        self.last_plan_scan = Instant::now();
    }

    pub fn set_plan(&mut self, guid: &str) -> Result<(), String> {
        if !self.plans.iter().any(|p| p.guid == guid) {
            return Err("Plan de energía desconocido".into());
        }
        power::set_active(guid)?;
        self.active_plan = Some(guid.to_string());
        if let Some(plan) = self.plans.iter().find(|p| p.guid == guid) {
            self.profile.plan = plan.name.clone();
            self.profile.guid = plan.guid.clone();
            self.log("info", format!("plan de energía: {}", plan.name));
        }
        self.save();
        Ok(())
    }

    fn power_step(&mut self) {
        if !self.config.power.auto {
            return;
        }
        let trigger = self.config.power.trigger.trim().to_lowercase();
        if trigger.is_empty()
            || self.config.power.base_plan.is_empty()
            || self.config.power.game_plan.is_empty()
        {
            return;
        }
        if self.last_power_check.elapsed().as_secs_f64() < 10.0 {
            return;
        }
        self.last_power_check = Instant::now();
        let running = self.rows.iter().any(|r| r.name.to_lowercase() == trigger);
        let target = if running {
            self.config.power.game_plan.clone()
        } else {
            self.config.power.base_plan.clone()
        };
        if self.active_plan.as_deref() != Some(target.as_str()) {
            if let Err(err) = self.set_plan(&target) {
                self.log("warn", format!("plan automático: {err}"));
            }
        }
    }

    // ---------------------------------------------------------------- perfiles

    pub fn set_profile(
        &mut self,
        name: &str,
        target: Option<String>,
    ) -> Result<ProfileState, String> {
        if name != self.profile.active {
            self.restore_profile_changes();
        }
        match name {
            "balanced" => {
                let previous = self.profile.previous_plan.clone();
                if !previous.is_empty() {
                    let _ = power::set_active(&previous);
                    self.active_plan = Some(previous.clone());
                    if let Some(plan) = self.plans.iter().find(|p| p.guid == previous) {
                        self.profile.plan = plan.name.clone();
                        self.profile.guid = plan.guid.clone();
                    }
                }
                self.profile.active = "balanced".into();
                self.profile.target = None;
                self.profile.since = now_epoch();
                self.log("info", "perfil: Equilibrado");
            }
            "full_power" => {
                if !win::ac_online() {
                    self.log(
                        "warn",
                        "Full Power pedido con batería: va a consumir y calentar",
                    );
                }
                let previous = if self.profile.active == "balanced" {
                    self.active_plan.clone().unwrap_or_default()
                } else {
                    self.profile.previous_plan.clone()
                };
                let (guid, plan, warnings) = power::ensure_full_power()?;
                for warning in warnings {
                    self.log("warn", format!("Full Power: {warning}"));
                }
                self.profile.active = "full_power".into();
                self.profile.plan = plan.clone();
                self.profile.guid = guid.clone();
                self.profile.previous_plan = previous;
                self.profile.target = target.clone();
                self.profile.since = now_epoch();
                self.active_plan = Some(guid);
                self.refresh_plans();
                let patterns = self.config.ai.patterns.clone();
                self.boost_patterns(&patterns, "khigh", false);
                if let Some(target) = &target {
                    self.boost_patterns(&[target.clone()], "high", false);
                }
                self.log("info", format!("perfil: Full Power · {plan}"));
            }
            "gaming" => {
                let target = target
                    .or_else(|| self.system.foreground_name.clone())
                    .ok_or("Elegí el ejecutable del juego")?;
                let previous = if self.profile.active == "balanced" {
                    self.active_plan.clone().unwrap_or_default()
                } else {
                    self.profile.previous_plan.clone()
                };
                let configured = self.config.power.game_plan.clone();
                let target_plan = if !configured.is_empty() {
                    configured
                } else {
                    power::ensure_full_power()?.0
                };
                power::set_active(&target_plan)?;
                self.active_plan = Some(target_plan.clone());
                self.profile.active = "gaming".into();
                self.profile.plan = self
                    .plans
                    .iter()
                    .find(|p| p.guid == target_plan)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "Alto rendimiento".into());
                self.profile.guid = target_plan;
                self.profile.previous_plan = previous;
                self.profile.target = Some(target.clone());
                self.profile.since = now_epoch();
                self.boost_patterns(&[target.clone()], "high", false);
                let ai = self.config.ai.patterns.clone();
                self.boost_patterns(&ai, "above_normal", false);
                self.log("info", format!("perfil: Juego · {target}"));
            }
            "ai" => {
                let patterns = self.config.ai.patterns.clone();
                self.boost_patterns(&patterns, "above_normal", false);
                for pattern in &patterns {
                    if !self
                        .config
                        .probalance
                        .protect
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(pattern))
                    {
                        self.config.probalance.protect.push(pattern.clone());
                    }
                }
                self.profile.active = "ai".into();
                self.profile.target = None;
                self.profile.since = now_epoch();
                self.log("info", "perfil: IA (herramientas priorizadas y protegidas)");
            }
            other => return Err(format!("Perfil desconocido: {other}")),
        }
        self.save();
        Ok(self.profile.clone())
    }

    fn boost_patterns(&mut self, patterns: &[String], priority: &str, eco: bool) {
        let priority = if priority == "khigh" {
            "high"
        } else {
            priority
        };
        let targets: Vec<(u32, String)> = self
            .rows
            .iter()
            .filter(|row| patterns.iter().any(|p| wildcard(p, &row.name)))
            .map(|row| (row.pid, row.name.clone()))
            .collect();
        for (pid, name) in targets {
            if pid == std::process::id() {
                continue;
            }
            let mut changed = false;
            if let Some(class) = win::priority_class_from_key(priority) {
                if win::get_priority_class(pid)
                    .map(|c| win::priority_rank(c) < win::priority_rank(class))
                    .unwrap_or(true)
                    && self.set_priority(pid, priority).is_ok()
                {
                    changed = true;
                }
            }
            if self.set_eco(pid, eco).is_ok() {
                changed = true;
            }
            if changed {
                if !self.profile_applied.contains(&pid) {
                    self.profile_applied.push(pid);
                }
                self.log("info", format!("perfil: {name} (PID {pid}) potenciado"));
            }
        }
    }

    fn restore_profile_changes(&mut self) {
        let pids = std::mem::take(&mut self.profile_applied);
        for pid in pids {
            let _ = self.restore(pid);
        }
    }

    // ---------------------------------------------------------------- API config

    pub fn set_probalance(&mut self, patch: &Value) -> Result<ProBalance, String> {
        {
            let cfg = &mut self.config.probalance;
            if let Some(v) = patch.get("enabled").and_then(|v| v.as_bool()) {
                cfg.enabled = v;
            }
            if let Some(v) = patch.get("threshold").and_then(|v| v.as_f64()) {
                cfg.threshold = v.clamp(1.0, 100.0);
            }
            if let Some(v) = patch.get("sustain_s").and_then(|v| v.as_u64()) {
                cfg.sustain_s = (v as u32).clamp(1, 60);
            }
            if let Some(v) = patch.get("fast_threshold").and_then(|v| v.as_f64()) {
                cfg.fast_threshold = v.clamp(0.0, 100.0);
            }
            if let Some(v) = patch.get("fast_sustain_s").and_then(|v| v.as_u64()) {
                cfg.fast_sustain_s = (v as u32).clamp(0, 60);
            }
            if let Some(v) = patch.get("restore_below").and_then(|v| v.as_f64()) {
                cfg.restore_below = v.clamp(0.0, 100.0);
            }
            if let Some(v) = patch.get("restore_s").and_then(|v| v.as_u64()) {
                cfg.restore_s = (v as u32).clamp(1, 120);
            }
            if let Some(v) = patch.get("priority").and_then(|v| v.as_str()) {
                if v == "idle" || v == "below_normal" {
                    cfg.priority = v.into();
                }
            }
            if let Some(v) = patch.get("eco").and_then(|v| v.as_bool()) {
                cfg.eco = v;
            }
            if let Some(v) = patch.get("skip_foreground").and_then(|v| v.as_bool()) {
                cfg.skip_foreground = v;
            }
            if let Some(v) = patch.get("skip_interactive").and_then(|v| v.as_bool()) {
                cfg.skip_interactive = v;
            }
            if let Some(v) = patch.get("cooldown_s").and_then(|v| v.as_u64()) {
                cfg.cooldown_s = (v as u32).min(600);
            }
            if let Some(items) = patch.get("protect").and_then(|v| v.as_array()) {
                cfg.protect = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .take(200)
                    .collect();
            }
        }
        let enabled = self.config.probalance.enabled;
        if !enabled {
            self.restore_all_pb("ProBalanza desactivada");
        }
        self.log(
            "info",
            format!(
                "ProBalanza {}",
                if enabled { "activada" } else { "desactivada" }
            ),
        );
        let result = self.config.probalance.clone();
        self.save();
        Ok(result)
    }

    pub fn set_runaway(&mut self, patch: &Value) -> Result<Runaway, String> {
        let cfg = &mut self.config.runaway;
        if let Some(v) = patch.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = patch.get("cpu_pct").and_then(|v| v.as_f64()) {
            cfg.cpu_pct = v.clamp(1.0, 100.0);
        }
        if let Some(v) = patch.get("minutes").and_then(|v| v.as_u64()) {
            cfg.minutes = (v as u32).clamp(1, 180);
        }
        if let Some(v) = patch.get("action").and_then(|v| v.as_str()) {
            if v == "warn" || v == "kill" {
                cfg.action = v.into();
            }
        }
        let result = cfg.clone();
        self.save();
        Ok(result)
    }

    pub fn set_trim(&mut self, patch: &Value) -> Result<Trim, String> {
        let cfg = &mut self.config.trim;
        if let Some(v) = patch.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = v;
        }
        if let Some(v) = patch.get("threshold").and_then(|v| v.as_f64()) {
            cfg.threshold = v.clamp(30.0, 100.0);
        }
        if let Some(v) = patch.get("min_mb").and_then(|v| v.as_u64()) {
            cfg.min_mb = (v as u32).clamp(10, 8192);
        }
        if let Some(v) = patch.get("interval_s").and_then(|v| v.as_u64()) {
            cfg.interval_s = (v as u32).clamp(30, 3600);
        }
        let result = cfg.clone();
        self.save();
        Ok(result)
    }

    pub fn set_foreground(&mut self, patch: &Value) -> Result<Foreground, String> {
        {
            let cfg = &mut self.config.foreground;
            if let Some(v) = patch.get("enabled").and_then(|v| v.as_bool()) {
                cfg.enabled = v;
            }
            if let Some(v) = patch.get("priority").and_then(|v| v.as_str()) {
                if win::priority_class_from_key(v).is_some() {
                    cfg.priority = v.into();
                }
            }
        }
        if !self.config.foreground.enabled {
            self.fg_release();
        }
        let result = self.config.foreground.clone();
        self.save();
        Ok(result)
    }

    pub fn set_power(&mut self, patch: &Value) -> Result<PowerCfg, String> {
        let cfg = &mut self.config.power;
        if let Some(v) = patch.get("auto").and_then(|v| v.as_bool()) {
            cfg.auto = v;
        }
        if let Some(v) = patch.get("trigger").and_then(|v| v.as_str()) {
            cfg.trigger = v.trim().to_string();
        }
        for key in ["base_plan", "game_plan"] {
            if let Some(v) = patch.get(key).and_then(|v| v.as_str()) {
                let v = v.trim();
                if !v.is_empty() && !self.plans.iter().any(|p| p.guid == v) {
                    return Err("Plan de energía desconocido".into());
                }
                if key == "base_plan" {
                    cfg.base_plan = v.to_string();
                } else {
                    cfg.game_plan = v.to_string();
                }
            }
        }
        let result = cfg.clone();
        self.save();
        Ok(result)
    }

    // ---------------------------------------------------------------- reglas CRUD

    pub fn add_rule(&mut self, payload: &Value) -> Result<Rule, String> {
        let rule = normalize_rule(payload, None)?;
        if self
            .config
            .rules
            .iter()
            .any(|r| r.pattern.eq_ignore_ascii_case(&rule.pattern))
        {
            return Err("Ya existe una regla con ese patrón".into());
        }
        self.config.rules.push(rule.clone());
        self.save();
        self.log("info", format!("regla creada: «{}»", rule.pattern));
        Ok(rule)
    }

    pub fn update_rule(&mut self, id: &str, payload: &Value) -> Result<Rule, String> {
        let index = self
            .config
            .rules
            .iter()
            .position(|r| r.id == id)
            .ok_or("Regla no encontrada")?;
        let updated = normalize_rule(payload, Some(self.config.rules[index].clone()))?;
        self.config.rules[index] = updated.clone();
        self.rules_done.clear();
        self.save();
        self.log("info", format!("regla actualizada: «{}»", updated.pattern));
        Ok(updated)
    }

    pub fn delete_rule(&mut self, id: &str) -> Result<(), String> {
        let before = self.config.rules.len();
        self.config.rules.retain(|r| r.id != id);
        if self.config.rules.len() == before {
            return Err("Regla no encontrada".into());
        }
        self.rules_done.clear();
        self.save();
        self.log("info", "regla eliminada");
        Ok(())
    }

    pub fn toggle_rule(&mut self, id: &str, enabled: Option<bool>) -> Result<Rule, String> {
        let rule = self
            .config
            .rules
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or("Regla no encontrada")?;
        rule.enabled = enabled.unwrap_or(!rule.enabled);
        let result = rule.clone();
        self.rules_done.clear();
        self.save();
        Ok(result)
    }

    // ---------------------------------------------------------------- estado

    pub fn snapshot(&mut self) -> Snapshot {
        if self.last_plan_scan.elapsed().as_secs_f64() > 60.0 {
            self.refresh_plans();
        }
        let applied_keys: KeySet = self.applied.keys().copied().collect();
        let pb_keys: KeySet = self.pb.keys().copied().collect();
        for row in &mut self.rows {
            row.applied = applied_keys.contains(&row.key());
            row.pb = pb_keys.contains(&row.key());
        }
        let active_name = self
            .active_plan
            .as_ref()
            .and_then(|guid| self.plans.iter().find(|p| &p.guid == guid))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if self.gpu_at.elapsed().as_secs_f64() > 5.0 {
            self.gpu = crate::gpu::query();
            self.gpu_at = Instant::now();
        }
        let tick_ms = self.tick_stats();
        Snapshot {
            app: AppInfo {
                name: APP_NAME.into(),
                version: VERSION.into(),
                admin: self.admin,
                pid: std::process::id(),
                uptime: self.started.elapsed().as_secs_f64(),
                config_path: self.config_path.display().to_string(),
                ncpu: self.ncpu,
                tick_ms,
                visible: self.visible.len(),
                eco_supported: self.eco_supported,
            },
            system: self.system.clone(),
            processes: self.rows.clone(),
            rules: self.config.rules.clone(),
            probalance: self.config.probalance.clone(),
            power: PowerInfo {
                auto: self.config.power.auto,
                trigger: self.config.power.trigger.clone(),
                base_plan: self.config.power.base_plan.clone(),
                game_plan: self.config.power.game_plan.clone(),
                plans: self.plans.clone(),
                active: self.active_plan.clone(),
                active_name,
                ac_line: win::ac_online(),
            },
            foreground: self.config.foreground.clone(),
            trim: self.config.trim.clone(),
            history: self.history.iter().cloned().collect(),
            log: self.log_lines.iter().cloned().collect(),
            counts: Counts {
                processes: self.rows.len(),
                rules: self.config.rules.len(),
                throttled: self.pb.len(),
                applied: self.applied.len(),
            },
            profile: self.profile.clone(),
            gpu: self.gpu.clone(),
            runaways: self.runaways.clone(),
            runaway: self.config.runaway.clone(),
        }
    }

    /// Snapshot serializado con los agregados nuevos. La forma existente no
    /// cambia: sólo se agregan claves (`memguard`, `mem_pressure`, `families`
    /// y `processes[].family`), así que las UIs viejas siguen funcionando.
    pub fn state_json(&mut self) -> Value {
        let snapshot = self.snapshot();
        let mut value = json!(snapshot);
        if let Some(map) = value.as_object_mut() {
            map.insert("memguard".into(), json!(self.memguard));
            map.insert("mem_pressure".into(), json!(self.mem_pressure));
            map.insert("families".into(), json!(self.families()));
            if let Some(processes) = map.get_mut("processes").and_then(|v| v.as_array_mut()) {
                for process in processes {
                    let Some(entry) = process.as_object_mut() else {
                        continue;
                    };
                    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(family) = family_of(name) {
                        entry.insert("family".into(), json!(family));
                    }
                }
            }
        }
        value
    }

    /// Informe de memoria para `/api/mem`: presión, umbrales, composición y
    /// mayores consumidores por memoria privada.
    pub fn mem_report(&self) -> Value {
        let mut rows: Vec<&ProcessRow> = self.rows.iter().collect();
        rows.sort_by(|a, b| b.private.cmp(&a.private).then_with(|| a.pid.cmp(&b.pid)));
        let top: Vec<Value> = rows
            .into_iter()
            .take(10)
            .map(|row| {
                json!({
                    "pid": row.pid,
                    "name": row.name,
                    "family": family_of(&row.name),
                    "rss": row.rss,
                    "private": row.private,
                    "cpu": row.cpu,
                })
            })
            .collect();
        json!({
            "ok": true,
            "memguard": self.memguard,
            "pressure": self.mem_pressure,
            "system": {
                "ram": self.system.ram,
                "ram_used": self.system.ram_used,
                "ram_total": self.system.ram_total,
                "mem": self.system.mem,
            },
            "families": self.families(),
            "top_private": top,
        })
    }

    fn tick_stats(&self) -> TickStats {
        if self.tick_ms.is_empty() {
            return TickStats {
                last: self.last_tick_ms,
                ..Default::default()
            };
        }
        let mut values: Vec<f64> = self.tick_ms.iter().copied().collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        TickStats {
            p50: values[values.len() / 2],
            p95: values[((values.len() as f64 * 0.95) as usize).min(values.len() - 1)],
            last: self.last_tick_ms,
            n: values.len(),
        }
    }

    pub fn shutdown(&mut self) {
        self.restore_all_pb("apagado");
        self.fg_release();
        if self.profile.active != "balanced" && !self.profile.previous_plan.is_empty() {
            let _ = power::set_active(&self.profile.previous_plan);
        }
        self.save();
    }

    pub fn json_status(&mut self) -> Value {
        let snapshot = self.snapshot();
        json!(snapshot)
    }
}

fn apply_meta(row: &mut ProcessRow, meta: &Meta, ncpu: usize) {
    row.nice = Some(meta.nice);
    row.prio = win::priority_key(meta.nice);
    row.prio_label = win::priority_label(meta.nice);
    row.affinity = Some(meta.affinity.clone());
    row.affinity_label = fmt_cpus(&meta.affinity, ncpu);
    row.eco = meta.eco;
    row.exact = true;
    if !meta.exe.is_empty() {
        row.exe = meta.exe.clone();
    }
}

pub fn fmt_cpus(cpus: &[u32], ncpu: usize) -> String {
    if cpus.is_empty() {
        return "—".into();
    }
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if ncpu > 0 && sorted.len() == ncpu {
        return "todos".into();
    }
    let mut parts = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];
    for value in sorted.iter().skip(1) {
        if *value == prev + 1 {
            prev = *value;
            continue;
        }
        parts.push(if start == prev {
            start.to_string()
        } else {
            format!("{start}-{prev}")
        });
        start = *value;
        prev = *value;
    }
    parts.push(if start == prev {
        start.to_string()
    } else {
        format!("{start}-{prev}")
    });
    parts.join(",")
}

fn should_throttle(
    samples: &VecDeque<f64>,
    threshold: f64,
    sustain_s: u32,
    fast_threshold: f64,
    fast_sustain_s: u32,
) -> bool {
    if fast_threshold > 0.0 && fast_sustain_s > 0 {
        let f = fast_sustain_s.max(1) as usize;
        if samples.len() >= f {
            let sum: f64 = samples.iter().rev().take(f).sum();
            if sum / f as f64 >= fast_threshold {
                return true;
            }
        }
    }
    let n = sustain_s.max(1) as usize;
    if samples.len() < n {
        return false;
    }
    let sum: f64 = samples.iter().rev().take(n).sum();
    sum / n as f64 >= threshold
}

fn should_restore(samples: &VecDeque<f64>, below: f64, restore_s: u32) -> bool {
    let n = restore_s.max(1) as usize;
    if samples.len() < n {
        return false;
    }
    let sum: f64 = samples.iter().rev().take(n).sum();
    sum / n as f64 <= below
}

fn normalize_rule(payload: &Value, existing: Option<Rule>) -> Result<Rule, String> {
    let mut rule = existing.unwrap_or_default();
    if let Some(pattern) = payload.get("pattern").and_then(|v| v.as_str()) {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Err("El patrón no puede quedar vacío".into());
        }
        rule.pattern = pattern.chars().take(260).collect();
    }
    if rule.pattern.is_empty() {
        return Err("Falta el patrón".into());
    }
    if let Some(value) = payload.get("priority") {
        rule.priority = value.as_str().map(|s| s.to_string());
        if let Some(p) = &rule.priority {
            win::priority_class_from_key(p).ok_or("Prioridad inválida")?;
        }
    }
    if let Some(value) = payload.get("affinity") {
        if value.is_null() {
            rule.affinity = None;
        } else {
            let cpus: Vec<u32> = value
                .as_array()
                .ok_or("Afinidad inválida")?
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect();
            if cpus.is_empty() {
                rule.affinity = None;
            } else {
                rule.affinity = Some(cpus);
            }
        }
    }
    if let Some(value) = payload.get("eco") {
        rule.eco = value.as_bool();
    }
    if let Some(value) = payload.get("io") {
        let value = value.as_str().unwrap_or("").to_string();
        if !value.is_empty() && !["very_low", "low", "normal", "high"].contains(&value.as_str()) {
            return Err("Prioridad de E/S inválida".into());
        }
        rule.io = if value.is_empty() { None } else { Some(value) };
    }
    if let Some(value) = payload.get("mem") {
        let value = value.as_str().unwrap_or("").to_string();
        if !value.is_empty()
            && !["very_low", "low", "medium", "below_normal", "normal"].contains(&value.as_str())
        {
            return Err("Prioridad de memoria inválida".into());
        }
        rule.mem = if value.is_empty() { None } else { Some(value) };
    }
    if let Some(value) = payload.get("note").and_then(|v| v.as_str()) {
        rule.note = value.chars().take(200).collect();
    }
    if let Some(value) = payload.get("watchdog") {
        if let Some(keep) = value.get("keep_alive").and_then(|v| v.as_bool()) {
            rule.watchdog.keep_alive = keep;
        }
        if let Some(max) = value.get("max_instances").and_then(|v| v.as_u64()) {
            rule.watchdog.max_instances = (max as u32).min(20);
        }
    }
    if rule.id.is_empty() {
        rule.id = format!(
            "{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
    }
    Ok(rule)
}

fn rule_matches_plain(rule: &Rule, row: &ProcessRow) -> bool {
    let pattern = rule.pattern.trim();
    if pattern.contains('\\') || pattern.contains('/') {
        if row.exe.is_empty() {
            return false;
        }
        let normalized = pattern.replace('/', "\\");
        let base = normalized.rsplit('\\').next().unwrap_or(&normalized);
        wildcard(base, &row.name) && wildcard(&normalized, &row.exe)
    } else {
        wildcard(pattern, &row.name)
    }
}

/// Primera regla habilitada que matchea la fila. Recibe la lista ya copiada
/// una vez por tick: antes se clonaba la lista completa por cada proceso y
/// sólo hacía falta clonar la regla ganadora.
fn match_rule_fast(
    rules: &[Rule],
    row: &ProcessRow,
    exe_cache: &mut HashMap<Key, String>,
) -> Option<Rule> {
    for rule in rules {
        if !rule.enabled || rule.pattern.trim().is_empty() {
            continue;
        }
        if rule_matches_cached(rule, row, exe_cache) {
            return Some(rule.clone());
        }
    }
    None
}

fn rule_matches_cached(
    rule: &Rule,
    row: &ProcessRow,
    exe_cache: &mut HashMap<Key, String>,
) -> bool {
    let pattern = rule.pattern.trim();
    if pattern.contains('\\') || pattern.contains('/') {
        let normalized = pattern.replace('/', "\\");
        let base = normalized.rsplit('\\').next().unwrap_or(&normalized);
        if !wildcard(base, &row.name) {
            return false;
        }
        let mut exe = row.exe.clone();
        if exe.is_empty() {
            if let Some(cached) = exe_cache.get(&row.key()) {
                exe = cached.clone();
            } else if let Some(found) = win::process_exe(row.pid) {
                exe_cache.insert(row.key(), found.clone());
                exe = found;
            }
        }
        !exe.is_empty() && wildcard(&normalized, &exe)
    } else {
        wildcard(pattern, &row.name)
    }
}

fn push_error(errors: &mut Vec<String>, message: String) {
    if errors.len() < 5 {
        errors.push(message);
    }
}

/// Lee la sección `memguard` del config. Si no existe (configs viejas) o está
/// malformada, se usan los valores por defecto.
fn load_memguard(path: &Path) -> MemGuard {
    let Ok(text) = std::fs::read_to_string(path) else {
        return MemGuard::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return MemGuard::default();
    };
    value
        .get("memguard")
        .and_then(|section| serde_json::from_value::<MemGuard>(section.clone()).ok())
        .unwrap_or_default()
}

/// Guarda `config` + `memguard` en un único archivo, preservando claves
/// desconocidas que ya estuvieran en el JSON (configs de versiones futuras).
fn save_config(path: &Path, config: &Config, memguard: &MemGuard) -> Result<(), String> {
    let mut value = serde_json::to_value(config).map_err(|err| err.to_string())?;
    let Some(map) = value.as_object_mut() else {
        return Err("el config no es un objeto JSON".into());
    };
    map.insert(
        "memguard".into(),
        serde_json::to_value(memguard).map_err(|err| err.to_string())?,
    );
    if let Ok(previous) = std::fs::read_to_string(path) {
        if let Ok(Value::Object(old)) = serde_json::from_str::<Value>(&previous) {
            for (key, old_value) in old {
                map.entry(key).or_insert(old_value);
            }
        }
    }
    let text = serde_json::to_string_pretty(&value).map_err(|err| err.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("no pude crear {parent:?}: {err}"))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|err| format!("no pude escribir config: {err}"))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("no pude reemplazar config: {err}"))?;
    Ok(())
}
