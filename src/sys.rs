use std::collections::{HashMap, VecDeque};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::model::ProcessRow;

pub struct Sampler {
    sys: System,
    ncpu: usize,
    hist: HashMap<(u32, i64), VecDeque<f32>>,
}

impl Sampler {
    pub fn new() -> Self {
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut sys = System::new_all();
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet);
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        Self {
            sys,
            ncpu,
            hist: HashMap::new(),
        }
    }

    pub fn ncpu(&self) -> usize {
        self.ncpu
    }

    pub fn refresh(&mut self) -> Vec<ProcessRow> {
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet);
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let ncpu = self.ncpu as f64;
        let mut rows = Vec::with_capacity(self.sys.processes().len());
        let mut seen = Vec::with_capacity(self.sys.processes().len());

        for (pid, process) in self.sys.processes() {
            let pid_u32 = pid.as_u32();
            if pid_u32 == 0 {
                continue;
            }
            let ct = process.start_time() as f64;
            let key = (pid_u32, (ct * 100.0).round() as i64);
            seen.push(key);
            let cpu = (process.cpu_usage() as f64 / ncpu).clamp(0.0, 100.0);
            let hist = self
                .hist
                .entry(key)
                .or_insert_with(|| VecDeque::with_capacity(24));
            hist.push_back(cpu as f32);
            if hist.len() > 24 {
                hist.pop_front();
            }
            let name = process.name().to_string_lossy().to_string();
            let exe = process
                .exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            rows.push(ProcessRow {
                pid: pid_u32,
                ct,
                name,
                exe,
                cpu: (cpu * 10.0).round() / 10.0,
                hist: hist.iter().copied().collect(),
                rss: process.memory(),
                nice: None,
                prio: None,
                prio_label: "—".into(),
                affinity: None,
                affinity_label: "—".into(),
                eco: false,
                exact: false,
                applied: false,
                pb: false,
                protected: false,
                rule_id: None,
                rule: None,
            });
        }

        for key in self.hist.keys() {
            if !seen.contains(key) {
                // se limpia en el siguiente prune para no recorrer dos veces
            }
        }
        if self.hist.len() > 1200 {
            let alive: std::collections::HashSet<_> = seen.into_iter().collect();
            self.hist.retain(|k, _| alive.contains(k));
        }

        rows.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        rows
    }

    pub fn system(&mut self) -> (f32, Vec<f32>, u64, u64, f64) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu = self.sys.global_cpu_usage();
        let percpu: Vec<f32> = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        let total = self.sys.total_memory();
        let used = self.sys.used_memory();
        let ram = if total > 0 {
            used as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        (cpu, percpu, used, total, ram)
    }

    pub fn processes(&self) -> impl Iterator<Item = &sysinfo::Process> {
        self.sys.processes().values()
    }

    pub fn memory_of(&self, pid: u32) -> Option<u64> {
        self.sys
            .process(sysinfo::Pid::from_u32(pid))
            .map(|process| process.memory())
    }
}
