use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::config;
use crate::model::*;
use crate::power;
use crate::sys::Sampler;
use crate::win;

type Key = (u32, i64);
type KeySet = HashSet<Key>;

const MB: u64 = 1024 * 1024;

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
    pub plans: Vec<PowerPlan>,
    pub active_plan: Option<String>,
    pub profile: ProfileState,
    profile_applied: Vec<u32>,
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
    pub dirty: bool,
}

impl Engine {
    pub fn new(config_path: PathBuf) -> Self {
        let config = config::load(&config_path);
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
            plans: Vec::new(),
            active_plan: None,
            profile: ProfileState::default(),
            profile_applied: Vec::new(),
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
        engine
    }

    pub fn save(&mut self) {
        self.config.profile = self.profile.clone();
        if let Err(err) = config::save(&self.config_path, &self.config) {
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

        for row in rows.iter_mut() {
            let rule = self.match_rule(row);
            row.rule_id = rule.as_ref().map(|r| r.id.clone());
            row.rule = rule;
            row.protected = self
                .config
                .probalance
                .protect
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&row.name));
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
            ram,
            ram_used: used,
            ram_total: total,
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

        if self.config.probalance.enabled && self.config.probalance.skip_interactive {
            self.visible = win::visible_window_pids();
        } else {
            self.visible.clear();
        }

        self.prune();
        self.apply_new_rules();
        self.pb_step();
        self.fg_step();
        self.watchdog_step();
        self.trim_step();
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

    fn match_rule(&mut self, row: &ProcessRow) -> Option<Rule> {
        let rules = self.config.rules.clone();
        for rule in rules {
            if !rule.enabled || rule.pattern.trim().is_empty() {
                continue;
            }
            if self.rule_matches(&rule, row) {
                return Some(rule);
            }
        }
        None
    }

    fn rule_matches(&mut self, rule: &Rule, row: &ProcessRow) -> bool {
        let pattern = rule.pattern.trim();
        if pattern.contains('\\') || pattern.contains('/') {
            let normalized = pattern.replace('/', "\\");
            let base = normalized.rsplit('\\').next().unwrap_or(&normalized);
            if !wildcard(base, &row.name) {
                return false;
            }
            let mut exe = row.exe.clone();
            if exe.is_empty() {
                if let Some(cached) = self.exe_cache.get(&row.key()) {
                    exe = cached.clone();
                } else if let Some(found) = win::process_exe(row.pid) {
                    self.exe_cache.insert(row.key(), found.clone());
                    exe = found;
                }
            }
            !exe.is_empty() && wildcard(&normalized, &exe)
        } else {
            wildcard(pattern, &row.name)
        }
    }

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
                Err(err) => self.log(
                    "warn",
                    format!(
                        "la regla «{}» falló en {} (PID {}): {}",
                        rule.pattern, name, pid, err
                    ),
                ),
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
        let before = self.sampler.memory_of(pid).unwrap_or(0);
        win::trim_working_set(pid)?;
        let after = self.sampler.memory_of(pid).unwrap_or(0);
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
        let mut freed = 0u64;
        let mut count = 0u32;
        for (pid, _) in targets {
            if let Ok(bytes) = self.trim(pid) {
                if bytes > 0 {
                    freed += bytes;
                    count += 1;
                }
            }
        }
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
            if nice <= win::priority_class_from_key("below_normal").unwrap() {
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
        if original <= win::priority_class_from_key("below_normal").unwrap() {
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
        if nice >= target {
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
                sorted.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
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
                    .map(|c| c < class)
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
        }
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
