use crate::desktop::{class_name, wide};
use puwbar::model::{group_key, pinned_order_rank};
use std::{
    collections::HashMap,
    mem::size_of,
    path::Path,
    ptr::{null, null_mut},
};
use windows::core::{Interface, PCWSTR};
use windows::Win32::{
    Storage::EnhancedStorage::PKEY_AppUserModel_ID,
    System::Com::StructuredStorage::{PropVariantClear, PropVariantToString},
    UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow},
};
use windows::Win32::{
    System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ},
    UI::{
        Controls::{IImageList, ILD_TRANSPARENT},
        Shell::{IShellLinkW, SHGetImageList, ShellLink, SHIL_EXTRALARGE},
    },
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
    pub app_id: String,
    pub name: String,
    pub launch: String,
    pub icon: Icon,
    pub windows: Vec<(HWND, String)>,
    pub pinned: bool,
}

pub fn file_icon(path: &str) -> Icon {
    // Shell APIs reject forward slashes even though Rust's filesystem accepts them.
    let path = path.replace('/', "\\");
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    unsafe {
        if SHGetFileInfoW(
            wide(&path).as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        ) != 0
        {
            let images: Result<IImageList, _> = SHGetImageList(SHIL_EXTRALARGE as i32);
            if let Ok(icon) =
                images.and_then(|images| images.GetIcon(info.iIcon, ILD_TRANSPARENT.0))
            {
                return Icon(icon.0);
            }
        }
        SHGetFileInfoW(
            wide(&path).as_ptr(),
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
            let launch = path.to_string_lossy().replace('/', "\\");
            let mut app_id = String::new();
            let target = unsafe {
                let link: Result<IShellLinkW, _> =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER);
                link.ok()
                    .and_then(|link| {
                        let persist: IPersistFile = link.cast().ok()?;
                        persist
                            .Load(PCWSTR(wide(&launch).as_ptr()), STGM_READ)
                            .ok()?;
                        app_id = explicit_app_id(link.cast().ok());
                        let mut buf = [0u16; 32768];
                        link.GetPath(&mut buf, null_mut(), 0).ok()?;
                        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                        if len > 0 {
                            return Some(String::from_utf16_lossy(&buf[..len]));
                        }
                        let mut icon_path = [0u16; 1024];
                        let mut index = 0;
                        link.GetIconLocation(&mut icon_path, &mut index).ok()?;
                        let len = icon_path
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(icon_path.len());
                        let icon_path = String::from_utf16_lossy(&icon_path[..len]);
                        if icon_path.to_lowercase().ends_with("\\explorer.exe") {
                            Some(std::env::var("SystemRoot").ok()? + "\\explorer.exe")
                        } else {
                            None
                        }
                    })
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| launch.clone())
            };
            let key = group_key(&target, &app_id);
            if groups.iter().any(|g: &Group| g.key == key) {
                continue;
            }
            groups.push(Group {
                key,
                app_id,
                name: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                icon: pinned_icon(&launch, &target),
                launch,
                windows: Vec::new(),
                pinned: true,
            });
        }
    }
    let order = native_taskbar_order(&mut groups);
    groups.sort_by_key(|g| pinned_order_rank(&g.name, &order));
    groups
}

fn pinned_icon(shortcut: &str, target: &str) -> Icon {
    let icon = file_icon(shortcut);
    if icon.0.is_null() {
        file_icon(target)
    } else {
        icon
    }
}

fn native_taskbar_order(pins: &mut Vec<Group>) -> Vec<String> {
    unsafe {
        use windows_sys::Win32::System::Registry::*;
        let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
        let value = wide("Favorites");
        let mut size = 0u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            null_mut(),
            &mut size,
        ) != ERROR_SUCCESS
            || size > 1024 * 1024
        {
            return Vec::new();
        }
        let mut data = vec![0u8; size as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            data.as_mut_ptr() as _,
            &mut size,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        data.truncate(size as usize);
        let mut order = Vec::new();
        let mut at = 0;
        while let Some(header) = data.get(at..at + 5) {
            if header[0] != 0 {
                break;
            }
            let len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
            let Some(end) = (at + 5).checked_add(len) else {
                break;
            };
            let Some(record) = data.get(at + 5..end) else {
                break;
            };
            if !valid_pidl(record) {
                break;
            }
            let pidl = record.as_ptr().cast();
            let mut ptr = null_mut();
            if SHGetNameFromIDList(pidl, SIGDN_PARENTRELATIVEPARSING, &mut ptr) >= 0
                && !ptr.is_null()
            {
                let id = PCWSTR(ptr).to_string().unwrap_or_default();
                windows_sys::Win32::System::Com::CoTaskMemFree(ptr.cast());
                if id.contains('!') && !id.contains(['\\', '/']) {
                    let key = group_key("", &id);
                    let mut info: SHFILEINFOW = std::mem::zeroed();
                    if SHGetFileInfoW(
                        pidl.cast(),
                        0,
                        &mut info,
                        size_of::<SHFILEINFOW>() as _,
                        SHGFI_PIDL | SHGFI_DISPLAYNAME | SHGFI_SYSICONINDEX,
                    ) != 0
                    {
                        let name = String::from_utf16_lossy(
                            &info.szDisplayName[..info
                                .szDisplayName
                                .iter()
                                .position(|x| *x == 0)
                                .unwrap_or(info.szDisplayName.len())],
                        );
                        order.push((at, name.clone()));
                        if !pins.iter().any(|g| g.key == key) {
                            let icon = SHGetImageList::<IImageList>(SHIL_EXTRALARGE as i32)
                                .and_then(|list| list.GetIcon(info.iIcon, ILD_TRANSPARENT.0))
                                .map(|h| Icon(h.0))
                                .unwrap_or(Icon(null_mut()));
                            pins.push(Group {
                                key,
                                app_id: id.clone(),
                                name,
                                launch: format!("shell:AppsFolder\\{id}"),
                                icon,
                                windows: vec![],
                                pinned: true,
                            });
                        }
                    }
                }
            }
            at = end;
        }
        for pin in pins
            .iter()
            .filter(|p| p.launch.to_lowercase().ends_with(".lnk"))
        {
            let needle: Vec<u8> = wide(&format!("{}.lnk", pin.name))
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
            if let Some(offset) = data.windows(needle.len()).position(|slice| slice == needle) {
                order.push((offset, pin.name.clone()));
            }
        }
        order.sort_by_key(|(offset, _)| *offset);
        order.into_iter().map(|(_, name)| name).collect()
    }
}

fn valid_pidl(mut bytes: &[u8]) -> bool {
    while let Some(h) = bytes.get(..2) {
        let len = u16::from_le_bytes([h[0], h[1]]) as usize;
        if len == 0 {
            return bytes.len() == 2;
        }
        if len < 2 || len > bytes.len() {
            return false;
        }
        bytes = &bytes[len..];
    }
    false
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
        if pid == std::process::id() {
            return 1;
        }
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
        let (path, process_id) = processes
            .entry(window.pid)
            .or_insert_with(|| process_identity(window.pid));
        if path.is_empty() {
            continue;
        }
        let window_id = unsafe {
            explicit_app_id(
                SHGetPropertyStoreForWindow(windows::Win32::Foundation::HWND(window.hwnd)).ok(),
            )
        };
        let id = if window_id.is_empty() {
            process_id.as_str()
        } else {
            window_id.as_str()
        };
        let key = group_key(path, id);
        let index = groups
            .iter()
            .position(|g| g.key == key)
            .or_else(|| groups.iter().position(|g| !id.is_empty() && g.app_id == id))
            .or_else(|| {
                if !id.is_empty() {
                    return None;
                }
                let path_key = group_key(path, "");
                let mut matches = groups
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.pinned && g.key.starts_with(&path_key));
                let (index, _) = matches.next()?;
                matches.next().is_none().then_some(index)
            })
            .or_else(|| {
                groups
                    .iter()
                    .position(|g| g.pinned && g.key == group_key(path, ""))
            });
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
            let launch = if process_id.is_empty() {
                path.clone()
            } else {
                format!("shell:AppsFolder\\{process_id}")
            };
            groups.push(Group {
                key,
                app_id: id.to_owned(),
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

unsafe fn explicit_app_id(store: Option<IPropertyStore>) -> String {
    let Some(mut value) = store.and_then(|s| s.GetValue(&PKEY_AppUserModel_ID).ok()) else {
        return String::new();
    };
    let mut buf = [0u16; 256];
    let ok = PropVariantToString(&value, &mut buf).is_ok();
    let _ = PropVariantClear(&mut value);
    if ok {
        String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(buf.len())])
    } else {
        String::new()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidl_records_require_whole_items_and_a_terminator() {
        assert!(valid_pidl(&[4, 0, 0x1f, 0x80, 0, 0]));
        for malformed in [&[1, 0][..], &[4, 0, 1], &[0, 0, 5], &[2, 0]] {
            assert!(!valid_pidl(malformed));
        }
    }

    #[test]
    fn shell_icon_accepts_paths_built_with_forward_slashes() {
        let path = std::env::var("SystemRoot").unwrap().replace('\\', "/") + "/explorer.exe";
        assert!(
            !file_icon(&path).0.is_null(),
            "Shell could not read icon from {path}"
        );
    }

    fn icon_pixels(icon: &Icon) -> (i32, Vec<u8>) {
        use windows_sys::Win32::Graphics::Gdi::*;
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            assert_ne!(GetIconInfo(icon.0, &mut info), 0);
            let mut bitmap: BITMAP = std::mem::zeroed();
            GetObjectW(
                info.hbmColor,
                size_of::<BITMAP>() as i32,
                &mut bitmap as *mut _ as _,
            );
            let mut pixels = vec![0u8; (bitmap.bmWidthBytes * bitmap.bmHeight) as usize];
            assert!(
                GetBitmapBits(info.hbmColor, pixels.len() as i32, pixels.as_mut_ptr() as _) > 0
            );
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            (bitmap.bmWidth, pixels)
        }
    }

    #[test]
    fn pinned_icons_preserve_shortcut_customization_at_large_size() {
        unsafe {
            windows_sys::Win32::System::Com::CoInitializeEx(null(), 2);
            let root = std::env::var("SystemRoot").unwrap();
            let exe = root.clone() + "\\explorer.exe";
            let custom = root + "\\System32\\shell32.dll";
            let path = std::env::temp_dir()
                .join(format!("puwbar-icon-fixture-{}.lnk", std::process::id()));
            let shortcut = path.to_string_lossy().to_string();
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(PCWSTR(wide(&exe).as_ptr())).unwrap();
            link.SetIconLocation(PCWSTR(wide(&custom).as_ptr()), 3)
                .unwrap();
            let persist: IPersistFile = link.cast().unwrap();
            persist
                .Save(PCWSTR(wide(&shortcut).as_ptr()), true)
                .unwrap();
            let chosen = icon_pixels(&pinned_icon(&shortcut, &exe));
            let default = icon_pixels(&file_icon(&exe));
            let _ = std::fs::remove_file(path);
            windows_sys::Win32::System::Com::CoUninitialize();
            assert_ne!(
                chosen.1, default.1,
                "Pinned icon must use the shortcut's custom icon"
            );
            assert!(
                chosen.0 >= 48,
                "Use at least 48px source icons, got {}",
                chosen.0
            );
        }
    }

    #[test]
    #[ignore = "reads this Windows user's real taskbar; run explicitly on the desktop"]
    fn native_pins_have_icons_and_follow_the_live_taskbar() {
        unsafe {
            windows_sys::Win32::System::Com::CoInitializeEx(null(), 2);
        }
        let pins = pinned();
        assert!(!pins.is_empty(), "Native taskbar buttons were not found");
        for p in &pins {
            println!(
                "pin: {} icon={} target={}",
                p.name,
                !p.icon.0.is_null(),
                p.key
            );
            assert!(!p.icon.0.is_null(), "{}", p.name);
        }
        unsafe {
            windows_sys::Win32::System::Com::CoUninitialize();
        }
    }
}
