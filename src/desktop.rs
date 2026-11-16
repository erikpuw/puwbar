use std::{
    io::{BufRead, Write},
    mem::size_of,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Command, Stdio},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::Threading::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub fn show_error(text: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(text).as_ptr(),
            wide("PuwBar").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn taskbars() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        let name = class_name(hwnd);
        if name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd" {
            (&mut *(data as *mut Vec<HWND>)).push(hwnd);
        }
        1
    }
    let mut result = Vec::new();
    unsafe {
        EnumWindows(Some(collect), &mut result as *mut _ as LPARAM);
    }
    result
}

pub unsafe fn class_name(hwnd: HWND) -> String {
    let mut name = [0u16; 128];
    let count = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32);
    String::from_utf16_lossy(&name[..count.max(0) as usize])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub state: usize,
    pub work: [i32; 4],
    pub bars: Vec<(usize, bool)>,
}

impl Snapshot {
    pub fn capture() -> Result<Self, String> {
        let mut work: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0) == 0 {
                return Err("Không đọc được vùng làm việc của Windows.".into());
            }
            let mut data: APPBARDATA = std::mem::zeroed();
            data.cbSize = size_of::<APPBARDATA>() as u32;
            Ok(Self {
                state: SHAppBarMessage(ABM_GETSTATE, &mut data),
                work: [work.left, work.top, work.right, work.bottom],
                bars: taskbars()
                    .iter()
                    .map(|&h| (h as usize, IsWindowVisible(h) != 0))
                    .collect(),
            })
        }
    }

    pub fn encode(&self) -> String {
        format!(
            "{};{},{},{},{};{}",
            self.state,
            self.work[0],
            self.work[1],
            self.work[2],
            self.work[3],
            self.bars
                .iter()
                .map(|(h, v)| format!("{h},{}", u8::from(*v)))
                .collect::<Vec<_>>()
                .join(";")
        )
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut parts = text.trim().split(';');
        let state = parts.next()?.parse().ok()?;
        if state > 3 {
            return None;
        }
        let coords: Vec<i32> = parts
            .next()?
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let work: [i32; 4] = coords.try_into().ok()?;
        if work[0] >= work[2] || work[1] >= work[3] {
            return None;
        }
        let mut bars = Vec::new();
        for part in parts.filter(|p| !p.is_empty()) {
            let (h, v) = part.split_once(',')?;
            let visible = match v {
                "0" => false,
                "1" => true,
                _ => return None,
            };
            bars.push((h.parse().ok()?, visible));
        }
        Some(Self { state, work, bars })
    }

    pub fn hide(&self) {
        unsafe {
            let mut data: APPBARDATA = std::mem::zeroed();
            data.cbSize = size_of::<APPBARDATA>() as u32;
            data.hWnd = FindWindowW(wide("Shell_TrayWnd").as_ptr(), std::ptr::null());
            data.lParam = (self.state | ABS_AUTOHIDE as usize) as isize;
            SHAppBarMessage(ABM_SETSTATE, &mut data);
        }
        hide_taskbars();
    }

    pub fn restore(&self) {
        unsafe {
            let mut data: APPBARDATA = std::mem::zeroed();
            data.cbSize = size_of::<APPBARDATA>() as u32;
            data.hWnd = FindWindowW(wide("Shell_TrayWnd").as_ptr(), std::ptr::null());
            data.lParam = self.state as isize;
            SHAppBarMessage(ABM_SETSTATE, &mut data);
            for h in taskbars() {
                let visible = self
                    .bars
                    .iter()
                    .find(|(id, _)| *id == h as usize)
                    .map(|(_, v)| *v)
                    .unwrap_or(true);
                ShowWindow(h, if visible { SW_SHOWNA } else { SW_HIDE });
            }
            let mut work = RECT {
                left: self.work[0],
                top: self.work[1],
                right: self.work[2],
                bottom: self.work[3],
            };
            SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                &mut work as *mut _ as _,
                SPIF_SENDCHANGE,
            );
        }
    }
}

pub fn hide_taskbars() {
    for hwnd in taskbars() {
        unsafe {
            if IsWindowVisible(hwnd) != 0 {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

// The child keeps the process handle, so PID reuse cannot restore another session's taskbar.
pub struct Guard {
    child: std::process::Child,
    event: HANDLE,
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.event);
            WaitForSingleObject(self.child.as_raw_handle() as HANDLE, 5000);
            CloseHandle(self.event);
        }
    }
}

pub fn start_guard(snapshot: &Snapshot) -> Result<Guard, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(["--guard", &std::process::id().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Không khởi động được bộ khôi phục: {e}"))?;
    let mut input = child.stdin.take().ok_or("Thiếu kênh khôi phục")?;
    writeln!(input, "{}", snapshot.encode()).map_err(|e| e.to_string())?;
    drop(input);
    let output = child.stdout.take().ok_or("Thiếu phản hồi khôi phục")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(output).read_line(&mut line);
        let _ = tx.send(line);
    });
    if rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_default()
        .trim()
        != "READY"
    {
        let _ = child.kill();
        return Err("Bộ khôi phục chưa sẵn sàng. Taskbar Windows được giữ nguyên.".into());
    }
    let event = unsafe {
        OpenEventW(
            EVENT_MODIFY_STATE,
            0,
            wide(&format!("Local\\PuwBar.Restore.{}", std::process::id())).as_ptr(),
        )
    };
    if event.is_null() {
        let _ = child.kill();
        return Err("Không mở được tín hiệu khôi phục".into());
    }
    Ok(Guard { child, event })
}

pub fn guard(pid: u32) -> Result<(), String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let snapshot = Snapshot::decode(&line).ok_or("Dữ liệu khôi phục không hợp lệ")?;
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            return Err("Không mở được tiến trình chính".into());
        }
        let event = CreateEventW(
            std::ptr::null(),
            1,
            0,
            wide(&format!("Local\\PuwBar.Restore.{pid}")).as_ptr(),
        );
        if event.is_null() {
            CloseHandle(process);
            return Err("Không tạo được tín hiệu khôi phục".into());
        }
        println!("READY");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let handles = [process, event];
        WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE);
        snapshot.restore();
        CloseHandle(event);
        CloseHandle(process);
    }
    Ok(())
}

pub fn monitors() -> Vec<MONITORINFO> {
    unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(h, &mut info) != 0 {
            (&mut *(data as *mut Vec<MONITORINFO>)).push(info);
        }
        1
    }
    let mut result = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut result as *mut _ as LPARAM,
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_round_trips_original_taskbar_visibility() {
        let s = Snapshot {
            state: 1,
            work: [-1920, 0, 1920, 1040],
            bars: vec![(123, true), (456, false)],
        };
        assert_eq!(Snapshot::decode(&s.encode()), Some(s));
    }
    #[test]
    fn corrupt_recovery_data_cannot_change_windows() {
        for s in [
            "",
            "3;0,0,0,0;",
            "999;0,0,1920,1080;",
            "0;0,0,1920,1080;1,9",
            "1;0,0,x,4;",
        ] {
            assert!(Snapshot::decode(s).is_none(), "{s}");
        }
    }
}
