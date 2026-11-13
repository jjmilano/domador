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
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadCursorW, LoadIconW, LoadImageW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TrackPopupMenu,
    TranslateMessage, IDC_ARROW, IDI_APPLICATION, IMAGE_ICON, LR_LOADFROMFILE, MF_STRING, MSG,
    SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_DESTROY,
    WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PORT: u16 = 8787;
const WM_TRAY: u32 = WM_APP + 1;
const TIMER_ANIM: usize = 1;
const ID_OPEN: u16 = 1;
const ID_FOLDER: u16 = 2;
const ID_EXIT: u16 = 3;
const ID_ADMIN: u16 = 4;

/// Frames de la bandeja, incrustados para que el .exe siga siendo un solo archivo.
const TRAY_FRAMES: [&[u8]; 8] = [
    include_bytes!("../../assets/tray/anim00.ico"),
    include_bytes!("../../assets/tray/anim01.ico"),
    include_bytes!("../../assets/tray/anim02.ico"),
    include_bytes!("../../assets/tray/anim03.ico"),
    include_bytes!("../../assets/tray/anim04.ico"),
    include_bytes!("../../assets/tray/anim05.ico"),
    include_bytes!("../../assets/tray/anim06.ico"),
    include_bytes!("../../assets/tray/anim07.ico"),
];

struct Animation {
    icons: Vec<usize>,
    index: usize,
}

static ANIMATION: OnceLock<Mutex<Animation>> = OnceLock::new();

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn app_icon(instance: *mut c_void) -> *mut c_void {
    // winres incrusta el .ico como recurso 1. Si algo falla, el icono
    // genérico es mejor que un hueco vacío en la bandeja.
    let icon = unsafe { LoadIconW(instance, 1 as *const u16) };
    if icon.is_null() {
        unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
    } else {
        icon
    }
}

fn load_animation_icons() -> Vec<usize> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(install_dir)
        .join("Domador")
        .join("anim");
    if std::fs::create_dir_all(&base).is_err() {
        return Vec::new();
    }
    let mut icons = Vec::with_capacity(TRAY_FRAMES.len());
    for (i, bytes) in TRAY_FRAMES.iter().enumerate() {
        let path = base.join(format!("frame{i:02}.ico"));
        if std::fs::write(&path, bytes).is_err() {
            continue;
        }
        let path_w = wide(&path.to_string_lossy());
        let icon = unsafe {
            LoadImageW(
                null_mut(),
                path_w.as_ptr(),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE,
            )
        };
        if !icon.is_null() {
            icons.push(icon as usize);
        }
    }
    icons
}

fn start_animation(hwnd: HWND) {
    let icons = load_animation_icons();
    if icons.len() < 2 {
        return;
    }
    let _ = ANIMATION.set(Mutex::new(Animation { icons, index: 0 }));
    unsafe {
        SetTimer(hwnd, TIMER_ANIM, 140, None);
    }
}

fn animate_tray(hwnd: HWND) {
    let Some(lock) = ANIMATION.get() else {
        return;
    };
    let Ok(mut animation) = lock.lock() else {
        return;
    };
    if animation.icons.len() < 2 {
        return;
    }
    animation.index = (animation.index + 1) % animation.icons.len();
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON;
    nid.hIcon = animation.icons[animation.index] as *mut c_void;
    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &mut nid);
    }
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
    // La UI nativa es la principal desde 0.6. Edge queda sólo como fallback.
    let native = install_dir().join("domador-ui.exe");
    if native.exists() {
        if Command::new(native).spawn().is_ok() {
            return;
        }
    }
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

fn relaunch_admin(hwnd: HWND) {
    stop_server();
    let exe = std::env::current_exe().unwrap_or_default();
    let operation = wide("runas");
    let target = wide(&exe.to_string_lossy());
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute devuelve >32 si pudo lanzar el proceso elevado. Si el
    // usuario cancela el UAC, dejamos la bandeja actual viva.
    if result as isize > 32 {
        thread::sleep(Duration::from_millis(600));
        unsafe {
            DestroyWindow(hwnd);
        }
    }
}

fn show_menu(hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    let open = wide("Abrir Domador");
    let folder = wide("Abrir carpeta");
    let admin = wide("Reiniciar como administrador");
    let exit = wide("Salir");
    unsafe {
        AppendMenuW(menu, MF_STRING, ID_OPEN as usize, open.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_FOLDER as usize, folder.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_ADMIN as usize, admin.as_ptr());
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
                ID_ADMIN => relaunch_admin(hwnd),
                ID_EXIT => {
                    stop_server();
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_ANIM {
                animate_tray(hwnd);
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_ANIM);
            }
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
    let mut mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
    let mut tries = 0;
    while !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS && tries < 30 {
        // Un "Reiniciar como administrador" lanza la nueva instancia antes de
        // que la vieja termine de destruir su ventana. Darle tiempo.
        unsafe {
            CloseHandle(mutex);
        }
        thread::sleep(Duration::from_millis(100));
        mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
        tries += 1;
    }
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    ensure_server();

    let class_name = wide("DomadorTrayWindow");
    let title = wide("Domador");
    let instance = unsafe { GetModuleHandleW(null()) };
    let icon = app_icon(instance);
    let window_class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: icon,
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
    nid.hIcon = icon;
    let tip: Vec<u16> = "Domador - control de procesos"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let tip_len = tip.len().min(nid.szTip.len());
    nid.szTip[..tip_len].copy_from_slice(&tip[..tip_len]);
    unsafe {
        Shell_NotifyIconW(NIM_ADD, &mut nid);
    }
    start_animation(hwnd);

    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let _ = mutex;
}
