use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, FALSE, HANDLE, HWND, LPARAM, TRUE};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows_sys::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessInformation, OpenProcess,
    QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask, SetProcessInformation,
    TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible,
    GWL_EXSTYLE, WS_EX_TOOLWINDOW,
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
