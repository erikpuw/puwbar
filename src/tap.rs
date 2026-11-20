use crate::desktop::wide;
use std::{
    mem::size_of,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::*,
        System::{LibraryLoader::*, Registry::*, Threading::*},
        UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

// {7F4B2C91-3E8A-4D15-9C62-8A1F5E3B9D42}
const CLSID_PUWBAR_TAP: GUID = GUID {
    data1: 0x7f4b_2c91,
    data2: 0x3e8a,
    data3: 0x4d15,
    data4: [0x9c, 0x62, 0x8a, 0x1f, 0x5e, 0x3b, 0x9d, 0x42],
};

const EXIT_HOTKEY: i32 = 1;
const TRAY_CALLBACK: u32 = WM_APP + 10;
const TRAY_UID: u32 = 1;
const CMD_AUTOSTART: u32 = 1001;
const CMD_REAPPLY: u32 = 1002;
const CMD_EXIT: u32 = 1003;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "PuwBar";
const TASK_NAME: &str = "PuwBar";
const STOP_EVENT_NAME: &str = "Local\\PuwBar.StopEvent";
const READY_EVENT_NAME: &str = "Local\\PuwBar.TapReady";
const RELOAD_EVENT_NAME: &str = "Local\\PuwBar.ReloadEvent";

type InitXamlDiagExFn = unsafe extern "system" fn(
    end_point_name: *const u16,
    pid: u32,
    wsz_dll_xaml_diagnostics: *const u16,
    wsz_tap_dll_name: *const u16,
    tap_clsid: GUID,
    wsz_initialization_data: *const u16,
) -> i32;

const EMBEDDED_TAP_DLL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/puwbar_tap.dll"));

pub fn tap_dll_path() -> Result<PathBuf, String> {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in EMBEDDED_TAP_DLL {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let staged = std::env::temp_dir().join(format!("puwbar_tap_{hash:016x}.dll"));
    if !staged.exists() {
        std::fs::write(&staged, EMBEDDED_TAP_DLL)
            .map_err(|e| format!("Không ghi được DLL tạm {}: {e}", staged.display()))?;
    }
    staged
        .canonicalize()
        .map(|p| {
            let s = p.to_string_lossy();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
        })
        .map_err(|e| format!("Không chuẩn hóa được đường dẫn DLL: {e}"))
}

unsafe fn explorer_pid() -> Result<u32, String> {
    let tray = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    if tray.is_null() {
        return Err("Không tìm thấy taskbar Windows (Shell_TrayWnd).".into());
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(tray, &mut pid);
    if pid == 0 {
        return Err("Không đọc được PID của explorer.exe.".into());
    }
    Ok(pid)
}

unsafe fn attach_tap(dll_path: &Path, ready_event: HANDLE) -> Result<(), String> {
    let pid = explorer_pid()?;
    let xaml = LoadLibraryExW(
        wide("Windows.UI.Xaml.dll").as_ptr(),
        null_mut(),
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    );
    if xaml.is_null() {
        return Err("Không nạp được Windows.UI.Xaml.dll.".into());
    }
    let proc = GetProcAddress(xaml, c"InitializeXamlDiagnosticsEx".as_ptr().cast());
    let Some(proc) = proc else {
        FreeLibrary(xaml);
        return Err("Không tìm thấy InitializeXamlDiagnosticsEx trong Windows.UI.Xaml.dll.".into());
    };
    let init: InitXamlDiagExFn = std::mem::transmute(proc);
    let dll_w = wide(&dll_path.to_string_lossy());
    ResetEvent(ready_event);

    let mut last_hr = -1i32;
    for idx in 1..=32 {
        let endpoint = wide(&format!("VisualDiagConnection{idx}"));
        let hr = init(
            endpoint.as_ptr(),
            pid,
            null(),
            dll_w.as_ptr(),
            CLSID_PUWBAR_TAP,
            null(),
        );
        if hr >= 0 {
            WaitForSingleObject(ready_event, 2000);
            FreeLibrary(xaml);
            return Ok(());
        }
        last_hr = hr;
    }
    FreeLibrary(xaml);
    Err(format!(
        "Không gắn được PuwBar vào XAML Taskbar (HRESULT 0x{:08X}).",
        last_hr as u32
    ))
}

unsafe fn attach_tap_with_retry(
    dll_path: &Path,
    ready_event: HANDLE,
    max_wait: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + max_wait;
    loop {
        match attach_tap(dll_path, ready_event) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if std::time::Instant::now() >= deadline {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    }
}

struct SessionGuard {
    stop_event: HANDLE,
    ready_event: HANDLE,
    reload_event: HANDLE,
    mutex: HANDLE,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.stop_event.is_null() {
                SetEvent(self.stop_event);
                std::thread::sleep(Duration::from_millis(250));
                CloseHandle(self.stop_event);
            }
            if !self.ready_event.is_null() {
                CloseHandle(self.ready_event);
            }
            if !self.reload_event.is_null() {
                CloseHandle(self.reload_event);
            }
            if !self.mutex.is_null() {
                CloseHandle(self.mutex);
            }
        }
    }
}

fn autostart_command(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

fn task_file() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join(r"System32\Tasks")
        .join(TASK_NAME)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn task_xml(exe: &Path, user: &str) -> String {
    let user = xml_escape(user);
    let command = xml_escape(&exe.display().to_string());
    let dir = xml_escape(
        &exe.parent()
            .map_or(String::new(), |d| d.display().to_string()),
    );
    // Launch directly at logon with zero delay and above-normal priority so PuwBar styles the taskbar before other startup apps.
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>\r\n\
  <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>\r\n\
  <Settings>\r\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\r\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\r\n\
    <Priority>4</Priority>\r\n\
    <Enabled>true</Enabled>\r\n\
  </Settings>\r\n\
  <Actions Context=\"Author\"><Exec><Command>{command}</Command><WorkingDirectory>{dir}</WorkingDirectory></Exec></Actions>\r\n\
</Task>\r\n"
    )
}

unsafe fn schtasks_as_admin(args: &str) -> bool {
    let verb = wide("runas");
    let file = wide("schtasks.exe");
    let params = wide(args);
    let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
        return false;
    }
    WaitForSingleObject(info.hProcess, 30_000);
    let mut code = 1u32;
    GetExitCodeProcess(info.hProcess, &mut code);
    CloseHandle(info.hProcess);
    code == 0
}

unsafe fn run_entry_value() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut bytes = (buf.len() * size_of::<u16>()) as u32;
    let ok = RegGetValueW(
        HKEY_CURRENT_USER,
        wide(RUN_KEY).as_ptr(),
        wide(RUN_VALUE).as_ptr(),
        RRF_RT_REG_SZ,
        null_mut(),
        buf.as_mut_ptr().cast(),
        &mut bytes,
    ) == 0;
    if !ok || bytes <= 2 {
        return None;
    }
    Some(String::from_utf16_lossy(
        &buf[..(bytes as usize / 2).saturating_sub(1)],
    ))
}

unsafe fn set_run_entry(enable: bool) {
    let mut hkey = null_mut();
    if RegOpenKeyExW(
        HKEY_CURRENT_USER,
        wide(RUN_KEY).as_ptr(),
        0,
        KEY_SET_VALUE,
        &mut hkey,
    ) != 0
    {
        return;
    }
    if enable {
        if let Ok(exe) = std::env::current_exe() {
            let cmd = wide(&autostart_command(&exe));
            RegSetValueExW(
                hkey,
                wide(RUN_VALUE).as_ptr(),
                0,
                REG_SZ,
                cmd.as_ptr().cast(),
                (cmd.len() * size_of::<u16>()) as u32,
            );
        }
    } else {
        RegDeleteValueW(hkey, wide(RUN_VALUE).as_ptr());
    }
    RegCloseKey(hkey);
}

unsafe fn is_autostart_enabled() -> bool {
    if task_file().exists() {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    run_entry_value().is_some_and(|v| v.eq_ignore_ascii_case(&autostart_command(&exe)))
}

unsafe fn refresh_autostart() {
    if run_entry_value().is_some() && !is_autostart_enabled() {
        set_run_entry(true);
    }
}

unsafe fn set_autostart(enable: bool) {
    if !enable {
        set_run_entry(false);
        if task_file().exists() {
            std::thread::spawn(|| {
                schtasks_as_admin(&format!("/delete /tn {TASK_NAME} /f"));
            });
        }
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    std::thread::spawn(move || {
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let file = std::env::temp_dir().join("puwbar_task.xml");
        let utf16: Vec<u8> = [0xFEFFu16]
            .into_iter()
            .chain(task_xml(&exe, &user).encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let made = std::fs::write(&file, utf16).is_ok()
            && schtasks_as_admin(&format!(
                "/create /tn {TASK_NAME} /xml \"{}\" /f",
                file.display()
            ));
        let _ = std::fs::remove_file(&file);
        if made {
            set_run_entry(false);
        } else {
            // Fallback to HKCU Run if UAC was declined.
            set_run_entry(true);
        }
    });
}

unsafe fn load_app_icon(instance: HINSTANCE) -> HICON {
    let icon = LoadImageW(
        instance,
        std::ptr::without_provenance::<u16>(1),
        IMAGE_ICON,
        0,
        0,
        LR_DEFAULTSIZE | LR_SHARED,
    ) as HICON;
    if icon.is_null() {
        LoadIconW(null_mut(), IDI_APPLICATION)
    } else {
        icon
    }
}

unsafe fn add_tray_icon(hwnd: HWND, icon: HICON) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = TRAY_CALLBACK;
    nid.hIcon = icon;
    let tip = wide("PuwBar — Taskbar kính mờ");
    let count = tip.len().min(nid.szTip.len());
    nid.szTip[..count].copy_from_slice(&tip[..count]);
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let autostart = is_autostart_enabled();
    AppendMenuW(
        menu,
        MF_STRING | if autostart { MF_CHECKED } else { MF_UNCHECKED },
        CMD_AUTOSTART as usize,
        wide("Khởi động cùng Windows").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING,
        CMD_REAPPLY as usize,
        wide("Áp dụng lại giao diện").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(
        menu,
        MF_STRING,
        CMD_EXIT as usize,
        wide("Thoát PuwBar\tCtrl+Alt+F12").as_ptr(),
    );

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let choice = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    ) as u32;
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    match choice {
        CMD_AUTOSTART => set_autostart(!autostart),
        CMD_REAPPLY => {
            let reload = RELOAD_HANDLE.with(|r| r.get()) as HANDLE;
            if !reload.is_null() {
                SetEvent(reload);
            }
        }
        CMD_EXIT => PostQuitMessage(0),
        _ => {}
    }
}

thread_local! {
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DLL_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static READY_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static RELOAD_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static TRAY_ICON_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

unsafe extern "system" fn host_wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        TRAY_CALLBACK => {
            let ev = (l as u32) & 0xffff;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP || ev == WM_CONTEXTMENU {
                show_tray_menu(hwnd);
            }
            return 0;
        }
        WM_HOTKEY if w == EXIT_HOTKEY as usize => {
            PostQuitMessage(0);
            return 0;
        }
        WM_TIMER if w == 9 => {
            PostQuitMessage(0);
            return 0;
        }
        WM_CLOSE | WM_ENDSESSION => {
            PostQuitMessage(0);
            return 0;
        }
        WM_QUERYENDSESSION => return 1,
        _ => {}
    }
    if msg != 0 && msg == TASKBAR_CREATED.with(|c| c.get()) {
        // Explorer restarted: re-add tray icon and re-attach TAP after a brief delay.
        let icon = TRAY_ICON_HANDLE.with(|h| h.get()) as HICON;
        if !icon.is_null() {
            add_tray_icon(hwnd, icon);
        }
        std::thread::sleep(Duration::from_millis(500));
        let dll = DLL_PATH.with(|p| p.borrow().clone());
        let ready = READY_HANDLE.with(|r| r.get()) as HANDLE;
        if let Some(dll) = dll {
            let _ = attach_tap(&dll, ready);
        }
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}

pub fn run(seconds: Option<u32>) -> Result<(), String> {
    let dll_path = tap_dll_path()?;
    unsafe {
        let mutex = CreateMutexW(null(), 0, wide("Local\\PuwBar.Session").as_ptr());
        if mutex.is_null() {
            return Err("Không tạo được khóa phiên PuwBar.".into());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            return Ok(());
        }
        let stop_event = CreateEventW(null(), 1, 0, wide(STOP_EVENT_NAME).as_ptr());
        let ready_event = CreateEventW(null(), 1, 0, wide(READY_EVENT_NAME).as_ptr());
        let reload_event = CreateEventW(null(), 0, 0, wide(RELOAD_EVENT_NAME).as_ptr());
        if stop_event.is_null() || ready_event.is_null() || reload_event.is_null() {
            if !stop_event.is_null() {
                CloseHandle(stop_event);
            }
            if !ready_event.is_null() {
                CloseHandle(ready_event);
            }
            if !reload_event.is_null() {
                CloseHandle(reload_event);
            }
            CloseHandle(mutex);
            return Err("Không tạo được sự kiện điều khiển PuwBar.".into());
        }
        ResetEvent(stop_event);
        let _guard = SessionGuard {
            stop_event,
            ready_event,
            reload_event,
            mutex,
        };

        // Ensure native taskbar is visible if a previous run left it hidden.
        let tray = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        if !tray.is_null() && IsWindowVisible(tray) == 0 {
            ShowWindow(tray, SW_SHOWNA);
        }

        refresh_autostart();
        attach_tap_with_retry(&dll_path, ready_event, Duration::from_secs(30))?;

        let class = wide("PuwBar.Host");
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        let _ = RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("PuwBar").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err("Không tạo được cửa sổ điều khiển PuwBar.".into());
        }

        let icon = load_app_icon(instance);
        TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr())));
        DLL_PATH.with(|p| *p.borrow_mut() = Some(dll_path));
        READY_HANDLE.with(|r| r.set(ready_event as isize));
        RELOAD_HANDLE.with(|r| r.set(reload_event as isize));
        TRAY_ICON_HANDLE.with(|h| h.set(icon as isize));
        add_tray_icon(hwnd, icon);

        if RegisterHotKey(
            hwnd,
            EXIT_HOTKEY,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            VK_F12 as u32,
        ) == 0
        {
            remove_tray_icon(hwnd);
            DestroyWindow(hwnd);
            return Err("Ctrl+Alt+F12 đang được ứng dụng khác sử dụng.".into());
        }
        if let Some(secs) = seconds {
            SetTimer(hwnd, 9, secs.clamp(1, 3600) * 1000, None);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnregisterHotKey(hwnd, EXIT_HOTKEY);
        remove_tray_icon(hwnd);
        DestroyWindow(hwnd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_command_quotes_executable_path() {
        let cmd = autostart_command(Path::new(r"C:\Program Files\PuwBar\puwbar.exe"));
        assert_eq!(cmd, r#""C:\Program Files\PuwBar\puwbar.exe""#);
    }

    #[test]
    fn scheduled_task_starts_immediately_at_logon_without_delay() {
        let xml = task_xml(Path::new(r"C:\Tools\puwbar.exe"), r"PC\User");
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains("<Command>C:\\Tools\\puwbar.exe</Command>"));
        assert!(
            !xml.contains("<Delay>"),
            "taskbar must start before delayed startup apps"
        );
    }
}
