//! Snapshot de procesos con una única llamada a `NtQuerySystemInformation`.
//!
//! `SystemProcessInformation` (clase 5) devuelve una lista enlazada de
//! `SYSTEM_PROCESS_INFORMATION` dentro de un buffer propio. El layout de abajo
//! es el de Windows 10/11 x64 (256 bytes) y está validado en tiempo de
//! compilación con `offset_of!`.
//!
//! No se abre ningún handle de proceso: los nombres salen de `ImageName`
//! (UTF-16) y los tiempos de CPU de `UserTime`/`KernelTime` acumulados.

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::ptr;

/// Clase de información `SystemProcessInformation`.
const SYSTEM_PROCESS_INFORMATION_CLASS: i32 = 5;
/// `STATUS_INFO_LENGTH_MISMATCH`: el buffer era chico.
const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
/// Intentos máximos de crecimiento del buffer.
const MAX_ATTEMPTS: usize = 8;
/// Tamaño inicial del buffer (se reutiliza entre ticks).
const INITIAL_BYTES: usize = 1 << 20;
/// Margen extra al crecer, para no repetir la llamada por unos pocos bytes.
const GROWTH_SLACK: usize = 64 * 1024;
/// Segundos entre 1601-01-01 (FILETIME) y 1970-01-01 (Unix).
const FILETIME_UNIX_EPOCH_OFFSET: i64 = 11_644_473_600;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        system_information_class: i32,
        system_information: *mut c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// Copia del layout nativo. Todos los campos son necesarios: los offsets
/// dependen de la posición, aunque sólo se lean unos pocos.
#[repr(C)]
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<SystemProcessInformation>() == 256);
    assert!(offset_of!(SystemProcessInformation, create_time) == 32);
    assert!(offset_of!(SystemProcessInformation, image_name) == 56);
    assert!(offset_of!(SystemProcessInformation, base_priority) == 72);
    assert!(offset_of!(SystemProcessInformation, unique_process_id) == 80);
    assert!(offset_of!(SystemProcessInformation, handle_count) == 96);
    assert!(offset_of!(SystemProcessInformation, working_set_size) == 144);
    assert!(offset_of!(SystemProcessInformation, other_transfer_count) == 248);
};

/// Una fila cruda del snapshot del kernel.
#[derive(Clone, Debug, Default)]
pub struct ProcEntry {
    pub pid: u32,
    pub name: String,
    /// `CreateTime` convertido a segundos epoch Unix (0 si no vino).
    pub create_time: f64,
    /// `UserTime + KernelTime` en unidades de 100 ns (acumulado).
    pub cpu_ticks: u64,
    /// `WorkingSetSize` en bytes.
    pub rss: u64,
    /// `PrivatePageCount` (páginas) convertido a bytes.
    pub private_bytes: u64,
    pub threads: u32,
    pub handles: u32,
    pub base_priority: i32,
}

/// Buffer reutilizable + parser del snapshot.
pub struct ProcessSnapshot {
    /// `Vec<u64>` sólo para garantizar alineación de 8 bytes.
    buf: Vec<u64>,
    used: usize,
    entries: Vec<ProcEntry>,
    scratch: Vec<ProcEntry>,
    name_scratch: Vec<u16>,
}

impl ProcessSnapshot {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            used: 0,
            entries: Vec::new(),
            scratch: Vec::new(),
            name_scratch: Vec::new(),
        }
    }

    /// Entradas del último snapshot válido. Si la consulta falla, quedan las
    /// del tick anterior en vez de dejar el motor sin procesos.
    pub fn entries(&self) -> &[ProcEntry] {
        &self.entries
    }

    /// Consulta el snapshot completo. Una sola llamada NT cuando el buffer ya
    /// tiene tamaño suficiente; sólo reintenta si el kernel pide más espacio.
    pub fn refresh(&mut self) -> Result<(), String> {
        let mut bytes = if self.buf.is_empty() {
            INITIAL_BYTES
        } else {
            self.buf.len() * size_of::<u64>()
        };
        let mut attempts = 0usize;
        let needed = loop {
            attempts += 1;
            if attempts > MAX_ATTEMPTS {
                return Err(format!(
                    "NtQuerySystemInformation: buffer insuficiente ({bytes} bytes)"
                ));
            }
            self.ensure(bytes);
            let mut return_length = 0u32;
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION_CLASS,
                    self.buf.as_mut_ptr().cast(),
                    bytes as u32,
                    &mut return_length,
                )
            };
            let code = status as u32;
            if code == 0 {
                break return_length;
            }
            if code == STATUS_INFO_LENGTH_MISMATCH {
                let wanted = (return_length as usize)
                    .saturating_add(GROWTH_SLACK)
                    .max(bytes.saturating_mul(2));
                bytes = wanted.min(u32::MAX as usize);
                continue;
            }
            return Err(format!("NtQuerySystemInformation: NTSTATUS 0x{code:08X}"));
        };

        let total_bytes = self.buf.len() * size_of::<u64>();
        self.used = if (needed as usize) >= size_of::<SystemProcessInformation>()
            && (needed as usize) <= total_bytes
        {
            needed as usize
        } else {
            total_bytes
        };

        self.scratch.clear();
        let parsed = self.walk();
        std::mem::swap(&mut self.entries, &mut self.scratch);
        parsed
    }

    fn ensure(&mut self, bytes: usize) {
        let units = bytes.div_ceil(size_of::<u64>());
        if self.buf.len() < units {
            self.buf.resize(units, 0);
        }
    }

    fn walk(&mut self) -> Result<(), String> {
        let record = size_of::<SystemProcessInformation>();
        let total = self.used.min(self.buf.len() * size_of::<u64>());
        if total < record {
            return Err("snapshot de procesos vacío".into());
        }
        let base = self.buf.as_ptr() as usize;
        let end = base.saturating_add(total);
        let mut offset = 0usize;

        loop {
            if offset.saturating_add(record) > total {
                break;
            }
            // `read_unaligned`: el buffer es un `Vec<u64>` alineado, pero el
            // compilador no puede saberlo a través del índice en bytes.
            let raw = unsafe {
                ptr::read_unaligned(
                    (base as *const u8)
                        .add(offset)
                        .cast::<SystemProcessInformation>(),
                )
            };
            let pid = raw.unique_process_id as u32;
            let name = read_image_name(&raw.image_name, base, end, pid, &mut self.name_scratch);
            self.scratch.push(ProcEntry {
                pid,
                name,
                create_time: unix_seconds_from_filetime(raw.create_time),
                cpu_ticks: (raw.user_time.max(0) as u64)
                    .saturating_add(raw.kernel_time.max(0) as u64),
                rss: raw.working_set_size as u64,
                // `PagefileUsage` es el commit privado del proceso (lo mismo
                // que psutil llama `private`), en bytes.
                private_bytes: raw.pagefile_usage as u64,
                threads: raw.number_of_threads,
                handles: raw.handle_count,
                base_priority: raw.base_priority,
            });

            let next = raw.next_entry_offset as usize;
            if next == 0 {
                break;
            }
            // Un offset menor al registro sería un bucle infinito.
            if next < record {
                break;
            }
            match offset.checked_add(next) {
                Some(value) if value < total => offset = value,
                _ => break,
            }
        }
        Ok(())
    }
}

impl Default for ProcessSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

fn read_image_name(
    image: &UnicodeString,
    base: usize,
    end: usize,
    pid: u32,
    scratch: &mut Vec<u16>,
) -> String {
    let fallback = |pid: u32| format!("PID {pid}");
    let length = image.length as usize;
    let start = image.buffer as usize;
    if image.buffer.is_null() || length < 2 {
        return fallback(pid);
    }
    // El nombre tiene que caer dentro del buffer del snapshot: si el layout
    // fuera incorrecto, esto evita leer memoria ajena.
    if start < base || length > end.saturating_sub(start) {
        return fallback(pid);
    }
    let units = length / 2;
    scratch.clear();
    scratch.reserve(units);
    for index in 0..units {
        scratch.push(unsafe { ptr::read_unaligned((start as *const u16).add(index)) });
    }
    if let Some(nul) = scratch.iter().position(|&unit| unit == 0) {
        scratch.truncate(nul);
    }
    if scratch.is_empty() {
        return fallback(pid);
    }
    String::from_utf16_lossy(scratch)
}

fn unix_seconds_from_filetime(filetime: i64) -> f64 {
    if filetime <= 0 {
        return 0.0;
    }
    (filetime as f64 / 10_000_000.0 - FILETIME_UNIX_EPOCH_OFFSET as f64).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn snapshot_lists_current_process() {
        let mut snapshot = ProcessSnapshot::new();
        snapshot.refresh().expect("snapshot del kernel");
        let entries = snapshot.entries();
        assert!(
            entries.len() > 20,
            "muy pocos procesos en el snapshot: {}",
            entries.len()
        );
        let me = std::process::id();
        let own = entries
            .iter()
            .find(|entry| entry.pid == me)
            .expect("el proceso de test no aparece en el snapshot");
        assert!(!own.name.is_empty(), "nombre vacío para el PID propio");
        assert!(own.rss > 0, "working set en cero para el PID propio");
        assert!(
            entries.iter().any(|entry| entry.pid == 4),
            "falta el proceso System (PID 4)"
        );

        let started = Instant::now();
        snapshot.refresh().expect("segundo snapshot");
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "snapshot: {} procesos en {ms:.2} ms (buffer reutilizado)",
            snapshot.entries().len()
        );
    }
}
