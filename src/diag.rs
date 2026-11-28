//! Diagnostic report for layout bugs seen on other machines: facts about Windows, the taskbar and PuwBar's settings,
//! plus the taskbar's XAML tree measured inside Explorer by the tap. It is written next to puwbar.exe.

use crate::{desktop::wide, tap};
use std::{
    fmt::Write as _,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::atomic::{AtomicIsize, Ordering},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    System::{
        Registry::*,
        Threading::{CreateEventW, SetEvent},
    },
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

const DIAG_EVENT: &str = "Local\\PuwBar.DiagEvent";
const REPORT_NAME: &str = "puwbar_diagnose.txt";
const TAP_FILE: &str = "puwbar_diag_tap.txt";
static DIAG: AtomicIsize = AtomicIsize::new(0);

/// Creates the named event the tap in Explorer waits on. Called once at start-up, before the tap is attached.
pub fn create_event() {
    unsafe {
        let handle = CreateEventW(null(), 0, 0, wide(DIAG_EVENT).as_ptr());
        DIAG.store(handle as isize, Ordering::SeqCst);
    }
}

unsafe fn reg_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let (mut v, mut n) = (0u32, 4u32);
    let ok = RegGetValueW(root, wide(key).as_ptr(), wide(value).as_ptr(), RRF_RT_REG_DWORD, null_mut(), (&mut v as *mut u32).cast(), &mut n) == 0;
    ok.then_some(v)
}

unsafe fn reg_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut n = (buf.len() * 2) as u32;
    let ok = RegGetValueW(root, wide(key).as_ptr(), wide(value).as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut n) == 0;
    ok.then(|| String::from_utf16_lossy(&buf[..(n as usize / 2).saturating_sub(1)]))
}

unsafe fn reg_bytes(root: HKEY, key: &str, value: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    let mut n = buf.len() as u32;
    let ok = RegGetValueW(root, wide(key).as_ptr(), wide(value).as_ptr(), RRF_RT_REG_BINARY, null_mut(), buf.as_mut_ptr().cast(), &mut n) == 0;
    ok.then(|| {
        buf.truncate(n as usize);
        buf
    })
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("(not set)".into(), |v| v.to_string())
}

/// Position, size, visibility, monitor and DPI of one taskbar window.
unsafe fn window_line(class: &str) -> String {
    let hwnd = FindWindowW(wide(class).as_ptr(), null());
    if hwnd.is_null() {
        return format!("{class}: not found");
    }
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rc);
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (0u32, 0u32);
    GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    format!(
        "{class}: window=({},{},{},{}) {}x{} visible={} monitor=({},{},{},{}) work=({},{},{},{}) dpi={}",
        rc.left,
        rc.top,
        rc.right,
        rc.bottom,
        rc.right - rc.left,
        rc.bottom - rc.top,
        IsWindowVisible(hwnd),
        mi.rcMonitor.left,
        mi.rcMonitor.top,
        mi.rcMonitor.right,
        mi.rcMonitor.bottom,
        mi.rcWork.left,
        mi.rcWork.top,
        mi.rcWork.right,
        mi.rcWork.bottom,
        dx
    )
}

/// The report text: machine facts first, then the XAML tree from the tap.
unsafe fn build(tree: &str) -> String {
    let nt = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let adv = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced";
    let mut r = String::new();
    let _ = writeln!(r, "PuwBar diagnostic report");
    let _ = writeln!(r, "puwbar: v{}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(r, "exe: {}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default());
    let _ = writeln!(r, "windows: {} {} build {}.{}", opt(reg_string(HKEY_LOCAL_MACHINE, nt, "ProductName")), opt(reg_string(HKEY_LOCAL_MACHINE, nt, "DisplayVersion")), opt(reg_string(HKEY_LOCAL_MACHINE, nt, "CurrentBuild")), opt(reg_dword(HKEY_LOCAL_MACHINE, nt, "UBR")));
    let prefs = tap::prefs();
    let _ = writeln!(r, "puwbar settings: style={} surfaces={:#04x} english={} autostart={}", prefs.style, prefs.surfaces, prefs.english, prefs.autostart);
    let _ = writeln!(r, "\n[taskbar]");
    let _ = writeln!(r, "{}", window_line("Shell_TrayWnd"));
    let _ = writeln!(r, "{}", window_line("Shell_SecondaryTrayWnd"));
    let _ = writeln!(r, "TaskbarAl (0 left, 1 center) = {}", opt(reg_dword(HKEY_CURRENT_USER, adv, "TaskbarAl")));
    let _ = writeln!(r, "TaskbarDa (widgets) = {}", opt(reg_dword(HKEY_CURRENT_USER, adv, "TaskbarDa")));
    let _ = writeln!(r, "TaskbarMn (chat) = {}", opt(reg_dword(HKEY_CURRENT_USER, adv, "TaskbarMn")));
    let _ = writeln!(r, "ShowTaskViewButton = {}", opt(reg_dword(HKEY_CURRENT_USER, adv, "ShowTaskViewButton")));
    let _ = writeln!(r, "SearchboxTaskbarMode = {}", opt(reg_dword(HKEY_CURRENT_USER, "Software\\Microsoft\\Windows\\CurrentVersion\\Search", "SearchboxTaskbarMode")));
    let stuck = reg_bytes(HKEY_CURRENT_USER, "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3", "Settings");
    let hex = stuck.map(|b| b.iter().take(40).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    let _ = writeln!(r, "StuckRects3 (byte 12 is the edge: 0 left, 1 top, 2 right, 3 bottom) = {}", opt(hex));
    let _ = writeln!(r, "\n[xaml tree]\n{tree}");
    r
}

/// Where the report goes: next to puwbar.exe, or the temp folder when that folder is not writable.
fn write_report(text: &str) -> Result<PathBuf, String> {
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(REPORT_NAME)));
    if let Some(path) = beside {
        if std::fs::write(&path, text).is_ok() {
            return Ok(path);
        }
    }
    let fallback = std::env::temp_dir().join(REPORT_NAME);
    std::fs::write(&fallback, text).map(|_| fallback).map_err(|e| e.to_string())
}

/// Asks the tap for the XAML tree, builds the report, tells the user where it went and shows it in Explorer.
/// Blocks for up to a few seconds, so call it from a worker thread.
pub fn run_and_report() {
    let english = tap::prefs().english;
    let tap_file = std::env::temp_dir().join(TAP_FILE);
    let _ = std::fs::remove_file(&tap_file);
    let mut tree = String::new();
    unsafe {
        let event = DIAG.load(Ordering::SeqCst) as HANDLE;
        if !event.is_null() && SetEvent(event) != 0 {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(6) {
                if std::fs::metadata(&tap_file).is_ok_and(|m| m.len() > 0) {
                    std::thread::sleep(Duration::from_millis(250));
                    tree = std::fs::read(&tap_file).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if tree.is_empty() {
            tree = "(no answer from the tap in Explorer within 6 seconds: PuwBar has not attached yet, or Explorer still runs an older tap build)".into();
        }
        let text = build(&tree);
        let (title, body) = match write_report(&text) {
            Ok(path) => {
                let arg = format!("/select,\"{}\"", path.display());
                ShellExecuteW(null_mut(), wide("open").as_ptr(), wide("explorer.exe").as_ptr(), wide(&arg).as_ptr(), null(), SW_SHOWNORMAL);
                if english {
                    ("PuwBar diagnostics", format!("Report saved to:\n{}\n\nSend this file back.", path.display()))
                } else {
                    ("Chẩn đoán PuwBar", format!("Đã ghi báo cáo vào:\n{}\n\nAnh gửi file này lại cho bé.", path.display()))
                }
            }
            Err(e) => ("PuwBar diagnostics", format!("Could not write the report: {e}")),
        };
        MessageBoxW(null_mut(), wide(&body).as_ptr(), wide(title).as_ptr(), MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND);
    }
}
