use crate::desktop::{class_name, wide};
use puwbar::model::group_key;
use std::{
    collections::HashMap,
    mem::size_of,
    path::Path,
    ptr::{null, null_mut},
};
use windows::core::{Interface, PCWSTR};
use windows::Win32::{
    System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ},
    UI::Shell::{IShellLinkW, ShellLink},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Dwm::*,
    Storage::Packaging::Appx::GetApplicationUserModelId,
    System::Threading::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub struct Icon(pub HICON);
impl Drop for Icon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DestroyIcon(self.0);
            }
        }
    }
}

pub struct Group {
    pub key: String,
    pub name: String,
    pub launch: String,
    pub icon: Icon,
    pub windows: Vec<(HWND, String)>,
    pub pinned: bool,
}

pub fn file_icon(path: &str) -> Icon {
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    unsafe {
        SHGetFileInfoW(
            wide(path).as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
    }
    Icon(info.hIcon)
}

pub fn pinned() -> Vec<Group> {
    let mut groups = Vec::new();
    let dir = std::env::var("APPDATA").unwrap_or_default()
        + "/Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar";
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let launch = path.to_string_lossy().into_owned();
            let target = unsafe {
                let link: Result<IShellLinkW, _> =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER);
                link.ok()
                    .and_then(|link| {
                        let persist: IPersistFile = link.cast().ok()?;
                        persist
                            .Load(PCWSTR(wide(&launch).as_ptr()), STGM_READ)
                            .ok()?;
                        let mut buf = [0u16; 32768];
                        link.GetPath(&mut buf, null_mut(), 0).ok()?;
                        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                        Some(String::from_utf16_lossy(&buf[..len]))
                    })
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| launch.clone())
            };
            let key = group_key(&target, "");
            if groups.iter().any(|g: &Group| g.key == key) {
                continue;
            }
            groups.push(Group {
                key,
                name: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                icon: file_icon(&launch),
                launch,
                windows: Vec::new(),
                pinned: true,
            });
        }
    }
    if groups.is_empty() {
        groups.push(Group {
            key: group_key("C:\\Windows\\explorer.exe", ""),
            name: "File Explorer".into(),
            launch: "explorer.exe".into(),
            icon: file_icon("C:\\Windows\\explorer.exe"),
            windows: Vec::new(),
            pinned: true,
        });
    }
    groups
}

pub fn refresh(groups: &mut Vec<Group>) {
    struct Window {
        hwnd: HWND,
        title: String,
        pid: u32,
    }
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 || GetWindowTextLengthW(hwnd) == 0 {
            return 1;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW != 0
            || (!GetWindow(hwnd, GW_OWNER).is_null() && ex & WS_EX_APPWINDOW == 0)
        {
            return 1;
        }
        if matches!(
            class_name(hwnd).as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
        ) {
            return 1;
        }
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut _ as _,
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return 1;
        }
        let mut title = [0u16; 1024];
        let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        (&mut *(data as *mut Vec<Window>)).push(Window {
            hwnd,
            title: String::from_utf16_lossy(&title[..len.max(0) as usize]),
            pid,
        });
        1
    }
    let mut windows = Vec::<Window>::new();
    unsafe {
        EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM);
    }
    for group in groups.iter_mut() {
        group.windows.clear();
    }
    let mut processes = HashMap::new();
    for window in windows {
        let (path, id) = processes
            .entry(window.pid)
            .or_insert_with(|| process_identity(window.pid));
        if path.is_empty() {
            continue;
        }
        let key = group_key(path, id);
        let index = groups
            .iter()
            .position(|g| g.key == key || (id.is_empty() && g.key == group_key(path, "")));
        if let Some(index) = index {
            groups[index].windows.push((window.hwnd, window.title));
        } else {
            let mut icon = unsafe {
                let mut result = 0;
                SendMessageTimeoutW(
                    window.hwnd,
                    WM_GETICON,
                    ICON_BIG as usize,
                    0,
                    SMTO_ABORTIFHUNG,
                    25,
                    &mut result,
                );
                if result == 0 {
                    result = GetClassLongPtrW(window.hwnd, GCLP_HICON);
                }
                Icon(if result == 0 {
                    null_mut()
                } else {
                    CopyIcon(result as HICON)
                })
            };
            if icon.0.is_null() {
                icon = file_icon(path);
            }
            let launch = if id.is_empty() {
                path.clone()
            } else {
                format!("shell:AppsFolder\\{id}")
            };
            groups.push(Group {
                key,
                name: Path::new(path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                launch,
                icon,
                windows: vec![(window.hwnd, window.title)],
                pinned: false,
            });
        }
    }
    groups.retain(|g| g.pinned || !g.windows.is_empty());
}

fn process_identity(pid: u32) -> (String, String) {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return (String::new(), String::new());
        }
        let mut path = [0u16; 32768];
        let mut count = path.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut count) != 0;
        let mut id = [0u16; 512];
        let mut id_len = id.len() as u32;
        let has_id =
            GetApplicationUserModelId(process, &mut id_len, id.as_mut_ptr()) == ERROR_SUCCESS;
        CloseHandle(process);
        (
            if ok {
                String::from_utf16_lossy(&path[..count as usize])
            } else {
                String::new()
            },
            if has_id {
                String::from_utf16_lossy(&id[..id_len.saturating_sub(1) as usize])
            } else {
                String::new()
            },
        )
    }
}

pub fn launch(owner: HWND, target: &str) {
    let result = unsafe {
        ShellExecuteW(
            owner,
            wide("open").as_ptr(),
            wide(target).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        crate::desktop::show_error(&format!("Không mở được {target} (mã {}).", result as isize));
    }
}

pub fn activate(hwnd: HWND) {
    unsafe {
        if IsWindow(hwnd) == 0 {
            return;
        }
        if GetForegroundWindow() == hwnd {
            ShowWindow(hwnd, SW_MINIMIZE);
        } else {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
}

pub fn fullscreen(monitor: &RECT) -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null()
            || matches!(
                class_name(hwnd).as_str(),
                "Progman" | "WorkerW" | "PuwBar.Window"
            )
        {
            return false;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return false;
        }
        rect.left <= monitor.left
            && rect.top <= monitor.top
            && rect.right >= monitor.right
            && rect.bottom >= monitor.bottom
    }
}
