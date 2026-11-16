use crate::{
    desktop::wide,
    shell::{file_icon, Icon},
};
use std::{
    collections::HashMap,
    mem::size_of,
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::*,
        System::{Com::CLSIDFromString, Registry::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

pub struct Item {
    pub name: String,
    pub icon: Icon,
    pub pid: u32,
    pub promoted: bool,
    order: usize,
}

unsafe fn value(key: &[u16], name: &str, kind: u32) -> Vec<u8> {
    let name = wide(name);
    let mut size = 0;
    let read = |buf, len| {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            kind,
            null_mut(),
            buf,
            len,
        )
    };
    if read(null_mut(), &mut size) != 0 || size > 1024 * 1024 {
        return vec![];
    }
    let mut buf = vec![0; size as usize];
    if read(buf.as_mut_ptr().cast(), &mut size) != 0 {
        return vec![];
    }
    buf.truncate(size as usize);
    buf
}

fn string(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>(),
    )
}

pub fn snapshot() -> Result<Vec<Item>, u32> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        (*(data as *mut Vec<HWND>)).push(hwnd);
        1
    }
    unsafe {
        // Explorer remembers old registrations. Include an entry only when the live Shell confirms its identifier.
        let root = "Control Panel\\NotifyIconSettings";
        let mut parent = null_mut();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(root).as_ptr(),
            0,
            KEY_ENUMERATE_SUB_KEYS,
            &mut parent,
        );
        if status != 0 {
            return Err(status);
        }
        let mut handles = Vec::<HWND>::new();
        EnumWindows(Some(collect), &mut handles as *mut _ as LPARAM);
        let mut child = null_mut();
        loop {
            child = FindWindowExW(HWND_MESSAGE, child, null(), null());
            if child.is_null() {
                break;
            }
            handles.push(child);
        }
        let mut processes = HashMap::<u32, String>::new();
        let mut candidates = HashMap::<String, Vec<(HWND, u32)>>::new();
        for h in handles {
            let mut pid = 0;
            GetWindowThreadProcessId(h, &mut pid);
            let path = processes.entry(pid).or_insert_with(|| {
                let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if p.is_null() {
                    return String::new();
                }
                let mut buf = [0u16; 32768];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(p, 0, buf.as_mut_ptr(), &mut len);
                CloseHandle(p);
                if ok == 0 {
                    String::new()
                } else {
                    String::from_utf16_lossy(&buf[..len as usize])
                }
            });
            if let Some(name) = Path::new(path).file_name() {
                candidates
                    .entry(name.to_string_lossy().to_lowercase())
                    .or_default()
                    .push((h, pid));
            }
        }
        let order: HashMap<u64, usize> = value(&wide(root), "UIOrderList", RRF_RT_REG_BINARY)
            .as_chunks::<8>()
            .0
            .iter()
            .enumerate()
            .map(|(i, b)| (u64::from_le_bytes(*b), i))
            .collect();
        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for idx in 0.. {
            let mut buf = [0u16; 128];
            let mut len = buf.len() as u32;
            let err = RegEnumKeyExW(
                parent,
                idx,
                buf.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            if err == ERROR_NO_MORE_ITEMS {
                break;
            }
            if err != 0 {
                RegCloseKey(parent);
                return Err(err);
            }
            let registration = String::from_utf16_lossy(&buf[..len as usize]);
            let key = wide(&format!("{root}\\{registration}"));
            let path = string(&value(&key, "ExecutablePath", RRF_RT_REG_SZ));
            let Some(name) = Path::new(&path).file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(windows) = candidates.get(&name.to_lowercase()) else {
                continue;
            };
            let uid = value(&key, "UID", RRF_RT_REG_DWORD)
                .try_into()
                .ok()
                .map(u32::from_le_bytes);
            let guid_text = string(&value(&key, "IconGuid", RRF_RT_REG_SZ));
            let mut guid: GUID = std::mem::zeroed();
            let has_guid =
                !guid_text.is_empty() && CLSIDFromString(wide(&guid_text).as_ptr(), &mut guid) >= 0;
            let mut rect = RECT::default();
            let mut identifier = NOTIFYICONIDENTIFIER {
                cbSize: size_of::<NOTIFYICONIDENTIFIER>() as _,
                ..std::mem::zeroed()
            };
            let found = if has_guid {
                identifier.guidItem = guid;
                (Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0).then_some(windows[0])
            } else if let Some(uid) = uid {
                identifier.uID = uid;
                windows.iter().copied().find(|(h, _)| {
                    identifier.hWnd = *h;
                    Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0
                })
            } else {
                None
            };
            let Some((hwnd, pid)) = found else { continue };
            if !seen.insert((if has_guid { 0 } else { hwnd as usize }, uid, guid_text)) {
                continue;
            }
            let tip = string(&value(&key, "InitialTooltip", RRF_RT_REG_SZ));
            let mut png = value(&key, "IconSnapshot", RRF_RT_REG_BINARY);
            let mut icon = Icon(null_mut());
            if png.starts_with(b"\x89PNG\r\n\x1a\n") {
                icon = Icon(CreateIconFromResourceEx(
                    png.as_mut_ptr(),
                    png.len() as _,
                    1,
                    0x30000,
                    0,
                    0,
                    0,
                ));
            }
            if icon.0.is_null() {
                icon = file_icon(&processes[&pid]);
            }
            items.push(Item {
                order: registration
                    .parse::<u64>()
                    .ok()
                    .and_then(|id| order.get(&id).copied())
                    .unwrap_or(usize::MAX),
                name: tip
                    .lines()
                    .find(|s| !s.trim().is_empty())
                    .unwrap_or(name)
                    .to_owned(),
                icon,
                pid,
                promoted: value(&key, "IsPromoted", RRF_RT_REG_DWORD).as_slice()
                    == 1u32.to_le_bytes(),
            });
        }
        RegCloseKey(parent);
        items.sort_by_cached_key(|i| (!i.promoted, i.order, i.name.to_lowercase()));
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a desktop with registered tray icons"]
    fn live_tray() {
        let entries = super::snapshot().unwrap();
        println!("Live tray entries: {}", entries.len());
        assert!(!entries.is_empty());
    }
}
