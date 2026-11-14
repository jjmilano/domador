//! Acceso mínimo al driver firmado PawnIO: abrir el dispositivo, cargar un
//! módulo firmado y ejecutar sus funciones. Cada módulo vive en su propio
//! handle (el driver asocia el módulo al handle).

use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const DEVICE: &str = r"\\?\GLOBALROOT\Device\PawnIO";
const DEVICE_TYPE: u32 = 41394 << 16;
const IOCTL_LOAD: u32 = DEVICE_TYPE | (0x821 << 2);
const IOCTL_EXECUTE: u32 = DEVICE_TYPE | (0x841 << 2);
const FN_NAME_LEN: usize = 32;

pub struct Module(HANDLE);

// El handle sólo se usa detrás de un Mutex del llamador.
unsafe impl Send for Module {}

impl Drop for Module {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

impl Module {
    /// Abre PawnIO y carga `blob` (un módulo firmado `.bin`).
    pub fn load(blob: &[u8]) -> Result<Self, String> {
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
                "PawnIO requiere Domador como administrador".into()
            });
        }
        let module = Module(raw);
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                module.0,
                IOCTL_LOAD,
                blob.as_ptr() as *const c_void,
                blob.len() as u32,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err("PawnIO no aceptó el módulo".into());
        }
        Ok(module)
    }

    /// Ejecuta `name` con `input` y devuelve hasta `out_len` valores.
    pub fn execute(&self, name: &str, input: &[u64], out_len: usize) -> Option<Vec<u64>> {
        let mut buf = vec![0u8; FN_NAME_LEN + input.len() * 8];
        let bytes = name.as_bytes();
        let n = bytes.len().min(FN_NAME_LEN - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        for (i, value) in input.iter().enumerate() {
            let at = FN_NAME_LEN + i * 8;
            buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        let mut out = vec![0u8; out_len.max(1) * 8];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_EXECUTE,
                buf.as_ptr() as *const c_void,
                buf.len() as u32,
                out.as_mut_ptr() as *mut c_void,
                (out_len * 8) as u32,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let count = (returned as usize / 8).min(out_len);
        Some(
            (0..count)
                .map(|i| u64::from_le_bytes(out[i * 8..i * 8 + 8].try_into().unwrap()))
                .collect(),
        )
    }
}
