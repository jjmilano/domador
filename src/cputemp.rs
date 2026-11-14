//! Temperatura real del CPU (AMD Zen) vía PawnIO.
//!
//! El BIOS de la Legion Gen 6 no publica la temperatura del CPU por WMI
//! (`GetCPUTemp` devuelve 0). Herramientas como LibreHardwareMonitor la leen
//! del sensor interno del procesador (Tctl, registro SMN `0x00059800`) con el
//! driver firmado **PawnIO** y su módulo `AMDFamily17` (familias 17h–1Ah).
//!
//! Domador hace lo mismo y **sólo lee**: carga el módulo firmado, llama a
//! `ioctl_read_smn` y nada más. Si PawnIO no está instalado o el CPU no es
//! AMD Zen, devuelve `None` y la UI explica cómo habilitarlo.

use std::ffi::c_void;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const MODULE: &[u8] = include_bytes!("../assets/pawnio/AMDFamily17.bin");
const DEVICE: &str = r"\\?\GLOBALROOT\Device\PawnIO";
const DEVICE_TYPE: u32 = 41394 << 16;
const IOCTL_LOAD: u32 = DEVICE_TYPE | (0x821 << 2);
const IOCTL_EXECUTE: u32 = DEVICE_TYPE | (0x841 << 2);
const FN_NAME_LEN: usize = 32;
/// THM_TCON_CUR_TMP: temperatura de control (Tctl) del CPU.
const SMN_THM_TCON_CUR_TMP: u64 = 0x0005_9800;
/// Si falla, no reintentar en cada muestra: una vez por minuto alcanza.
const RETRY: Duration = Duration::from_secs(60);

struct Handle(HANDLE);
// Sólo lo usa un hilo a la vez (detrás del Mutex).
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct State {
    handle: Option<Handle>,
    last_error: Option<String>,
    failed_at: Option<Instant>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            handle: None,
            last_error: None,
            failed_at: None,
        })
    })
}

fn open() -> Result<Handle, String> {
    let path: Vec<u16> = DEVICE.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(if crate::win::is_admin() {
            "PawnIO no está instalado (winget install namazso.PawnIO)".into()
        } else {
            "leer la temperatura del CPU requiere Domador como administrador".into()
        });
    }
    let handle = Handle(raw);
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_LOAD,
            MODULE.as_ptr() as *const c_void,
            MODULE.len() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err("PawnIO no aceptó el módulo AMD (¿CPU no es AMD Zen?)".into());
    }
    Ok(handle)
}

fn read_smn(handle: &Handle, offset: u64) -> Option<u32> {
    let mut input = [0u8; FN_NAME_LEN + 8];
    let name = b"ioctl_read_smn";
    input[..name.len()].copy_from_slice(name);
    input[FN_NAME_LEN..].copy_from_slice(&offset.to_le_bytes());
    let mut output = [0u8; 8];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_EXECUTE,
            input.as_ptr() as *const c_void,
            input.len() as u32,
            output.as_mut_ptr() as *mut c_void,
            output.len() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 || returned < 8 {
        return None;
    }
    Some(u64::from_le_bytes(output) as u32)
}

/// Convierte el registro Tctl a °C (misma fórmula que LibreHardwareMonitor).
pub fn tctl_from_raw(raw: u32) -> f64 {
    let mut temp = ((raw >> 21) & 0x7FF) as f64 * 0.125;
    if raw & 0x0008_0000 != 0 {
        temp -= 49.0;
    }
    temp
}

/// Temperatura del CPU en °C, o `None` si no hay PawnIO/admin/AMD.
pub fn read() -> Option<f64> {
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    if state.handle.is_none() {
        if let Some(at) = state.failed_at {
            if at.elapsed() < RETRY {
                return None;
            }
        }
        match open() {
            Ok(handle) => {
                state.handle = Some(handle);
                state.last_error = None;
                state.failed_at = None;
            }
            Err(err) => {
                state.last_error = Some(err);
                state.failed_at = Some(Instant::now());
                return None;
            }
        }
    }
    let raw = state
        .handle
        .as_ref()
        .and_then(|h| read_smn(h, SMN_THM_TCON_CUR_TMP));
    match raw {
        Some(raw) => {
            let temp = tctl_from_raw(raw);
            (temp > 0.0 && temp < 125.0).then_some(temp)
        }
        None => {
            state.handle = None;
            state.last_error = Some("PawnIO dejó de responder; reintento en un minuto".into());
            state.failed_at = Some(Instant::now());
            None
        }
    }
}

/// Estado para la UI: de dónde sale la temperatura o por qué no hay.
pub fn source() -> serde_json::Value {
    let state = state().lock().unwrap_or_else(|p| p.into_inner());
    serde_json::json!({
        "source": if state.handle.is_some() { "pawnio" } else { "none" },
        "error": state.last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::tctl_from_raw;

    #[test]
    fn convierte_tctl() {
        // 65 °C → 520 pasos de 0,125 en los bits 21..31.
        assert_eq!(tctl_from_raw(520 << 21), 65.0);
        // Con el bit de rango -49 °C.
        assert_eq!(tctl_from_raw((520 << 21) | 0x0008_0000), 16.0);
    }
}
