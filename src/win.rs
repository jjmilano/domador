use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    CloseHandle, BOOL, FALSE, FILETIME, HANDLE, HWND, LPARAM, TRUE,
};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessInformation, GetProcessTimes, OpenProcess,
    QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask, SetProcessInformation,
    TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

#[repr(C)]
struct PowerThrottlingState {
    version: u32,
    control_mask: u32,
    state_mask: u32,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationProcess(
        process_handle: HANDLE,
        process_information_class: u32,
        process_information: *mut c_void,
        process_information_length: u32,
    ) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn IsUserAnAdmin() -> BOOL;
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

fn open(pid: u32, access: u32) -> Option<HANDLE> {
    let handle = unsafe { OpenProcess(access, FALSE, pid) };
    if handle.is_null() {
        None
    } else {
        Some(handle)
    }
}

fn close(handle: HANDLE) {
    unsafe {
        CloseHandle(handle);
    }
}

pub fn priority_class_from_key(key: &str) -> Option<u32> {
    Some(match key {
        "idle" => IDLE_PRIORITY_CLASS,
        "below_normal" => BELOW_NORMAL_PRIORITY_CLASS,
        "normal" => NORMAL_PRIORITY_CLASS,
        "above_normal" => ABOVE_NORMAL_PRIORITY_CLASS,
        "high" => HIGH_PRIORITY_CLASS,
        "realtime" => REALTIME_PRIORITY_CLASS,
        _ => return None,
    })
}

/// Orden lógico de las clases de prioridad. Las constantes de Windows no
/// están ordenadas numéricamente (Below Normal = 16384, High = 128), así que
/// comparar los valores crudos da resultados incorrectos.
pub fn priority_rank(class: u32) -> u8 {
    match class {
        IDLE_PRIORITY_CLASS => 0,
        BELOW_NORMAL_PRIORITY_CLASS => 1,
        NORMAL_PRIORITY_CLASS => 2,
        ABOVE_NORMAL_PRIORITY_CLASS => 3,
        HIGH_PRIORITY_CLASS => 4,
        REALTIME_PRIORITY_CLASS => 5,
        _ => 2,
    }
}

pub fn priority_key(class: u32) -> Option<String> {
    Some(
        match class {
            IDLE_PRIORITY_CLASS => "idle",
            BELOW_NORMAL_PRIORITY_CLASS => "below_normal",
            NORMAL_PRIORITY_CLASS => "normal",
            ABOVE_NORMAL_PRIORITY_CLASS => "above_normal",
            HIGH_PRIORITY_CLASS => "high",
            REALTIME_PRIORITY_CLASS => "realtime",
            _ => return None,
        }
        .to_string(),
    )
}

pub fn priority_label(class: u32) -> String {
    match priority_key(class).as_deref() {
        Some("idle") => "Idle".into(),
        Some("below_normal") => "Bajo".into(),
        Some("normal") => "Normal".into(),
        Some("above_normal") => "Alto".into(),
        Some("high") => "Muy alto".into(),
        Some("realtime") => "Tiempo real".into(),
        _ => format!("0x{class:X}"),
    }
}

pub fn get_priority_class(pid: u32) -> Option<u32> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let class = unsafe { GetPriorityClass(handle) };
    close(handle);
    if class == 0 {
        None
    } else {
        Some(class)
    }
}

pub fn set_priority_class(pid: u32, class: u32) -> Result<(), String> {
    let handle = open(pid, PROCESS_SET_INFORMATION).ok_or_else(|| last_error("OpenProcess"))?;
    let ok = unsafe { SetPriorityClass(handle, class) };
    let err = last_error("SetPriorityClass");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

pub fn get_affinity(pid: u32) -> Option<Vec<u32>> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    let ok = unsafe { GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask) };
    close(handle);
    if ok == 0 {
        return None;
    }
    let mut cpus = Vec::new();
    for bit in 0..usize::BITS {
        if process_mask & (1usize << bit) != 0 {
            cpus.push(bit);
        }
    }
    Some(cpus)
}

pub fn set_affinity(pid: u32, cpus: &[u32]) -> Result<(), String> {
    if cpus.is_empty() {
        return Err("Elegí al menos un núcleo".into());
    }
    if cpus.len() > usize::BITS as usize {
        return Err("Demasiados núcleos".into());
    }
    let mut mask: usize = 0;
    for cpu in cpus {
        if *cpu >= usize::BITS {
            return Err(format!("Núcleo fuera de rango: {cpu}"));
        }
        mask |= 1usize << cpu;
    }
    let handle = open(pid, PROCESS_SET_INFORMATION).ok_or_else(|| last_error("OpenProcess"))?;
    let ok = unsafe { SetProcessAffinityMask(handle, mask) };
    let err = last_error("SetProcessAffinityMask");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

pub fn get_eco(pid: u32) -> Option<bool> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut state = PowerThrottlingState {
        version: 1,
        control_mask: 0,
        state_mask: 0,
    };
    let ok = unsafe {
        GetProcessInformation(
            handle,
            4,
            &mut state as *mut _ as *mut c_void,
            std::mem::size_of::<PowerThrottlingState>() as u32,
        )
    };
    close(handle);
    if ok == 0 {
        None
    } else {
        Some(state.state_mask & 1 != 0)
    }
}

pub fn set_eco(pid: u32, enabled: bool) -> Result<(), String> {
    let handle = open(pid, PROCESS_SET_INFORMATION).ok_or_else(|| last_error("OpenProcess"))?;
    let state = PowerThrottlingState {
        version: 1,
        control_mask: 1,
        state_mask: if enabled { 1 } else { 0 },
    };
    let ok = unsafe {
        SetProcessInformation(
            handle,
            4,
            &state as *const _ as *const c_void,
            std::mem::size_of::<PowerThrottlingState>() as u32,
        )
    };
    let err = last_error("SetProcessInformation(EcoQoS)");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

pub fn set_memory_priority(pid: u32, level: u32) -> Result<(), String> {
    let handle = open(pid, PROCESS_SET_INFORMATION).ok_or_else(|| last_error("OpenProcess"))?;
    let value = level;
    let ok = unsafe {
        SetProcessInformation(
            handle,
            0,
            &value as *const _ as *const c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    let err = last_error("SetProcessInformation(memoria)");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

pub fn set_io_priority(pid: u32, level: u32) -> Result<(), String> {
    let handle = open(pid, PROCESS_SET_INFORMATION).ok_or_else(|| last_error("OpenProcess"))?;
    let mut value = level;
    let status = unsafe {
        NtSetInformationProcess(
            handle,
            33,
            &mut value as *mut _ as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    close(handle);
    if status != 0 {
        Err(format!("NTSTATUS 0x{:08X}", status as u32))
    } else {
        Ok(())
    }
}

pub fn trim_working_set(pid: u32) -> Result<(), String> {
    let handle = open(pid, PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION)
        .ok_or_else(|| last_error("OpenProcess"))?;
    let ok = unsafe { EmptyWorkingSet(handle) };
    let err = last_error("EmptyWorkingSet");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

/// Working set y memoria privada de un proceso sin pasar por el sampler.
///
/// Sirve para medir el efecto de un trim sin disparar un snapshot NT completo
/// (que además ensuciaría los deltas de CPU del tick siguiente).
pub fn process_memory(pid: u32) -> Option<(u64, u64)> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA)?;
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe {
        GetProcessMemoryInfo(
            handle,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )
    };
    close(handle);
    if ok == 0 {
        None
    } else {
        Some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
    }
}

pub fn terminate_process(pid: u32) -> Result<(), String> {
    let handle = open(pid, PROCESS_TERMINATE).ok_or_else(|| last_error("OpenProcess"))?;
    let ok = unsafe { TerminateProcess(handle, 1) };
    let err = last_error("TerminateProcess");
    close(handle);
    if ok == 0 {
        Err(err)
    } else {
        Ok(())
    }
}

pub fn process_exe(pid: u32) -> Option<String> {
    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buffer = [0u16; 2048];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    close(handle);
    if ok == 0 || size == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

// ------------------------------------------------------ ficha de un proceso
//
// Todo lo que la UI muestra en el modal de detalle, resuelto para un solo PID
// y bajo demanda: metadata del ejecutable (GetFileVersionInfo), padre e
// instancia (Toolhelp + GetProcessTimes), ventana visible (EnumWindows),
// línea de comandos (NtQueryInformationProcess) y firma Authenticode
// (WinVerifyTrust, embebida o por catálogo de Windows).
//
// Nada de esto corre en el tick: el handler de la API lo llama fuera del lock
// del motor, así que un `WinVerifyTrust` lento no puede atrasar el sampler.

/// El PID no está: o nunca existió (404) o terminó mientras lo consultábamos
/// (410). El handler traduce cada variante a su status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailError {
    NotFound,
    Vanished,
}

/// Ficha de un proceso. Los `Option` son honestos: `None` significa "no se
/// pudo leer", no "vacío". Un proceso protegido (SYSTEM, PPL) devuelve la
/// ficha parcial con `opened == false`.
#[derive(Clone, Debug, Default)]
pub struct ProcessDetail {
    pub pid: u32,
    pub name: String,
    pub path: Option<String>,
    pub description: Option<String>,
    pub company: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub parent_pid: Option<u32>,
    pub parent: Option<String>,
    /// Segundos epoch Unix, igual que `processes[].ct` del snapshot.
    pub start_time: Option<f64>,
    pub window_title: Option<String>,
    pub command_line: Option<String>,
    pub signature: Option<String>,
    /// De dónde salió la firma: `embedded` (en el PE), `catalog` (catálogo de
    /// Windows), `powershell` (respaldo) o `none`.
    pub signature_kind: Option<&'static str>,
    pub trusted: Option<bool>,
    pub signed: Option<bool>,
    pub known_description: Option<&'static str>,
    /// `false` cuando el proceso no se dejó abrir (protegido u otro usuario).
    pub opened: bool,
}

const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const ERROR_INVALID_PARAMETER: i32 = 87;
/// `ProcessCommandLineInformation`, disponible desde Windows 8.1.
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> HANDLE;
    fn Process32FirstW(snapshot: HANDLE, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: HANDLE, entry: *mut ProcessEntry32W) -> i32;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(
        handle: HANDLE,
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

#[link(name = "version")]
extern "system" {
    fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(file: *const u16, handle: u32, length: u32, data: *mut c_void) -> i32;
    fn VerQueryValueW(
        block: *const c_void,
        sub_block: *const u16,
        buffer: *mut *mut c_void,
        length: *mut u32,
    ) -> i32;
}

const TRUST_E_NOSIGNATURE: u32 = 0x800B_0100;
const TRUST_E_SUBJECT_FORM_UNKNOWN: u32 = 0x800B_0102;
const TRUST_E_PROVIDER_UNKNOWN: u32 = 0x800B_0001;
const TRUST_E_SUBJECT_NOT_TRUSTED: u32 = 0x800B_0004;
const TRUST_E_EXPLICIT_DISTRUST: u32 = 0x800B_0111;
const CERT_E_UNTRUSTEDROOT: u32 = 0x800B_0109;
const CERT_E_CHAINING: u32 = 0x800B_010A;
const CERT_E_EXPIRED: u32 = 0x800B_010C;

const WTD_UI_NONE: u32 = 2;
const WTD_REVOKE_NONE: u32 = 0;
const WTD_CHOICE_FILE: u32 = 1;
const WTD_CHOICE_CATALOG: u32 = 2;
const WTD_STATEACTION_VERIFY: u32 = 1;
const WTD_STATEACTION_CLOSE: u32 = 2;
const WTD_REVOCATION_CHECK_NONE: u32 = 0x0000_0010;
/// Sin red: la cadena se valida con la caché local de certificados. Un
/// WinVerifyTrust que salga a internet puede tardar segundos.
const WTD_CACHE_ONLY_URL_RETRIEVAL: u32 = 0x0000_1000;
const WTD_DISABLE_MD2_MD4: u32 = 0x0000_2000;

#[repr(C)]
struct WintrustFileInfo {
    cb_struct: u32,
    file_path: *const u16,
    file_handle: HANDLE,
    known_subject: *mut c_void,
}

/// `WINTRUST_CATALOG_INFO`: verificación de un miembro de catálogo.
#[repr(C)]
struct WintrustCatalogInfo {
    cb_struct: u32,
    catalog_version: u32,
    catalog_file_path: *const u16,
    member_tag: *const u16,
    member_file_path: *const u16,
    member_file: HANDLE,
    calculated_hash: *mut u8,
    calculated_hash_len: u32,
    catalog_context: *mut c_void,
    cat_admin: HANDLE,
}

#[repr(C)]
struct CatalogInfo {
    cb_struct: u32,
    catalog_file: [u16; 260],
}

/// `WINTRUST_DATA`. La unión de proveedores se representa con su único
/// miembro relevante (un puntero): el layout no cambia.
#[repr(C)]
struct WintrustData {
    cb_struct: u32,
    policy_callback_data: *mut c_void,
    sip_client_data: *mut c_void,
    ui_choice: u32,
    revocation_checks: u32,
    union_choice: u32,
    choice_data: *mut c_void,
    state_action: u32,
    state_data: HANDLE,
    url_reference: *const u16,
    provider_flags: u32,
    ui_context: u32,
    signature_settings: *mut c_void,
}

/// `CRYPT_PROVIDER_CERT`: sólo se lee `pCert`; el `cbStruct` sirve de
/// verificación de que el puntero es lo que creemos.
#[repr(C)]
struct CryptProviderCertHead {
    cb_struct: u32,
    cert: *mut c_void,
}

#[link(name = "wintrust")]
extern "system" {
    fn WinVerifyTrust(hwnd: isize, action: *mut GUID, data: *mut c_void) -> i32;
    fn WTHelperProvDataFromStateData(state: HANDLE) -> *mut c_void;
    fn WTHelperGetProvSignerFromChain(
        data: *mut c_void,
        index: u32,
        counter_signer: i32,
        index2: u32,
    ) -> *mut c_void;
    fn WTHelperGetProvCertFromChain(signer: *mut c_void, index: u32) -> *mut c_void;
}

#[link(name = "crypt32")]
extern "system" {
    fn CertGetNameStringW(
        cert: *mut c_void,
        name_type: u32,
        flags: u32,
        type_para: *const c_void,
        buffer: *mut u16,
        size: u32,
    ) -> u32;
    fn CryptCATAdminAcquireContext(
        cat_admin: *mut HANDLE,
        subsystem: *const GUID,
        flags: u32,
    ) -> i32;
    fn CryptCATAdminCalcHashFromFileHandle(
        file: HANDLE,
        hash_len: *mut u32,
        hash: *mut u8,
        flags: u32,
    ) -> i32;
    fn CryptCATAdminEnumCatalogFromHash(
        cat_admin: HANDLE,
        hash: *mut u8,
        hash_len: u32,
        flags: u32,
        previous: *mut HANDLE,
    ) -> HANDLE;
    fn CryptCATCatalogInfoFromContext(info: HANDLE, catalog: *mut CatalogInfo, flags: u32) -> i32;
    fn CryptCATAdminReleaseCatalogContext(cat_admin: HANDLE, info: HANDLE, flags: u32) -> i32;
    fn CryptCATAdminReleaseContext(cat_admin: HANDLE, flags: u32) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn utf16_field(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).trim().to_string()
}

fn filetime_seconds(filetime: FILETIME) -> f64 {
    let ticks = ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64;
    if ticks == 0 {
        return 0.0;
    }
    // 100 ns desde 1601-01-01; el mismo offset que usa nt.rs.
    (ticks as f64 / 10_000_000.0 - 11_644_473_600.0).max(0.0)
}

/// Descripción en español de los procesos de Windows que el usuario ve sin
/// reconocer. Sólo aplica a nombres conocidos: para el resto, `None` y la UI
/// muestra "sin descripción del fabricante".
pub fn known_description(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_ascii_lowercase();
    let base = lower.strip_suffix(".exe").unwrap_or(&lower);
    let compact = base.replace([' ', '-', '_'], "");
    Some(match compact.as_str() {
        "svchost" => "Host de servicio: agrupa varios servicios de Windows en un mismo proceso para ahorrar memoria.",
        "conhost" => "Host de consola: dibuja la ventana y maneja la entrada/salida de las aplicaciones de consola.",
        "dllhost" => "Host COM+: ejecuta componentes COM fuera del proceso que los usa (COM Surrogate).",
        "runtimebroker" => "Agente de tiempo de ejecución: aplicaciones de la Tienda y permisos de cámara, micrófono o ubicación.",
        "wmiprvse" => "Proveedor WMI: atiende las consultas de instrumentación de Windows.",
        "searchindexer" => "Indexador de Windows Search: mantiene el índice de búsqueda de archivos y correo.",
        "msmpeng" => "Motor antimalware de Microsoft Defender: análisis en tiempo real.",
        "audiodg" => "Aislador de dispositivo de audio: procesa el audio en un proceso protegido.",
        "dwm" => "Administrador de ventanas de escritorio: composición, transparencias y efectos visuales.",
        "explorer" => "Explorador de Windows: escritorio, barra de tareas y administrador de archivos.",
        "ctfmon" => "CTF Loader: entrada de texto, teclado en pantalla y servicios de idioma.",
        "sihost" => "Shell Infrastructure Host: hospeda la interfaz del shell (notificaciones, accesorios).",
        "shellexperiencehost" => "Host de experiencia del shell: menú Inicio moderno, centro de actividades y acciones rápidas.",
        "startmenuexperiencehost" => "Host del menú Inicio: interfaz del menú Inicio de Windows 10.",
        "searchhost" => "Host de búsqueda: interfaz del cuadro de búsqueda de Windows 11.",
        "phoneexperiencehost" => "Vínculo con el teléfono: llamadas, mensajes y fotos del celular en la PC.",
        "crossdeviceservice" => "Servicio entre dispositivos: descubre y conecta dispositivos cercanos y el teléfono.",
        "unsecapp" => "Aplicación host no segura: recibe eventos WMI de procesos con otro nivel de seguridad.",
        "taskhostw" => "Host de tareas: ejecuta tareas programadas que viven en DLL.",
        "spoolsv" => "Cola de impresión: gestiona los trabajos de impresión y sus controladores.",
        "lsass" => "Autoridad de seguridad local: autentica usuarios y aplica las políticas de seguridad.",
        "csrss" => "Servidor de tiempo de ejecución del cliente: subsistema de modo usuario de Windows.",
        "wininit" => "Inicio de Windows: arranca los servicios críticos de la sesión 0.",
        "services" => "Administrador de control de servicios: inicia y supervisa los servicios de Windows.",
        "smss" => "Administrador de sesiones: crea las sesiones de usuario.",
        "winlogon" => "Inicio de sesión de Windows: pantalla de bienvenida y bloqueo de la sesión.",
        "fontdrvhost" => "Host de controladores de fuentes: carga las fuentes tipográficas en modo usuario.",
        "securityhealthservice" => "Estado de seguridad de Windows: supervisa Defender y el centro de seguridad.",
        "textinputhost" => "Host de entrada de texto: panel de emojis y entrada táctil o por teclado.",
        "lockapp" => "Aplicación de bloqueo: dibuja la pantalla de bloqueo de Windows.",
        "applicationframehost" => "Host de marco de aplicaciones: dibuja el marco de las apps de la Tienda.",
        "backgroundtaskhost" => "Host de tareas en segundo plano: ejecuta tareas de apps de la Tienda.",
        "aggregatorhost" => "Host agregador: agrupa eventos de diagnóstico y telemetría de Windows.",
        "appactions" => "Acciones de aplicaciones: controla las acciones rápidas y el menú contextual de apps.",
        "amdfendrsr" => "Servicio de AMD: soporte de seguridad y gráficos del driver AMD.",
        "msedgewebview2" => "Componente WebView2 de Microsoft Edge: motor web que usan varias aplicaciones.",
        "msedge" => "Microsoft Edge: navegador de Microsoft.",
        "chrome" => "Google Chrome: navegador de Google.",
        "code" => "Visual Studio Code: editor de código de Microsoft.",
        "chatgpt" => "Aplicación de escritorio de ChatGPT, si la instalaste.",
        "hermesagent" => "Agente de Hermes: automatización y chat del asistente.",
        "node" => "Runtime de Node.js: ejecuta aplicaciones y herramientas JavaScript.",
        "python" => "Intérprete de Python: ejecuta scripts y herramientas Python.",
        "bash" => "Shell de comandos (Git Bash/WSL): no es un servicio de Windows.",
        "powershell" => "Shell de PowerShell: ejecuta comandos y automatizaciones.",
        "pwsh" => "PowerShell 7: shell moderno de Microsoft.",
        "cmd" => "Símbolo del sistema: intérprete de comandos de Windows.",
        "mousocoreworker" => "Worker de Update Orchestrator: descarga e instala actualizaciones de Windows.",
        "trustedinstaller" => "Instalador de módulos de confianza: instala componentes y actualizaciones de Windows.",
        _ => return None,
    })
}

fn toolhelp_rows() -> Vec<(u32, u32, String)> {
    let mut rows = Vec::with_capacity(512);
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return rows;
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != 0 {
        rows.push((
            entry.th32_process_id,
            entry.th32_parent_process_id,
            utf16_field(&entry.sz_exe_file),
        ));
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    close(snapshot);
    rows
}

/// ¿El PID sigue en la tabla de procesos? Es barato (una llamada a Toolhelp)
/// y sirve para no servir una ficha cacheada de un proceso que ya murió.
pub fn process_exists(pid: u32) -> bool {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        // Sin tabla no se puede afirmar nada: que decida el camino lento.
        return true;
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    let mut found = false;
    while more != 0 {
        if entry.th32_process_id == pid {
            found = true;
            break;
        }
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    close(snapshot);
    found
}

fn image_path(handle: HANDLE) -> Option<String> {
    let mut buffer = [0u16; 2048];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 || size == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

fn creation_time(handle: HANDLE) -> Option<f64> {
    let mut creation: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
    let mut user: FILETIME = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    let seconds = filetime_seconds(creation);
    if seconds > 0.0 {
        Some(seconds)
    } else {
        None
    }
}

/// Línea de comandos desde el PEB, vía `ProcessCommandLineInformation`.
/// El buffer empieza con un `UNICODE_STRING` cuyo `Buffer` apunta dentro del
/// mismo buffer: se resuelve por offset en vez de confiar en el puntero.
fn command_line(handle: HANDLE) -> Option<String> {
    let mut needed = 0u32;
    unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 64 * 1024 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize + 16];
    let mut returned = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return None;
    }
    let unit = std::mem::size_of::<usize>();
    if buffer.len() < 8 + unit {
        return None;
    }
    let length = u16::from_le_bytes([buffer[0], buffer[1]]) as usize;
    let mut pointer = [0u8; 8];
    pointer.copy_from_slice(&buffer[8..8 + unit]);
    let address = usize::from_le_bytes(pointer);
    let base = buffer.as_ptr() as usize;
    if length == 0 || length % 2 != 0 {
        return None;
    }
    if address < base || address + length > base + buffer.len() {
        return None;
    }
    let start = address - base;
    let units: Vec<u16> = buffer[start..start + length]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = utf16_field(&units);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Fallback para la línea de comandos: WMI por PID. Sólo se usa cuando la
/// consulta NT falló con un handle válido, y con tope de tiempo para no
/// colgar el handler.
fn command_line_via_wmi(pid: u32) -> Option<String> {
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'\n\
         $p = Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\"\n\
         if ($null -ne $p -and $p.CommandLine) {{ [Console]::Out.Write($p.CommandLine) }}\n"
    );
    let output = run_powershell(&script, Duration::from_millis(2000)).ok()?;
    if output.timed_out {
        return None;
    }
    let text = output.stdout.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

struct VersionStrings {
    description: Option<String>,
    company: Option<String>,
    product: Option<String>,
    version: Option<String>,
}

fn version_string(block: &[u8], translation: (u16, u16), key: &str) -> Option<String> {
    let (language, codepage) = translation;
    let path = wide(&format!(
        "\\StringFileInfo\\{language:04x}{codepage:04x}\\{key}"
    ));
    let mut pointer: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            path.as_ptr(),
            &mut pointer,
            &mut length,
        )
    };
    if ok == 0 || pointer.is_null() || length == 0 {
        return None;
    }
    // `length` viene en WCHARs (incluye el NUL final).
    let units = unsafe { std::slice::from_raw_parts(pointer as *const u16, length as usize) };
    let text = utf16_field(units);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn version_strings(path: &str) -> Option<VersionStrings> {
    let file = wide(path);
    let mut handle = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(file.as_ptr(), &mut handle) };
    if size == 0 || size > 8 * 1024 * 1024 {
        return None;
    }
    let mut block = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(file.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
        return None;
    }

    let mut translations: Vec<(u16, u16)> = Vec::new();
    let key = wide("\\VarFileInfo\\Translation");
    let mut pointer: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            key.as_ptr(),
            &mut pointer,
            &mut length,
        )
    };
    if ok != 0 && !pointer.is_null() {
        let units = length as usize / 2;
        for index in 0..units / 2 {
            let base = pointer as *const u16;
            let language = unsafe { std::ptr::read_unaligned(base.add(index * 2)) };
            let codepage = unsafe { std::ptr::read_unaligned(base.add(index * 2 + 1)) };
            translations.push((language, codepage));
        }
    }
    // 0409 (inglés) + 04B0 (Unicode) es el par habitual de los recursos
    // versionados; se agregan otros por si el archivo trae un solo idioma y
    // la tabla de traducciones no se pudo leer.
    for fallback in [
        (0x0409, 0x04B0),
        (0x0000, 0x04B0),
        (0x0409, 0x04E4),
        (0x0C0A, 0x04B0),
    ] {
        if !translations.contains(&fallback) {
            translations.push(fallback);
        }
    }

    let mut strings = VersionStrings {
        description: None,
        company: None,
        product: None,
        version: None,
    };
    for translation in translations {
        if strings.description.is_none() {
            strings.description = version_string(&block, translation, "FileDescription");
        }
        if strings.company.is_none() {
            strings.company = version_string(&block, translation, "CompanyName");
        }
        if strings.product.is_none() {
            strings.product = version_string(&block, translation, "ProductName");
        }
        if strings.version.is_none() {
            strings.version = version_string(&block, translation, "FileVersion")
                .or_else(|| version_string(&block, translation, "ProductVersion"));
        }
    }
    Some(strings)
}

/// Nombre simple del firmante a partir del contexto de certificado que deja
/// `WinVerifyTrust` en su estado (sirve igual para firma embebida y catálogo).
fn state_signer(state: HANDLE) -> Option<String> {
    if state.is_null() {
        return None;
    }
    let mut buffer = [0u16; 256];
    unsafe {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        if signer.is_null() {
            return None;
        }
        let cert = WTHelperGetProvCertFromChain(signer, 0);
        if cert.is_null() {
            return None;
        }
        let head = &*(cert as *const CryptProviderCertHead);
        // 88 es el tamaño de CRYPT_PROVIDER_CERT en x64: si no coincide, el
        // puntero no es lo que esperamos y no se toca.
        if head.cb_struct != 88 || head.cert.is_null() {
            return None;
        }
        let length = CertGetNameStringW(
            head.cert,
            4, // CERT_NAME_SIMPLE_DISPLAY_TYPE
            0,
            std::ptr::null(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        );
        if length <= 1 {
            return None;
        }
        let text = utf16_field(&buffer[..(length - 1) as usize]);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

struct TrustResult {
    status: u32,
    kind: &'static str,
    signer: Option<String>,
}

/// Firma embebida en el propio PE: `WinVerifyTrust` con `WTD_CHOICE_FILE`.
fn verify_embedded(file: &[u16]) -> TrustResult {
    let mut info = WintrustFileInfo {
        cb_struct: std::mem::size_of::<WintrustFileInfo>() as u32,
        file_path: file.as_ptr(),
        file_handle: null_handle(),
        known_subject: std::ptr::null_mut(),
    };
    let mut data = WintrustData {
        cb_struct: std::mem::size_of::<WintrustData>() as u32,
        policy_callback_data: std::ptr::null_mut(),
        sip_client_data: std::ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        choice_data: &mut info as *mut WintrustFileInfo as *mut c_void,
        state_action: WTD_STATEACTION_VERIFY,
        state_data: null_handle(),
        url_reference: std::ptr::null(),
        provider_flags: WTD_REVOCATION_CHECK_NONE
            | WTD_CACHE_ONLY_URL_RETRIEVAL
            | WTD_DISABLE_MD2_MD4,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    // {00AAC56B-CD44-11d0-8CC2-00C04FC295EE}
    let mut action = GUID {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };
    // `-1` es INVALID_HANDLE_VALUE: la política no abre ninguna ventana.
    let code = unsafe {
        WinVerifyTrust(
            -1isize,
            &mut action,
            &mut data as *mut WintrustData as *mut c_void,
        )
    };
    let signer = state_signer(data.state_data);
    data.state_action = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            -1isize,
            &mut action,
            &mut data as *mut WintrustData as *mut c_void,
        );
    }
    TrustResult {
        status: code as u32,
        kind: "embedded",
        signer,
    }
}

/// Firma por catálogo: muchos binarios de Windows (cmd, conhost, notepad,
/// winlogon) no llevan Authenticode en el PE y los cubre un catálogo firmado
/// del sistema. Sin esto la ficha diría "sin firma" en medio sistema.
fn verify_catalog(file: &[u16], path: &str) -> Option<TrustResult> {
    let mut admin: HANDLE = null_handle();
    if unsafe { CryptCATAdminAcquireContext(&mut admin, std::ptr::null(), 0) } == 0 {
        return None;
    }
    let result = catalog_member(admin, file, path);
    unsafe { CryptCATAdminReleaseContext(admin, 0) };
    result
}

fn catalog_member(admin: HANDLE, file: &[u16], path: &str) -> Option<TrustResult> {
    use std::os::windows::io::AsRawHandle;

    let handle = std::fs::File::open(path).ok()?;
    let raw = handle.as_raw_handle() as HANDLE;
    let mut hash_length = 0u32;
    if unsafe {
        CryptCATAdminCalcHashFromFileHandle(raw, &mut hash_length, std::ptr::null_mut(), 0)
    } == 0
        || hash_length == 0
        || hash_length > 128
    {
        return None;
    }
    let mut hash = vec![0u8; hash_length as usize];
    if unsafe { CryptCATAdminCalcHashFromFileHandle(raw, &mut hash_length, hash.as_mut_ptr(), 0) }
        == 0
    {
        return None;
    }
    let catalog = unsafe {
        CryptCATAdminEnumCatalogFromHash(
            admin,
            hash.as_mut_ptr(),
            hash_length,
            0,
            std::ptr::null_mut(),
        )
    };
    if catalog.is_null() {
        return None;
    }

    let mut info: CatalogInfo = unsafe { std::mem::zeroed() };
    info.cb_struct = std::mem::size_of::<CatalogInfo>() as u32;
    if unsafe { CryptCATCatalogInfoFromContext(catalog, &mut info, 0) } == 0 {
        unsafe { CryptCATAdminReleaseCatalogContext(admin, catalog, 0) };
        return None;
    }
    let catalog_path = wide(&utf16_field(&info.catalog_file));
    // El "member tag" de un catálogo es el hash del archivo en hex mayúsculas.
    let tag: String = hash.iter().map(|byte| format!("{byte:02X}")).collect();
    let tag = wide(&tag);

    let mut catalog_info = WintrustCatalogInfo {
        cb_struct: std::mem::size_of::<WintrustCatalogInfo>() as u32,
        catalog_version: 0,
        catalog_file_path: catalog_path.as_ptr(),
        member_tag: tag.as_ptr(),
        member_file_path: file.as_ptr(),
        member_file: raw,
        calculated_hash: hash.as_mut_ptr(),
        calculated_hash_len: hash_length,
        catalog_context: std::ptr::null_mut(),
        cat_admin: admin,
    };
    let mut data = WintrustData {
        cb_struct: std::mem::size_of::<WintrustData>() as u32,
        policy_callback_data: std::ptr::null_mut(),
        sip_client_data: std::ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_CATALOG,
        choice_data: &mut catalog_info as *mut WintrustCatalogInfo as *mut c_void,
        state_action: WTD_STATEACTION_VERIFY,
        state_data: null_handle(),
        url_reference: std::ptr::null(),
        provider_flags: WTD_REVOCATION_CHECK_NONE
            | WTD_CACHE_ONLY_URL_RETRIEVAL
            | WTD_DISABLE_MD2_MD4,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    let mut action = GUID {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };
    let code = unsafe {
        WinVerifyTrust(
            -1isize,
            &mut action,
            &mut data as *mut WintrustData as *mut c_void,
        )
    };
    let signer = state_signer(data.state_data);
    data.state_action = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            -1isize,
            &mut action,
            &mut data as *mut WintrustData as *mut c_void,
        );
        CryptCATAdminReleaseCatalogContext(admin, catalog, 0);
    }
    Some(TrustResult {
        status: code as u32,
        kind: "catalog",
        signer,
    })
}

#[derive(Clone)]
struct SignatureInfo {
    text: String,
    kind: &'static str,
    signed: bool,
    trusted: bool,
}

struct CachedSignature {
    stored_at: Instant,
    info: SignatureInfo,
}

/// Cinco minutos por ruta: los binarios (y sus procesos hijos) comparten
/// archivo, y verificar la firma cuesta entre 5 y 60 ms.
const SIGNATURE_TTL: Duration = Duration::from_secs(300);
const SIGNATURE_CACHE_MAX: usize = 128;

fn signature_cache() -> &'static Mutex<HashMap<String, CachedSignature>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedSignature>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_system_path(path: &str) -> bool {
    static ROOT: OnceLock<String> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        std::env::var("SystemRoot")
            .unwrap_or_default()
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    });
    if root.is_empty() {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    lower.starts_with(root.as_str()) && lower.as_bytes().get(root.len()) == Some(&b'\\')
}

/// Firma de un ejecutable, cacheada por ruta: Authenticode embebido y, si no
/// hay, catálogo de Windows. El catálogo sólo se consulta para binarios del
/// sistema: hashear un .exe grande de terceros (Electron, MSIX) cuesta casi un
/// segundo y no aporta, porque esos van firmados en el propio PE.
fn signature_of(path: &str) -> SignatureInfo {
    let key = path.to_ascii_lowercase();
    {
        let cache = signature_cache();
        let guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(entry) = guard.get(&key) {
            if entry.stored_at.elapsed() < SIGNATURE_TTL {
                return entry.info.clone();
            }
        }
    }
    let info = compute_signature(path);
    let cache = signature_cache();
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    guard.retain(|_, entry| entry.stored_at.elapsed() < SIGNATURE_TTL);
    if guard.len() >= SIGNATURE_CACHE_MAX {
        if let Some(oldest) = guard
            .iter()
            .min_by_key(|(_, entry)| entry.stored_at)
            .map(|(key, _)| key.clone())
        {
            guard.remove(&oldest);
        }
    }
    guard.insert(
        key,
        CachedSignature {
            stored_at: Instant::now(),
            info: info.clone(),
        },
    );
    info
}

fn compute_signature(path: &str) -> SignatureInfo {
    let file = wide(path);
    let embedded = verify_embedded(&file);
    let trust = if embedded.status == TRUST_E_NOSIGNATURE && is_system_path(path) {
        verify_catalog(&file, path).unwrap_or(embedded)
    } else {
        embedded
    };
    let mut info = SignatureInfo {
        text: signature_text(trust.status, trust.kind, trust.signer.as_deref()),
        kind: trust.kind,
        signed: !matches!(
            trust.status,
            TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN
        ),
        trusted: trust.status == 0,
    };
    // Códigos que la política no supo clasificar: última palabra para
    // Get-AuthenticodeSignature.
    if matches!(
        trust.status,
        TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN
    ) {
        if let Some((text, signed, trusted)) = signature_via_powershell(path) {
            info.text = text;
            info.signed = signed;
            info.trusted = trusted;
            info.kind = "powershell";
        }
    }
    info
}

fn signature_text(status: u32, kind: &str, signer: Option<&str>) -> String {
    let who = signer.unwrap_or("editor desconocido");
    match status {
        0 => match signer {
            Some(name) if kind == "catalog" => format!("Firmado por catálogo · {name}"),
            Some(name) => format!("Firmado · {name}"),
            None => "Firmado y verificado".to_string(),
        },
        TRUST_E_NOSIGNATURE => "Sin firma digital".to_string(),
        TRUST_E_EXPLICIT_DISTRUST => {
            "Sin firma válida · editor en la lista de no confianza".to_string()
        }
        CERT_E_UNTRUSTEDROOT | CERT_E_CHAINING => {
            format!("Sin firma válida · {who} (cadena de confianza incompleta)")
        }
        CERT_E_EXPIRED => format!("Sin firma válida · {who} (certificado vencido)"),
        TRUST_E_SUBJECT_NOT_TRUSTED => format!("Sin firma válida · {who}"),
        TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
            "Sin firma verificada · formato no reconocido".to_string()
        }
        other => format!("Sin firma verificada (0x{other:08X}) · {who}"),
    }
}

/// Fallback de la firma: Get-AuthenticodeSignature. Sólo para los códigos que
/// WinVerifyTrust no supo clasificar, y con tope de 1,2 s.
fn signature_via_powershell(path: &str) -> Option<(String, bool, bool)> {
    let escaped = path.replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'\n\
         $s = Get-AuthenticodeSignature -LiteralPath '{escaped}'\n\
         if ($null -eq $s) {{ exit 0 }}\n\
         $who = ''\n\
         if ($null -ne $s.SignerCertificate) {{ $who = $s.SignerCertificate.GetNameInfo('SimpleName', $false) }}\n\
         [Console]::Out.Write(([string]$s.Status) + \"`t\" + $who)\n"
    );
    let output = run_powershell(&script, Duration::from_millis(1200)).ok()?;
    if output.timed_out {
        return None;
    }
    let line = output
        .stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    let (status, who) = line.split_once('\t').unwrap_or((line, ""));
    let who = who.trim();
    let trusted = status.eq_ignore_ascii_case("Valid");
    let signed = !status.is_empty() && !status.eq_ignore_ascii_case("NotSigned");
    let text = if trusted {
        match who.is_empty() {
            true => "Firmado y verificado".to_string(),
            false => format!("Firmado · {who}"),
        }
    } else if !signed {
        "Sin firma digital".to_string()
    } else {
        match who.is_empty() {
            true => format!("Sin firma válida ({status})"),
            false => format!("Sin firma válida · {who} ({status})"),
        }
    };
    Some((text, signed, trusted))
}

fn window_title_of(pid: u32) -> Option<String> {
    struct Search {
        pid: u32,
        best: Option<String>,
        longest: usize,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam as *mut Search);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner != search.pid || IsWindowVisible(hwnd) == 0 {
            return TRUE;
        }
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        if style & WS_EX_TOOLWINDOW as i32 != 0 {
            return TRUE;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return TRUE;
        }
        let mut buffer = [0u16; 512];
        let written = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        if written <= 0 {
            return TRUE;
        }
        let text = String::from_utf16_lossy(&buffer[..written as usize])
            .trim()
            .to_string();
        if text.is_empty() {
            return TRUE;
        }
        if text.chars().count() > search.longest {
            search.longest = text.chars().count();
            search.best = Some(text);
        }
        TRUE
    }

    let mut search = Search {
        pid,
        best: None,
        longest: 0,
    };
    unsafe {
        EnumWindows(Some(callback), &mut search as *mut Search as LPARAM);
    }
    search.best
}

/// Ficha completa de un PID. `seed_name`/`seed_start` son los datos del último
/// tick del motor: sirven para los procesos que no se dejan abrir (SYSTEM,
/// PPL) y para distinguir "no existe" (404) de "terminó" (410).
pub fn process_detail(
    pid: u32,
    seed_name: &str,
    seed_start: f64,
) -> Result<ProcessDetail, DetailError> {
    // El PID 0 sólo existe en el snapshot del kernel (System Idle): no tiene
    // imagen ni handles, así que no hay ficha que armar.
    if pid == 0 {
        return Err(DetailError::NotFound);
    }
    let rows = toolhelp_rows();
    let row = rows
        .iter()
        .find(|(row_pid, _, _)| *row_pid == pid)
        .map(|(row_pid, parent, name)| (*row_pid, *parent, name.clone()));
    let name = match row.as_ref() {
        Some((_, _, name)) if !name.is_empty() => name.clone(),
        _ => seed_name.trim().to_string(),
    };
    if row.is_none() && name.is_empty() {
        return Err(DetailError::NotFound);
    }

    let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION);
    if row.is_none() && handle.is_none() {
        // Ni en la tabla de procesos ni abrible: si el error es "parámetro
        // inválido" el PID ya no existe (terminó entre el tick y el pedido).
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(if err == ERROR_INVALID_PARAMETER {
            DetailError::Vanished
        } else {
            DetailError::NotFound
        });
    }

    let parent_pid = row
        .as_ref()
        .map(|(_, parent, _)| *parent)
        .filter(|parent| *parent != 0);
    let parent = parent_pid.map(|parent_pid| {
        match rows
            .iter()
            .find(|(row_pid, _, _)| *row_pid == parent_pid)
            .map(|(_, _, name)| name.clone())
        {
            Some(parent_name) if !parent_name.is_empty() => {
                format!("{parent_name} (PID {parent_pid})")
            }
            _ => format!("PID {parent_pid}"),
        }
    });

    let mut detail = ProcessDetail {
        pid,
        name,
        parent_pid,
        parent,
        start_time: if seed_start > 0.0 {
            Some(seed_start)
        } else {
            None
        },
        ..ProcessDetail::default()
    };
    detail.known_description = known_description(&detail.name);

    if let Some(handle) = handle {
        detail.opened = true;
        let path = image_path(handle);
        if path.is_none() && row.is_none() {
            // Abrible pero sin imagen y fuera de la tabla: el proceso terminó
            // entre el snapshot y esta consulta.
            close(handle);
            return Err(DetailError::Vanished);
        }
        if let Some(path) = path {
            if let Some(strings) = version_strings(&path) {
                detail.description = strings.description;
                detail.company = strings.company;
                detail.product = strings.product;
                detail.version = strings.version;
            }
            let signature = signature_of(&path);
            detail.signature = Some(signature.text);
            detail.signature_kind = Some(signature.kind);
            detail.signed = Some(signature.signed);
            detail.trusted = Some(signature.trusted);
            detail.path = Some(path);
        }
        if let Some(started) = creation_time(handle) {
            detail.start_time = Some(started);
        }
        detail.command_line = command_line(handle).or_else(|| command_line_via_wmi(pid));
        close(handle);
    }

    detail.window_title = window_title_of(pid);
    Ok(detail)
}

pub fn foreground_pid() -> Option<u32> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let pids = &mut *(lparam as *mut HashSet<u32>);
    if IsWindowVisible(hwnd) == 0 {
        return TRUE;
    }
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
    if ex_style & WS_EX_TOOLWINDOW as i32 != 0 {
        return TRUE;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        pids.insert(pid);
    }
    TRUE
}

pub fn visible_window_pids() -> HashSet<u32> {
    let mut pids = HashSet::new();
    unsafe {
        EnumWindows(Some(enum_windows_callback), &mut pids as *mut _ as LPARAM);
    }
    pids
}

pub fn ac_online() -> bool {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    if ok == 0 {
        return true;
    }
    match status.ACLineStatus {
        0 => false,
        _ => true,
    }
}

// ------------------------------------------------------------ tarea admin

/// Nombre fijo de la tarea programada elevada.
pub const TASK_NAME: &str = "Domador";
/// `CREATE_NO_WINDOW`: evita consolas parpadeando al lanzar PowerShell.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Tope de salida que se acumula por proceso (evita crecer sin límite).
const MAX_CAPTURE: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct AdminTaskStatus {
    pub installed: bool,
    pub task_name: String,
    pub state: String,
    pub run_level: String,
    pub execute: String,
    pub arguments: String,
    pub last_result: Option<i64>,
    pub last_run: String,
    pub error: Option<String>,
}

impl AdminTaskStatus {
    /// La tarea existe y está configurada para correr elevada.
    pub fn elevated(&self) -> bool {
        self.installed && self.run_level.eq_ignore_ascii_case("HighestAvailable")
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "installed": self.installed,
            "task_name": self.task_name,
            "state": self.state,
            "run_level": self.run_level,
            "execute": self.execute,
            "arguments": self.arguments,
            "last_result": self.last_result,
            "last_run": self.last_run,
            "elevated": self.elevated(),
            "error": self.error,
        })
    }
}

fn powershell_path() -> PathBuf {
    if let Ok(root) = std::env::var("SystemRoot") {
        let candidate = Path::new(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    // Último recurso: que lo resuelva el sistema. Se prefiere la ruta absoluta
    // porque `powershell` a secas se puede secuestrar desde el directorio
    // actual o un PATH manipulador.
    PathBuf::from("powershell.exe")
}

fn schtasks_path() -> PathBuf {
    if let Ok(root) = std::env::var("SystemRoot") {
        let candidate = Path::new(&root).join("System32").join("schtasks.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("schtasks.exe")
}

/// Chequeo barato de existencia de la tarea: `schtasks` es un binario y no
/// paga el arranque de PowerShell (2-3 s en frío). Si no se puede determinar,
/// devuelve `None` y el llamador consulta el detalle completo.
fn scheduled_task_exists() -> Option<bool> {
    let mut command = Command::new(schtasks_path());
    command.args(["/Query", "/TN", TASK_NAME]);
    let output = run_bounded(command, Duration::from_secs(8)).ok()?;
    if output.timed_out {
        return None;
    }
    Some(output.success())
}

fn read_capped(mut pipe: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = pipe.by_ref().take(MAX_CAPTURE).read_to_end(&mut buffer);
    buffer
}

/// Decodifica stdout/stderr de herramientas Win32: UTF-16 si hay BOM, UTF-8
/// tolerante en el resto de los casos.
fn decode_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).to_string()
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// PowerShell `-EncodedCommand` espera UTF-16LE en base64. Evita problemas de
/// comillas y de codificación al pasar scripts de varias líneas.
fn base64_utf16le(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Ejecuta un proceso con tope de tiempo, capturando stdout/stderr en paralelo
/// para no bloquearse si el hijo llena el pipe.
fn run_bounded(mut command: Command, timeout: Duration) -> Result<CmdOutput, String> {
    use std::os::windows::process::CommandExt;

    let program = format!("{:?}", command.get_program());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .map_err(|err| format!("no pude lanzar {program}: {err}"))?;

    let out_thread = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_capped(pipe)));
    let err_thread = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || read_capped(pipe)));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break None,
        }
    };

    let stdout = out_thread
        .and_then(|handle| handle.join().ok())
        .map(|bytes| decode_output(&bytes))
        .unwrap_or_default();
    let stderr = err_thread
        .and_then(|handle| handle.join().ok())
        .map(|bytes| decode_output(&bytes))
        .unwrap_or_default();

    Ok(CmdOutput {
        code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        timed_out,
    })
}

fn powershell_command() -> Command {
    let mut command = Command::new(powershell_path());
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
    ]);
    command
}

/// Corre un script PowerShell (sin archivo) con `-EncodedCommand`.
pub fn run_powershell(script: &str, timeout: Duration) -> Result<CmdOutput, String> {
    let encoded = base64_utf16le(script);
    let mut command = powershell_command();
    command.args(["-EncodedCommand", &encoded]);
    run_bounded(command, timeout)
}

/// Busca `install-admin.ps1` junto al ejecutable, en `scripts/` (instalación y
/// árbol de desarrollo) en vez de asumir una sola ubicación.
pub fn find_install_script() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("install-admin.ps1"));
            candidates.push(dir.join("scripts").join("install-admin.ps1"));
        }
    }
    if let Some(dir) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(Path::new(dir).join("scripts").join("install-admin.ps1"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Copia embebida del instalador: evita depender de una copia vieja junto al
/// ejecutable (el binario y el .ps1 pueden desincronizarse tras una actualización).
pub const INSTALL_SCRIPT: &str = include_str!("../scripts/install-admin.ps1");

/// Ejecuta el instalador embebido como scriptblock. No se escribe ningún
/// archivo temporal (evita el TOCTOU de ejecutar un .ps1 en %TEMP% elevado).
pub fn run_embedded_install_script() -> Result<CmdOutput, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let wrapper = format!(
        "$ErrorActionPreference = 'Stop'\n& ([scriptblock]::Create(@'\n{}\n'@)) -ExeDir '{}'\n",
        INSTALL_SCRIPT,
        exe_dir.replace('\'', "''")
    );
    run_powershell(&wrapper, Duration::from_secs(120))
}

/// Primer mensaje útil de un stderr de PowerShell: descarta el preámbulo
/// CLIXML de progreso que aparece al redirigir la salida.
pub fn clean_diagnostic(text: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("#<") || line.starts_with('<') {
            continue;
        }
        return line.chars().take(300).collect();
    }
    String::new()
}

/// Consulta el estado real de la tarea programada (no asume que exista).
pub fn admin_task_status() -> AdminTaskStatus {
    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$name = 'Domador'
$task = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
if ($null -eq $task) { Write-Output 'INSTALLED=0'; exit 0 }
Write-Output 'INSTALLED=1'
Write-Output ('STATE=' + [string]$task.State)
Write-Output ('RUNLEVEL=' + [string]$task.Principal.RunLevel)
$first = @($task.Actions)[0]
if ($null -ne $first) {
  Write-Output ('EXECUTE=' + [string]$first.Execute)
  Write-Output ('ARGS=' + [string]$first.Arguments)
} else {
  Write-Output 'EXECUTE='
  Write-Output 'ARGS='
}
$info = Get-ScheduledTaskInfo -TaskName $name -ErrorAction SilentlyContinue
if ($null -ne $info) {
  Write-Output ('LASTRESULT=' + [string]$info.LastTaskResult)
  if ($null -ne $info.LastRunTime) {
    Write-Output ('LASTRUN=' + $info.LastRunTime.ToString('o'))
  } else {
    Write-Output 'LASTRUN='
  }
}
"#;

    let mut status = AdminTaskStatus {
        task_name: TASK_NAME.to_string(),
        ..AdminTaskStatus::default()
    };
    if scheduled_task_exists() == Some(false) {
        // Camino común (no instalada): evita levantar PowerShell al pedir estado.
        return status;
    }
    let output = match run_powershell(script, Duration::from_secs(12)) {
        Ok(output) => output,
        Err(err) => {
            status.error = Some(err);
            return status;
        }
    };
    if output.timed_out {
        status.error = Some("PowerShell no respondió a tiempo".into());
        return status;
    }
    if !output.success() {
        let detail = clean_diagnostic(&output.stderr);
        if !detail.is_empty() {
            status.error = Some(detail);
        }
        return status;
    }
    for line in output.stdout.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "INSTALLED" => status.installed = value == "1",
            "STATE" => status.state = value,
            "RUNLEVEL" => status.run_level = value,
            "EXECUTE" => status.execute = value,
            "ARGS" => status.arguments = value,
            "LASTRESULT" => status.last_result = value.parse::<i64>().ok(),
            "LASTRUN" => status.last_run = value,
            _ => {}
        }
    }
    status
}

fn last_error(what: &str) -> String {
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(0) {
        format!("{what} falló")
    } else {
        format!("{what}: {err}")
    }
}

pub fn null_handle() -> HANDLE {
    ptr::null_mut()
}

#[cfg(test)]
mod detail_tests {
    use super::*;

    #[test]
    fn describe_los_procesos_conocidos() {
        assert!(known_description("svchost.exe").is_some());
        assert!(known_description("SVCHOST").is_some());
        assert!(known_description("RuntimeBroker.exe").is_some());
        assert!(known_description("SearchIndexer.exe").is_some());
        assert!(known_description("TrustedInstaller.exe").is_some());
        // Desconocidos y entradas raras: ni descripción inventada ni pánico.
        assert!(known_description("mi-app.exe").is_none());
        assert!(known_description("").is_none());
        assert!(known_description(".exe").is_none());
    }

    #[test]
    fn ficha_del_propio_proceso() {
        let pid = std::process::id();
        let detail = process_detail(pid, "", 0.0).expect("ficha del propio proceso");
        assert_eq!(detail.pid, pid);
        assert!(detail.name.to_ascii_lowercase().ends_with(".exe"));
        assert!(detail.opened, "el propio proceso se tiene que poder abrir");
        assert!(detail.path.is_some());
        assert!(detail.start_time.unwrap_or(0.0) > 1_600_000_000.0);
        assert!(detail.command_line.is_some());
        assert!(detail.signature.is_some(), "la firma se resuelve siempre");
        assert!(detail.parent_pid.is_some());
    }

    #[test]
    fn pid_inexistente_es_not_found() {
        assert_eq!(
            process_detail(0, "", 0.0).unwrap_err(),
            DetailError::NotFound
        );
        assert_eq!(
            process_detail(u32::MAX - 1, "", 0.0).unwrap_err(),
            DetailError::NotFound
        );
    }

    #[test]
    fn proceso_terminado_es_vanished() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .expect("hijo efímero");
        let pid = child.id();
        let _ = child.wait();
        // Con el nombre del último tick, el PID que ya no está se reporta como
        // "terminó" (410) y no como inexistente (404).
        assert_eq!(
            process_detail(pid, "cmd.exe", 1.0).unwrap_err(),
            DetailError::Vanished
        );
    }
}
