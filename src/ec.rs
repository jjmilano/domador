//! Lectura de la zona pública del Embedded Controller (espacio ACPI, 256
//! bytes del banco 0) vía PawnIO + módulo firmado `LpcACPIEC`.
//!
//! **Sólo lectura** de registros: usa el comando estándar ACPI "read" (0x80)
//! por los puertos 0x66/0x62. No escribe en ningún registro del EC ni cambia
//! de banco. Toma el mutex `Global\Access_EC`, el mismo que usan Windows,
//! LibreHardwareMonitor y otras herramientas, para no pisarse.

use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use crate::pawnio::Module;

const MODULE: &[u8] = include_bytes!("../assets/pawnio/LpcACPIEC.bin");
const PORT_CMD: u64 = 0x66;
const PORT_DATA: u64 = 0x62;
const CMD_READ: u64 = 0x80;
const IBF: u8 = 0x02;
const OBF: u8 = 0x01;
const RETRY: Duration = Duration::from_secs(60);

struct State {
    module: Option<Module>,
    error: Option<String>,
    failed_at: Option<Instant>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            module: None,
            error: None,
            failed_at: None,
        })
    })
}

fn inb(m: &Module, port: u64) -> Option<u8> {
    m.execute("ioctl_pio_read", &[port], 1)
        .and_then(|v| v.first().copied())
        .map(|v| v as u8)
}

fn outb(m: &Module, port: u64, value: u64) -> Option<()> {
    m.execute("ioctl_pio_write", &[port, value], 0).map(|_| ())
}

fn wait(m: &Module, bit: u8, set: bool) -> bool {
    for i in 0..60 {
        if let Some(status) = inb(m, PORT_CMD) {
            if ((status & bit) != 0) == set {
                return true;
            }
        }
        if i > 5 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    false
}

fn read_byte(m: &Module, register: u8) -> Option<u8> {
    for _ in 0..3 {
        if !wait(m, IBF, false) {
            continue;
        }
        outb(m, PORT_CMD, CMD_READ)?;
        if !wait(m, IBF, false) {
            continue;
        }
        outb(m, PORT_DATA, register as u64)?;
        // Algunos EC no levantan OBF a tiempo: como LibreHardwareMonitor,
        // si OBF no aparece se acepta IBF libre como "dato listo".
        if !wait(m, OBF, true) && !wait(m, IBF, false) {
            continue;
        }
        return inb(m, PORT_DATA);
    }
    None
}

/// Guarda del mutex global del EC.
struct EcLock(windows_sys::Win32::Foundation::HANDLE);

impl EcLock {
    fn acquire() -> Option<Self> {
        let name: Vec<u16> = "Global\\Access_EC".encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        let wait = unsafe { WaitForSingleObject(handle, 200) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Some(EcLock(handle))
        } else {
            unsafe {
                CloseHandle(handle);
            }
            None
        }
    }
}

impl Drop for EcLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

fn with_module<T>(f: impl FnOnce(&Module) -> Option<T>) -> Result<T, String> {
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    if st.module.is_none() {
        if st.failed_at.map(|at| at.elapsed() < RETRY).unwrap_or(false) {
            return Err(st.error.clone().unwrap_or_default());
        }
        match Module::load(MODULE) {
            Ok(m) => {
                st.module = Some(m);
                st.error = None;
                st.failed_at = None;
            }
            Err(err) => {
                st.error = Some(err.clone());
                st.failed_at = Some(Instant::now());
                return Err(err);
            }
        }
    }
    let _lock = EcLock::acquire().ok_or("el EC está ocupado (Access_EC)")?;
    let module = st.module.as_ref().expect("módulo cargado");
    f(module).ok_or_else(|| "el EC no respondió a tiempo".to_string())
}

/// Lee registros sueltos del banco 0. Un registro que no responde queda en
/// `None`; no corta el resto.
pub fn read(registers: &[u8]) -> Result<Vec<Option<u8>>, String> {
    with_module(|m| Some(registers.iter().map(|r| read_byte(m, *r)).collect()))
}

/// Volcado completo 0x00..=0xFE (diagnóstico).
pub fn dump() -> Result<Vec<Option<u8>>, String> {
    let regs: Vec<u8> = (0u8..=0xFE).collect();
    read(&regs)
}
