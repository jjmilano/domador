//! Icono de bandeja para Domador.
//!
//! El motor corre aparte (`domador.exe run --no-open`). Este binario sólo
//! muestra el icono, abre la UI y permite apagar el motor desde el menú.

#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::mem::size_of;
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    IDC_ARROW, IDI_APPLICATION, MF_STRING, MSG, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON,
    WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PORT: u16 = 8787;
const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: u16 = 1;
const ID_FOLDER: u16 = 2;
const ID_EXIT: u16 = 3;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn server_exe() -> PathBuf {
    install_dir().join("domador.exe")
}

fn api_alive() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn ensure_server() {
    if api_alive() {
        return;
    }
    let exe = server_exe();
    let _ = Command::new(exe)
        .args(["run", "--no-open"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    for _ in 0..40 {
        if api_alive() {
            return;
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn open_ui() {
    ensure_server();
    let url = format!("http://127.0.0.1:{PORT}/");
    let browsers = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ];
    for browser in browsers {
        if Path::new(browser).exists() {
            let _ = Command::new(browser)
                .arg(format!("--app={url}"))
                .arg("--window-size=1400,900")
                .spawn();
            return;
        }
    }
    let operation = wide("open");
    let target = wide(&url);
    unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

fn open_folder() {
    let operation = wide("open");
    let folder = wide(&install_dir().to_string_lossy());
    unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            folder.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

fn stop_server() {
    let _ = Command::new(server_exe())
        .arg("stop")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

fn show_menu(hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let open = wide("Abrir Domador");
    let folder = wide("Abrir carpeta");
    let exit = wide("Salir");
    unsafe {
        AppendMenuW(menu, MF_STRING, ID_OPEN as usize, open.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_FOLDER as usize, folder.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_EXIT as usize, exit.as_ptr());
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAY => {
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_RBUTTONUP {
                show_menu(hwnd);
            } else if event == WM_LBUTTONDBLCLK {
                open_ui();
            }
            0
        }
        WM_COMMAND => {
            match (wparam & 0xFFFF) as u16 {
                ID_OPEN => open_ui(),
                ID_FOLDER => open_folder(),
                ID_EXIT => {
                    stop_server();
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn main() {
    let mutex_name = wide("DomadorTrayMutex");
    let mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    ensure_server();

    let class_name = wide("DomadorTrayWindow");
    let title = wide("Domador");
    let instance = unsafe { GetModuleHandleW(null()) };
    let window_class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: 0 as *mut c_void,
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
    };
    unsafe {
        RegisterClassW(&window_class);
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null_mut(),
        )
    };
    if hwnd.is_null() {
        return;
    }

    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    let tip: Vec<u16> = "Domador - control de procesos"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let tip_len = tip.len().min(nid.szTip.len());
    nid.szTip[..tip_len].copy_from_slice(&tip[..tip_len]);
    unsafe {
        Shell_NotifyIconW(NIM_ADD, &mut nid);
    }

    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let _ = mutex;
}
