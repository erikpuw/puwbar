use std::{
    mem::size_of,
    path::{Path, PathBuf},
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::{
        Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER,
            KEY_SET_VALUE, REG_SZ,
        },
        Threading::{
            GetExitCodeProcess, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
        },
    },
    UI::{
        Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
        WindowsAndMessaging::SW_HIDE,
    },
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "PuwBar";
const TASK_NAME: &str = "PuwBar";
const STOP_EVENT_NAME: &str = "Local\\PuwBar.StopEvent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartOutcome {
    Disabled,
    TaskSchedulerElevated,
    RegistryRunFallback,
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Signals any running `puwbar.exe` to restore the native taskbar and exit cleanly
/// before overwriting or removing the binary.
pub fn stop_running_puwbar(target_exe: &Path) {
    let event_name = wide(STOP_EVENT_NAME);
    unsafe {
        let ev = OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr());
        if !ev.is_null() && ev != INVALID_HANDLE_VALUE {
            SetEvent(ev);
            CloseHandle(ev);
        }
    }

    // Wait up to 2.5 seconds for the running executable to unlock if it already exists.
    let deadline = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < deadline {
        if !target_exe.exists() {
            break;
        }
        if std::fs::OpenOptions::new()
            .write(true)
            .open(target_exe)
            .is_ok()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

fn task_file() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root)
        .join("System32")
        .join("Tasks")
        .join(TASK_NAME)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn build_task_xml(exe: &Path, user: &str) -> String {
    let user = xml_escape(user);
    let command = xml_escape(&exe.to_string_lossy());
    let dir = xml_escape(
        &exe.parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
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

fn schtasks_as_admin(args: &str) -> bool {
    let verb = wide("runas");
    let file = wide("schtasks.exe");
    let params = wide(args);
    unsafe {
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
}

fn set_run_entry(exe: Option<&Path>) {
    unsafe {
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
        if let Some(path) = exe {
            let cmd = wide(&format!("\"{}\"", path.display()));
            RegSetValueExW(
                hkey,
                wide(RUN_VALUE).as_ptr(),
                0,
                REG_SZ,
                cmd.as_ptr().cast(),
                (cmd.len() * size_of::<u16>()) as u32,
            );
        } else {
            RegDeleteValueW(hkey, wide(RUN_VALUE).as_ptr());
        }
        RegCloseKey(hkey);
    }
}

/// Configures PuwBar autostart on Windows logon.
/// When enabled, requests Administrator elevation (UAC) via `schtasks.exe` to register
/// a zero-delay `HighestAvailable` logon task. Falls back to `HKCU\...\Run` if UAC is declined.
pub fn configure_autostart(enable: bool, target_exe: &Path) -> AutostartOutcome {
    if !enable {
        set_run_entry(None);
        if task_file().exists() {
            let _ = schtasks_as_admin(&format!("/delete /tn {TASK_NAME} /f"));
        }
        return AutostartOutcome::Disabled;
    }

    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let xml_file = std::env::temp_dir().join("puwbar_setup_task.xml");
    let utf16: Vec<u8> = [0xFEFFu16]
        .into_iter()
        .chain(build_task_xml(target_exe, &user).encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();

    let created = std::fs::write(&xml_file, utf16).is_ok()
        && schtasks_as_admin(&format!(
            "/create /tn {TASK_NAME} /xml \"{}\" /f",
            xml_file.display()
        ));
    let _ = std::fs::remove_file(&xml_file);

    if created {
        set_run_entry(None);
        AutostartOutcome::TaskSchedulerElevated
    } else {
        set_run_entry(Some(target_exe));
        AutostartOutcome::RegistryRunFallback
    }
}

pub fn remove_autostart() {
    set_run_entry(None);
    if task_file().exists() {
        let _ = schtasks_as_admin(&format!("/delete /tn {TASK_NAME} /f"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_contains_highest_available_and_zero_delay() {
        let xml = build_task_xml(
            Path::new(r"C:\Users\demo\AppData\Local\Programs\PuwBar\puwbar.exe"),
            r"PC\demo",
        );
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(!xml.contains("<Delay>"));
        assert!(xml.contains("puwbar.exe"));
    }
}
