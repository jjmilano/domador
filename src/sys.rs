//! Sampler del motor: un snapshot NT por tick para los procesos y `sysinfo`
//! sólo para CPU/RAM globales.
//!
//! El costo dominante del sampler anterior era recorrer ~500 procesos con
//! `sysinfo` (handles por proceso, rutas de exe, etc.). Acá los procesos salen
//! de una única llamada a `NtQuerySystemInformation` con buffer reutilizado.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use sysinfo::System;

use crate::model::ProcessRow;
use crate::nt::{ProcEntry, ProcessSnapshot};

/// Muestras de CPU que se guardan por proceso (historial del UI).
const HIST_LEN: usize = 24;
/// Unidades de 100 ns por segundo, para los tiempos del kernel.
const TICKS_PER_SECOND: f64 = 10_000_000.0;

pub struct Sampler {
    sys: System,
    snap: ProcessSnapshot,
    ncpu: usize,
    hist: HashMap<(u32, i64), VecDeque<f32>>,
    cpu_prev: HashMap<(u32, i64), (u64, Instant)>,
    rss: HashMap<u32, u64>,
}

impl Sampler {
    pub fn new() -> Self {
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let mut snap = ProcessSnapshot::new();
        // Primer snapshot: dimensiona el buffer y deja RSS disponible antes
        // del primer tick.
        let _ = snap.refresh();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_usage();
        Self {
            sys,
            snap,
            ncpu,
            hist: HashMap::new(),
            cpu_prev: HashMap::new(),
            rss: HashMap::new(),
        }
    }

    pub fn ncpu(&self) -> usize {
        self.ncpu
    }

    pub fn refresh(&mut self) -> Vec<ProcessRow> {
        let now = Instant::now();

        // Una sola consulta por tick. Si falla, se usan las entradas previas:
        // es preferible mostrar datos de hace un segundo a vaciar la lista.
        let _ = self.snap.refresh();

        let ncpu = self.ncpu as f64;
        let entries = self.snap.entries();
        let mut rows = Vec::with_capacity(entries.len());
        let mut alive: HashSet<(u32, i64)> = HashSet::with_capacity(entries.len());
        self.rss.clear();

        for entry in entries {
            let pid = entry.pid;
            if pid == 0 {
                continue;
            }
            let ct = entry.create_time;
            let key = (pid, (ct * 100.0).round() as i64);
            alive.insert(key);

            // CPU% = delta(user+kernel) / 10^7 / elapsed / ncpu * 100.
            // La primera muestra de cada proceso queda en 0.
            let cpu = match self.cpu_prev.insert(key, (entry.cpu_ticks, now)) {
                Some((prev_ticks, prev_at)) if entry.cpu_ticks >= prev_ticks => {
                    let dt = now.duration_since(prev_at).as_secs_f64().max(1e-6);
                    let delta = (entry.cpu_ticks - prev_ticks) as f64 / TICKS_PER_SECOND;
                    (delta / dt / ncpu * 100.0).clamp(0.0, 100.0) as f32
                }
                _ => 0.0,
            };

            let hist = self
                .hist
                .entry(key)
                .or_insert_with(|| VecDeque::with_capacity(HIST_LEN));
            hist.push_back(cpu);
            while hist.len() > HIST_LEN {
                hist.pop_front();
            }

            self.rss.insert(pid, entry.rss);
            rows.push(ProcessRow {
                pid,
                ct,
                name: entry.name.clone(),
                // El path completo no viene en el snapshot: el engine lo
                // resuelve con win::process_exe y lo cachea.
                exe: String::new(),
                cpu: (cpu as f64 * 10.0).round() / 10.0,
                hist: hist.iter().copied().collect(),
                rss: entry.rss,
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

        // Sin entradas muertas: ni historial ni tiempos previos crecen sin
        // límite cuando los procesos terminan.
        self.hist.retain(|key, _| alive.contains(key));
        self.cpu_prev.retain(|key, _| alive.contains(key));

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

    /// Entradas crudas del último snapshot (antes no se usaba desde el engine).
    pub fn processes(&self) -> &[ProcEntry] {
        self.snap.entries()
    }

    pub fn memory_of(&self, pid: u32) -> Option<u64> {
        self.rss.get(&pid).copied()
    }
}
