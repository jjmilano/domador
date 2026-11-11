use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const APP_NAME: &str = "Domador";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const DEFAULT_PROTECT: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "secure system",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "smss.exe",
    "winlogon.exe",
    "dwm.exe",
    "audiodg.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "ctfmon.exe",
    "processlasso.exe",
    "processgovernor.exe",
];

pub const AI_PATTERNS: &[&str] = &[
    "hermes agent.exe",
    "hermes.exe",
    "claude.exe",
    "codex.exe",
    "codex-code-mode-host.exe",
    "opencode.exe",
    "gemini.exe",
    "cursor.exe",
    "code.exe",
    "windowsterminal.exe",
];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub interval: f64,
    pub rules: Vec<Rule>,
    pub probalance: ProBalance,
    pub foreground: Foreground,
    pub trim: Trim,
    pub power: PowerCfg,
    pub ai: AiConfig,
    pub profile: ProfileState,
    pub ui: UiCfg,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            interval: 1.0,
            rules: Vec::new(),
            probalance: ProBalance::default(),
            foreground: Foreground::default(),
            trim: Trim::default(),
            power: PowerCfg::default(),
            ai: AiConfig::default(),
            profile: ProfileState::default(),
            ui: UiCfg::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub pattern: String,
    pub priority: Option<String>,
    pub affinity: Option<Vec<u32>>,
    pub eco: Option<bool>,
    pub io: Option<String>,
    pub mem: Option<String>,
    pub watchdog: Watchdog,
    pub note: String,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            pattern: String::new(),
            priority: None,
            affinity: None,
            eco: None,
            io: None,
            mem: None,
            watchdog: Watchdog::default(),
            note: String::new(),
            enabled: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Watchdog {
    pub keep_alive: bool,
    pub max_instances: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ProBalance {
    pub enabled: bool,
    pub threshold: f64,
    pub sustain_s: u32,
    pub restore_below: f64,
    pub restore_s: u32,
    pub priority: String,
    pub eco: bool,
    pub skip_foreground: bool,
    pub skip_interactive: bool,
    pub cooldown_s: u32,
    pub protect: Vec<String>,
}

impl Default for ProBalance {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 18.0,
            sustain_s: 5,
            restore_below: 4.0,
            restore_s: 8,
            priority: "below_normal".into(),
            eco: true,
            skip_foreground: true,
            skip_interactive: true,
            cooldown_s: 30,
            protect: DEFAULT_PROTECT.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Foreground {
    pub enabled: bool,
    pub priority: String,
}

impl Default for Foreground {
    fn default() -> Self {
        Self {
            enabled: false,
            priority: "above_normal".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Trim {
    pub enabled: bool,
    pub threshold: f64,
    pub min_mb: u32,
    pub interval_s: u32,
}

impl Default for Trim {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 85.0,
            min_mb: 250,
            interval_s: 300,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct PowerCfg {
    pub auto: bool,
    pub trigger: String,
    pub base_plan: String,
    pub game_plan: String,
}

impl Default for PowerCfg {
    fn default() -> Self {
        Self {
            auto: false,
            trigger: String::new(),
            base_plan: String::new(),
            game_plan: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct AiConfig {
    pub patterns: Vec<String>,
    pub priority: String,
    pub eco: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            patterns: AI_PATTERNS.iter().map(|s| s.to_string()).collect(),
            priority: "above_normal".into(),
            eco: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ProfileState {
    pub active: String,
    pub plan: String,
    pub target: Option<String>,
    pub since: f64,
    pub previous_plan: String,
    pub guid: String,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self {
            active: "balanced".into(),
            plan: String::new(),
            target: None,
            since: 0.0,
            previous_plan: String::new(),
            guid: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct UiCfg {
    pub poll_ms: u64,
    pub sort: String,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            poll_ms: 2000,
            sort: "cpu".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ProcessRow {
    pub pid: u32,
    pub ct: f64,
    pub name: String,
    pub exe: String,
    pub cpu: f64,
    pub hist: Vec<f32>,
    pub rss: u64,
    pub nice: Option<u32>,
    pub prio: Option<String>,
    pub prio_label: String,
    pub affinity: Option<Vec<u32>>,
    pub affinity_label: String,
    pub eco: bool,
    pub exact: bool,
    pub applied: bool,
    pub pb: bool,
    pub protected: bool,
    pub rule_id: Option<String>,
    #[serde(skip)]
    pub rule: Option<Rule>,
}

impl ProcessRow {
    pub fn key(&self) -> (u32, i64) {
        (self.pid, (self.ct * 100.0).round() as i64)
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct TickStats {
    pub p50: f64,
    pub p95: f64,
    pub last: f64,
    pub n: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub admin: bool,
    pub pid: u32,
    pub uptime: f64,
    pub config_path: String,
    pub ncpu: usize,
    pub tick_ms: TickStats,
    pub visible: usize,
    pub eco_supported: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct SystemInfo {
    pub cpu: f32,
    pub percpu: Vec<f32>,
    pub ram: f64,
    pub ram_used: u64,
    pub ram_total: u64,
    pub foreground: Option<u32>,
    pub foreground_name: Option<String>,
    pub last_tick: f64,
}

impl Default for SystemInfo {
    fn default() -> Self {
        Self {
            cpu: 0.0,
            percpu: Vec::new(),
            ram: 0.0,
            ram_used: 0,
            ram_total: 0,
            foreground: None,
            foreground_name: None,
            last_tick: 0.0,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct HistoryPoint {
    pub t: f64,
    pub cpu: f32,
    pub ram: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct LogEntry {
    pub ts: f64,
    pub level: String,
    pub msg: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Counts {
    pub processes: usize,
    pub rules: usize,
    pub throttled: usize,
    pub applied: usize,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct PowerPlan {
    pub guid: String,
    pub name: String,
    pub active: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct PowerInfo {
    pub auto: bool,
    pub trigger: String,
    pub base_plan: String,
    pub game_plan: String,
    pub plans: Vec<PowerPlan>,
    pub active: Option<String>,
    pub active_name: String,
    pub ac_line: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub app: AppInfo,
    pub system: SystemInfo,
    pub processes: Vec<ProcessRow>,
    pub rules: Vec<Rule>,
    pub probalance: ProBalance,
    pub power: PowerInfo,
    pub foreground: Foreground,
    pub trim: Trim,
    pub history: Vec<HistoryPoint>,
    pub log: Vec<LogEntry>,
    pub counts: Counts,
    pub profile: ProfileState,
}

#[derive(Serialize, Clone, Debug)]
pub struct MetaRow {
    pub pid: u32,
    pub ct: f64,
    pub nice: u32,
    pub prio: Option<String>,
    pub prio_label: String,
    pub affinity: Vec<u32>,
    pub affinity_label: String,
    pub eco: bool,
    pub exe: String,
}

pub type Deque<T> = VecDeque<T>;
