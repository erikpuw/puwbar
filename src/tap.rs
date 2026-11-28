use crate::desktop::wide;
use std::{
    mem::size_of,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::ToolHelp::*,
            LibraryLoader::*,
            Registry::*,
            Threading::*,
        },
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
const CMD_ABOUT: u32 = 1005;
const CMD_EXIT: u32 = 1006;
const CMD_SETTINGS: u32 = 1007;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "PuwBar";
const APP_KEY: &str = "Software\\PuwBar";
const LANG_VALUE: &str = "Language";
const SURFACES_VALUE: &str = "Surfaces";
const STYLE_VALUE: &str = "Style";

const SURF_START_MENU: u32 = 1 << 0;
const SURF_CONTROL_CENTER: u32 = 1 << 1;
const SURF_NOTIFICATIONS: u32 = 1 << 2;
const SURF_CALENDAR: u32 = 1 << 3;
const SURF_JUMP_LIST: u32 = 1 << 4;
const SURF_TOOLTIP: u32 = 1 << 5;
const SURF_THUMBNAIL: u32 = 1 << 6;
const SURF_ALL: u32 = 0x7F;
const TASK_NAME: &str = "PuwBar";
const STOP_EVENT_NAME: &str = "Local\\PuwBar.StopEvent";
const READY_EVENT_NAME: &str = "Local\\PuwBar.TapReady";
const RELOAD_EVENT_NAME: &str = "Local\\PuwBar.ReloadEvent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Vi,
    En,
}

fn parse_lang(code: &str) -> Lang {
    if code.trim().eq_ignore_ascii_case("en") {
        Lang::En
    } else {
        Lang::Vi
    }
}

unsafe fn read_lang() -> Lang {
    let mut buf = [0u16; 32];
    let mut bytes = (buf.len() * size_of::<u16>()) as u32;
    let ok = RegGetValueW(
        HKEY_CURRENT_USER,
        wide(APP_KEY).as_ptr(),
        wide(LANG_VALUE).as_ptr(),
        RRF_RT_REG_SZ,
        null_mut(),
        buf.as_mut_ptr().cast(),
        &mut bytes,
    ) == 0;
    if !ok || bytes <= 2 {
        return Lang::Vi;
    }
    let s = String::from_utf16_lossy(&buf[..(bytes as usize / 2).saturating_sub(1)]);
    parse_lang(&s)
}

unsafe fn save_lang(lang: Lang) {
    let mut hkey = null_mut();
    if RegCreateKeyExW(
        HKEY_CURRENT_USER,
        wide(APP_KEY).as_ptr(),
        0,
        null(),
        0,
        KEY_SET_VALUE,
        null(),
        &mut hkey,
        null_mut(),
    ) != 0
    {
        return;
    }
    let val = wide(match lang {
        Lang::Vi => "vi",
        Lang::En => "en",
    });
    RegSetValueExW(
        hkey,
        wide(LANG_VALUE).as_ptr(),
        0,
        REG_SZ,
        val.as_ptr().cast(),
        (val.len() * size_of::<u16>()) as u32,
    );
    RegCloseKey(hkey);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskbarStyle {
    FrostedGlass = 0,
    CrystalClear = 1,
    IconReflect = 2,
    FlatFrosted = 3,
    Clear = 4,
}

impl TaskbarStyle {
    fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::CrystalClear,
            2 => Self::IconReflect,
            3 => Self::FlatFrosted,
            4 => Self::Clear,
            _ => Self::FrostedGlass,
        }
    }
}

fn format_glass_cfg(surfaces: u32, style: TaskbarStyle) -> String {
    format!("{:02x}:{}", surfaces & SURF_ALL, style as u32)
}

fn sync_glass_cfg_file(surfaces: u32, style: TaskbarStyle) {
    static ACL_GRANTED: std::sync::Once = std::sync::Once::new();
    let cfg = std::env::temp_dir().join("puwbar_dark_tint.cfg");
    if std::fs::write(&cfg, format_glass_cfg(surfaces, style)).is_ok() {
        ACL_GRANTED.call_once(|| grant_appcontainer_read(&cfg));
    }
}

unsafe fn read_reg_dword(value_name: &str, default: u32) -> u32 {
    let mut val = default;
    let mut bytes = size_of::<u32>() as u32;
    let ok = RegGetValueW(
        HKEY_CURRENT_USER,
        wide(APP_KEY).as_ptr(),
        wide(value_name).as_ptr(),
        RRF_RT_REG_DWORD,
        null_mut(),
        (&raw mut val).cast(),
        &mut bytes,
    ) == 0;
    if ok { val } else { default }
}

unsafe fn save_reg_dword(value_name: &str, val: u32) {
    let mut hkey = null_mut();
    if RegCreateKeyExW(
        HKEY_CURRENT_USER,
        wide(APP_KEY).as_ptr(),
        0,
        null(),
        0,
        KEY_SET_VALUE,
        null(),
        &mut hkey,
        null_mut(),
    ) == 0
    {
        RegSetValueExW(
            hkey,
            wide(value_name).as_ptr(),
            0,
            REG_DWORD,
            (&raw const val).cast(),
            size_of::<u32>() as u32,
        );
        RegCloseKey(hkey);
    }
}

unsafe fn read_surfaces_mask() -> u32 {
    read_reg_dword(SURFACES_VALUE, SURF_ALL) & SURF_ALL
}

unsafe fn read_taskbar_style() -> TaskbarStyle {
    TaskbarStyle::from_u32(read_reg_dword(STYLE_VALUE, TaskbarStyle::FrostedGlass as u32))
}

unsafe fn save_surfaces_mask(mask: u32) {
    let clean = mask & SURF_ALL;
    save_reg_dword(SURFACES_VALUE, clean);
    sync_glass_cfg_file(clean, read_taskbar_style());
}

unsafe fn save_taskbar_style(style: TaskbarStyle) {
    save_reg_dword(STYLE_VALUE, style as u32);
    sync_glass_cfg_file(read_surfaces_mask(), style);
}

/// Snapshot of every user preference the settings window shows.
pub(crate) struct Prefs {
    pub style: u32,
    pub surfaces: u32,
    pub autostart: bool,
    pub english: bool,
}

pub(crate) const SURFACE_BITS: [u32; 7] = [
    SURF_START_MENU,
    SURF_CONTROL_CENTER,
    SURF_NOTIFICATIONS,
    SURF_CALENDAR,
    SURF_JUMP_LIST,
    SURF_TOOLTIP,
    SURF_THUMBNAIL,
];

pub(crate) fn prefs() -> Prefs {
    unsafe {
        Prefs {
            style: read_taskbar_style() as u32,
            surfaces: read_surfaces_mask(),
            autostart: is_autostart_enabled(),
            english: read_lang() == Lang::En,
        }
    }
}

unsafe fn signal_reload() {
    let reload = RELOAD_HANDLE.with(|r| r.get()) as HANDLE;
    if !reload.is_null() {
        SetEvent(reload);
    }
}

pub(crate) fn apply_style(style: u32) {
    unsafe {
        save_taskbar_style(TaskbarStyle::from_u32(style));
        signal_reload();
    }
}

pub(crate) fn toggle_surface(bit: u32) {
    unsafe {
        save_surfaces_mask(read_surfaces_mask() ^ bit);
        signal_reload();
    }
}

pub(crate) fn apply_autostart(enable: bool) {
    unsafe { set_autostart(enable) }
}

pub(crate) fn apply_language(hwnd: HWND, english: bool) {
    unsafe {
        let lang = if english { Lang::En } else { Lang::Vi };
        save_lang(lang);
        let icon = TRAY_ICON_HANDLE.with(|h| h.get()) as HICON;
        if !icon.is_null() {
            update_tray_icon(hwnd, icon, lang, NIM_MODIFY);
        }
    }
}

pub(crate) fn reapply() {
    unsafe { signal_reload() }
}

pub(crate) fn quit() {
    unsafe { PostQuitMessage(0) }
}

type InitXamlDiagExFn = unsafe extern "system" fn(
    end_point_name: *const u16,
    pid: u32,
    wsz_dll_xaml_diagnostics: *const u16,
    wsz_tap_dll_name: *const u16,
    tap_clsid: GUID,
    wsz_initialization_data: *const u16,
) -> i32;

const EMBEDDED_TAP_DLL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/puwbar_tap.dll"));

fn grant_appcontainer_read(path: &Path) {
    // Allow UWP AppContainer hosts (ShellExperienceHost / StartMenuExperienceHost) to load the staged TAP DLL.
    let _ = Command::new("icacls.exe")
        .arg(path)
        .args(["/grant", "*S-1-15-2-1:(RX)", "/Q"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

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
        grant_appcontainer_read(&staged);
    }
    staged
        .canonicalize()
        .map(|p| {
            let s = p.to_string_lossy();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
        })
        .map_err(|e| format!("Không chuẩn hóa được đường dẫn DLL: {e}"))
}

unsafe fn find_process_pid(target_exe: &str) -> Option<u32> {
    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if snap.is_null() || snap == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry: PROCESSENTRY32W = std::mem::zeroed();
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut found = None;
    if Process32FirstW(snap, &mut entry) != 0 {
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(target_exe) {
                found = Some(entry.th32ProcessID);
                break;
            }
            if Process32NextW(snap, &mut entry) == 0 {
                break;
            }
        }
    }
    CloseHandle(snap);
    found
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

unsafe fn attach_tap_pid(
    pid: u32,
    dll_path: &Path,
    ready_event: Option<HANDLE>,
) -> Result<(), String> {
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
    if let Some(ev) = ready_event {
        ResetEvent(ev);
    }

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
            if let Some(ev) = ready_event {
                WaitForSingleObject(ev, 2000);
            }
            FreeLibrary(xaml);
            return Ok(());
        }
        last_hr = hr;
    }
    FreeLibrary(xaml);
    Err(format!(
        "Không gắn được PuwBar vào XAML (PID {pid}, HRESULT 0x{:08X}).",
        last_hr as u32
    ))
}

unsafe fn attach_tap(dll_path: &Path, ready_event: HANDLE) -> Result<(), String> {
    let pid = explorer_pid()?;
    attach_tap_pid(pid, dll_path, Some(ready_event))
}

fn spawn_auxiliary_hosts_watcher(dll_path: PathBuf, stop_event: HANDLE) {
    let stop_raw = stop_event as usize;
    std::thread::spawn(move || unsafe {
        let stop = stop_raw as HANDLE;
        let hosts = [
            "ShellExperienceHost.exe",
            "ShellHost.exe",
            "StartMenuExperienceHost.exe",
        ];
        let mut attached = [0u32; 3];
        loop {
            for (i, &exe) in hosts.iter().enumerate() {
                if let Some(pid) = find_process_pid(exe) {
                    if pid != attached[i] && attach_tap_pid(pid, &dll_path, None).is_ok() {
                        attached[i] = pid;
                    }
                } else {
                    attached[i] = 0;
                }
            }
            if WaitForSingleObject(stop, 250) == WAIT_OBJECT_0 {
                break;
            }
        }
    });
}

unsafe fn explorer_xaml_ready() -> bool {
    let tray = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    if tray.is_null() {
        return false;
    }
    !FindWindowExW(
        tray,
        null_mut(),
        wide("Windows.UI.Composition.DesktopWindowContentBridge").as_ptr(),
        null(),
    )
    .is_null()
}

/// Calls `f` with every native taskbar window (the primary one and those on secondary monitors).
unsafe fn for_each_taskbar(mut f: impl FnMut(HWND)) {
    for class in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        let class_w = wide(class);
        let mut prev: HWND = null_mut();
        loop {
            let hwnd = FindWindowExW(null_mut(), prev, class_w.as_ptr(), null());
            if hwnd.is_null() {
                break;
            }
            f(hwnd);
            prev = hwnd;
        }
    }
}

/// Makes sure no native taskbar is left hidden (by a veil that was cut short, or a previous run).
unsafe fn show_hidden_taskbars() {
    for_each_taskbar(|hwnd| {
        if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_SHOWNA);
        }
    });
}

/// Hides the native taskbar windows the moment they exist and keeps them hidden until the tap has
/// restyled them (plus a short grace for the first elements), or `max_wait` passes. Without it, a
/// logon or an Explorer restart shows the stock taskbar for a moment before PuwBar's glass replaces it.
/// The caller must `ResetEvent` the tap-ready event first, or a stale signal ends the veil at once.
fn spawn_taskbar_veil(max_wait: Duration) {
    const GRACE: Duration = Duration::from_millis(600);
    std::thread::spawn(move || unsafe {
        const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
        let ready = OpenEventW(SYNCHRONIZE_ACCESS, 0, wide(READY_EVENT_NAME).as_ptr());
        let deadline = std::time::Instant::now() + max_wait;
        let mut styled_at: Option<std::time::Instant> = None;
        loop {
            for_each_taskbar(|hwnd| {
                if IsWindowVisible(hwnd) != 0 {
                    ShowWindow(hwnd, SW_HIDE);
                }
            });
            if styled_at.is_none() && !ready.is_null() && WaitForSingleObject(ready, 0) == WAIT_OBJECT_0 {
                styled_at = Some(std::time::Instant::now());
            }
            if styled_at.is_some_and(|t| t.elapsed() >= GRACE) || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        show_hidden_taskbars();
        if !ready.is_null() {
            CloseHandle(ready);
        }
    });
}

unsafe fn attach_tap_with_retry(
    dll_path: &Path,
    ready_event: HANDLE,
    max_wait: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + max_wait;
    // Wait tightly for Explorer's XAML taskbar bridge before probing COM endpoints.
    while !explorer_xaml_ready() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    loop {
        match attach_tap(dll_path, ready_event) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if std::time::Instant::now() >= deadline {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(50));
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

unsafe fn terminate_process_by_name(target_exe: &str) {
    if let Some(pid) = find_process_pid(target_exe) {
        let proc = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !proc.is_null() {
            let _ = TerminateProcess(proc, 0);
            CloseHandle(proc);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe {
            show_hidden_taskbars();
            // Signal Explorer TAP to restore native taskbar/tray cleanly without restarting Explorer.
            let cfg = std::env::temp_dir().join("puwbar_dark_tint.cfg");
            let _ = std::fs::write(&cfg, b"stop");
            if !self.stop_event.is_null() {
                SetEvent(self.stop_event);
                std::thread::sleep(Duration::from_millis(250));
                CloseHandle(self.stop_event);
            }
            // Restart stateless UWP flyout hosts so suspended hosts (Start Menu, Quick Settings,
            // Notifications/Calendar, Jump Lists) immediately revert to 100% native UI and unload the DLL.
            for host in [
                "ShellExperienceHost.exe",
                "ShellHost.exe",
                "StartMenuExperienceHost.exe",
            ] {
                terminate_process_by_name(host);
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

unsafe fn update_tray_icon(hwnd: HWND, icon: HICON, lang: Lang, action: u32) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = TRAY_CALLBACK;
    nid.hIcon = icon;
    let tip = wide(match lang {
        Lang::Vi => "PuwBar — Taskbar kính mờ",
        Lang::En => "PuwBar — Frosted glass taskbar",
    });
    let count = tip.len().min(nid.szTip.len());
    nid.szTip[..count].copy_from_slice(&tip[..count]);
    Shell_NotifyIconW(action, &nid);
}

unsafe fn add_tray_icon(hwnd: HWND, icon: HICON) {
    update_tray_icon(hwnd, icon, read_lang(), NIM_ADD);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn show_about_dialog(hwnd: HWND, lang: Lang) {
    let version = env!("CARGO_PKG_VERSION");
    let (title, body) = match lang {
        Lang::Vi => (
            "Giới thiệu PuwBar",
            format!(
                "PuwBar v{version}\n\n\
Tùy biến thanh taskbar gốc Windows 11 thành các cụm kính mờ nổi, giữ nguyên toàn bộ tính năng và system tray gốc.\n\n\
Tác giả: erikpuw\n\
GitHub: https://github.com/erikpuw/puwbar\n\
Phím tắt thoát nhanh: Ctrl+Alt+F12"
            ),
        ),
        Lang::En => (
            "About PuwBar",
            format!(
                "PuwBar v{version}\n\n\
Styles the native Windows 11 taskbar into floating frosted glass pills while keeping all native taskbar and system tray features.\n\n\
Author: erikpuw\n\
GitHub: https://github.com/erikpuw/puwbar\n\
Quick exit shortcut: Ctrl+Alt+F12"
            ),
        ),
    };
    MessageBoxW(
        hwnd,
        wide(&body).as_ptr(),
        wide(title).as_ptr(),
        MB_OK | MB_ICONINFORMATION,
    );
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let lang = read_lang();
    let (lbl_settings, lbl_about, lbl_exit) = match lang {
        Lang::Vi => ("Cài đặt PuwBar...", "Giới thiệu PuwBar", "Thoát PuwBar	Ctrl+Alt+F12"),
        Lang::En => ("PuwBar settings...", "About PuwBar", "Exit PuwBar	Ctrl+Alt+F12"),
    };
    AppendMenuW(menu, MF_STRING, CMD_SETTINGS as usize, wide(lbl_settings).as_ptr());
    AppendMenuW(menu, MF_STRING, CMD_ABOUT as usize, wide(lbl_about).as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, CMD_EXIT as usize, wide(lbl_exit).as_ptr());

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
        CMD_SETTINGS => crate::settings::open(hwnd),
        CMD_ABOUT => show_about_dialog(hwnd, lang),
        CMD_EXIT => PostQuitMessage(0),
        _ => {}
    }
}

const POSITION_TIMER: usize = 10;
/// How long after the taskbar changed edge its window is refreshed, so Explorer has finished moving it.
const REFRESH_DELAY: Duration = Duration::from_millis(2500);

thread_local! {
    static TRAY_RECT: std::cell::Cell<(i32, i32, i32, i32)> = const { std::cell::Cell::new((0, 0, 0, 0)) };
    static REFRESH_AT: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DLL_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static READY_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static RELOAD_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static TRAY_ICON_HANDLE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// A white square appears at the start corner of the taskbar after it changes edge, and stays until the window is
/// recomposed. When the taskbar window moves, hide and show it once after Explorer has settled.
unsafe fn refresh_taskbar_after_move() {
    let tray = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
    if tray.is_null() {
        return;
    }
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(tray, &mut rc);
    let now = (rc.left, rc.top, rc.right, rc.bottom);
    let before = TRAY_RECT.with(|c| c.replace(now));
    if before != (0, 0, 0, 0) && before != now {
        REFRESH_AT.with(|c| c.set(Some(std::time::Instant::now() + REFRESH_DELAY)));
        return;
    }
    let due = REFRESH_AT.with(|c| c.get()).is_some_and(|t| std::time::Instant::now() >= t);
    if due {
        REFRESH_AT.with(|c| c.set(None));
        for_each_taskbar(|hwnd| {
            ShowWindow(hwnd, SW_HIDE);
        });
        std::thread::sleep(Duration::from_millis(60));
        show_hidden_taskbars();
    }
}

unsafe extern "system" fn host_wnd_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        TRAY_CALLBACK => {
            let ev = (l as u32) & 0xffff;
            if ev == WM_LBUTTONUP {
                crate::settings::open(hwnd);
            } else if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
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
        WM_TIMER if w == POSITION_TIMER => {
            refresh_taskbar_after_move();
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
        let dll = DLL_PATH.with(|p| p.borrow().clone());
        let ready = READY_HANDLE.with(|r| r.get()) as HANDLE;
        if let Some(dll) = dll {
            ResetEvent(ready);
            spawn_taskbar_veil(Duration::from_secs(10));
            let _ = attach_tap_with_retry(&dll, ready, Duration::from_secs(10));
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
        // The diagnostics event must exist before the tap attaches, because the tap opens it by name.
        crate::diag::create_event();
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

        // Ensure native taskbars are visible if a previous run left them hidden, then veil them until styled.
        show_hidden_taskbars();
        ResetEvent(ready_event);
        spawn_taskbar_veil(Duration::from_secs(10));

        // Sync persisted enabled surfaces & taskbar style for both Explorer and AppContainer hosts before attaching.
        sync_glass_cfg_file(read_surfaces_mask(), read_taskbar_style());

        // Boost priority during initial attach so PuwBar styles Explorer as soon as XAML initializes.
        let proc = GetCurrentProcess();
        SetPriorityClass(proc, ABOVE_NORMAL_PRIORITY_CLASS);
        let attach_res = attach_tap_with_retry(&dll_path, ready_event, Duration::from_secs(30));
        SetPriorityClass(proc, NORMAL_PRIORITY_CLASS);
        attach_res?;

        refresh_autostart();
        spawn_auxiliary_hosts_watcher(dll_path.clone(), stop_event);

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
        SetTimer(hwnd, POSITION_TIMER, 400, None);

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

    #[test]
    fn language_parsing_defaults_to_vietnamese() {
        assert_eq!(parse_lang("en"), Lang::En);
        assert_eq!(parse_lang("EN"), Lang::En);
        assert_eq!(parse_lang("vi"), Lang::Vi);
        assert_eq!(parse_lang(""), Lang::Vi);
    }

    #[test]
    fn taskbar_style_and_glass_cfg_encode_and_parse() {
        assert_eq!(TaskbarStyle::from_u32(0), TaskbarStyle::FrostedGlass);
        assert_eq!(TaskbarStyle::from_u32(1), TaskbarStyle::CrystalClear);
        assert_eq!(TaskbarStyle::from_u32(2), TaskbarStyle::IconReflect);
        assert_eq!(TaskbarStyle::from_u32(3), TaskbarStyle::FlatFrosted);
        assert_eq!(TaskbarStyle::from_u32(4), TaskbarStyle::Clear);
        assert_eq!(TaskbarStyle::from_u32(99), TaskbarStyle::FrostedGlass);
        assert_eq!(format_glass_cfg(SURF_ALL, TaskbarStyle::FrostedGlass), "7f:0");
        assert_eq!(
            format_glass_cfg(SURF_START_MENU | SURF_CALENDAR, TaskbarStyle::CrystalClear),
            "09:1"
        );
        assert_eq!(format_glass_cfg(0, TaskbarStyle::IconReflect), "00:2");
        assert_eq!(format_glass_cfg(SURF_ALL, TaskbarStyle::FlatFrosted), "7f:3");
        assert_eq!(format_glass_cfg(SURF_ALL, TaskbarStyle::Clear), "7f:4");
    }
}
