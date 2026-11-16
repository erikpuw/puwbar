use crate::{
    desktop::{self, wide, Snapshot},
    shell::{self, Group},
};
use puwbar::model::{layout, Layout, BOTTOM_GAP_DIP};
use std::{
    cell::RefCell,
    mem::size_of,
    ptr::{null, null_mut},
    sync::atomic::{AtomicIsize, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::*, Power::*, SystemInformation::*, Threading::*},
    UI::{
        Accessibility::*, Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};

const APPBAR_MESSAGE: u32 = WM_APP + 1;
const START: usize = 10;
const OVERFLOW: usize = 11;
const APP: usize = 100;
const NETWORK: usize = 20;
const SOUND: usize = 21;
const BATTERY: usize = 22;
const CLOCK: usize = 23;
const NOTIFICATIONS: usize = 24;
const TRAY_ICON: usize = 30;
const EXIT_HOTKEY: i32 = 1;
const FOCUS_HOTKEY: i32 = 2;
static MAIN: AtomicIsize = AtomicIsize::new(0);
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

struct Panel {
    dock: HWND,
    tray: HWND,
    monitor: RECT,
    dpi: u32,
    layout: Layout,
    signature: Vec<String>,
    tooltip: HWND,
    tray_tooltip: HWND,
}

struct State {
    groups: Vec<Group>,
    tray_items: Vec<crate::tray::Item>,
    panels: Vec<Panel>,
    snapshot: Snapshot,
    preview: bool,
    menu_open: bool,
    tray_text: String,
    taskbar_created: u32,
}

pub fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--guard") {
        return desktop::guard(
            args.get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("Thiếu PID")?,
        );
    }
    let preview = args.iter().any(|a| a == "--preview");
    let seconds = args
        .windows(2)
        .find(|a| a[0] == "--test-seconds")
        .map(|a| a[1].parse::<u32>())
        .transpose()
        .map_err(|_| "Số giây không hợp lệ")?;
    if args.iter().any(|a| a == "--diagnose") {
        return diagnose();
    }
    if args
        .iter()
        .any(|a| !matches!(a.as_str(), "--preview" | "--test-seconds") && a.parse::<u32>().is_err())
    {
        return Err(
            "Tham số không hợp lệ. Dùng --preview, --diagnose hoặc --test-seconds N.".into(),
        );
    }
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let mutex = CreateMutexW(null(), 0, wide("Local\\PuwBar.Session").as_ptr());
        if mutex.is_null() {
            return Err("Không tạo được khóa phiên PuwBar".into());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            return Ok(());
        }
        let result = run_session(preview, seconds);
        CloseHandle(mutex);
        CoUninitialize();
        result
    }
}

unsafe fn run_session(preview: bool, seconds: Option<u32>) -> Result<(), String> {
    let snapshot = Snapshot::capture()?;
    let tray_items = crate::tray::snapshot().unwrap_or_default();
    let mut groups = shell::pinned();
    shell::refresh(&mut groups);
    let class = wide("PuwBar.Window");
    let instance = GetModuleHandleW(null());
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        lpszClassName: class.as_ptr(),
        ..std::mem::zeroed()
    };
    if RegisterClassExW(&wc) == 0 {
        return Err("Không đăng ký được cửa sổ PuwBar".into());
    }
    let mut panels = Vec::new();
    for monitor in desktop::monitors() {
        let r = monitor.rcMonitor;
        let dock = create_window(&class, "PuwBar — ứng dụng", instance, preview);
        let tray = create_window(&class, "PuwBar — trạng thái", instance, preview);
        if dock.is_null() || tray.is_null() {
            if !dock.is_null() {
                DestroyWindow(dock);
            }
            if !tray.is_null() {
                DestroyWindow(tray);
            }
            for p in &panels {
                destroy_panel(p, preview);
            }
            return Err("Không tạo được dock. Taskbar gốc chưa bị thay đổi.".into());
        }
        SetWindowPos(
            dock,
            null_mut(),
            r.left,
            r.top,
            1,
            1,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let dpi = GetDpiForWindow(dock).max(96);
        let Some(l) = layout(
            r.right - r.left,
            r.bottom - r.top,
            dpi,
            groups.len(),
            tray_items.iter().filter(|i| i.promoted).count(),
        ) else {
            DestroyWindow(dock);
            DestroyWindow(tray);
            for p in &panels {
                destroy_panel(p, preview);
            }
            return Err(
                "Màn hình quá nhỏ cho dock và cụm trạng thái. Taskbar gốc được giữ nguyên.".into(),
            );
        };
        let panel = Panel {
            dock,
            tray,
            monitor: r,
            dpi,
            layout: l,
            signature: Vec::new(),
            tooltip: create_tooltip(dock),
            tray_tooltip: create_tooltip(tray),
        };
        position(&panel);
        panels.push(panel);
    }
    if panels.is_empty() {
        return Err("Không tìm thấy màn hình".into());
    }
    let main = panels[0].dock;
    MAIN.store(main as isize, Ordering::SeqCst);
    // Nothing is hidden until the independent recovery child acknowledges readiness.
    let _guard = if preview {
        None
    } else {
        Some(desktop::start_guard(&snapshot)?)
    };
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            groups,
            tray_items,
            panels,
            snapshot: snapshot.clone(),
            preview,
            menu_open: false,
            tray_text: String::new(),
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        })
    });
    if !preview {
        snapshot.hide();
        let mut failed = false;
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let s = s.as_mut().unwrap();
            for panel in &s.panels {
                if !register_appbar(panel) {
                    failed = true;
                }
            }
        });
        if failed {
            cleanup();
            return Err("Không dành được vùng đáy màn hình. Đã trả lại taskbar Windows.".into());
        }
    }
    rebuild();
    STATE.with(|s| {
        for p in &s.borrow().as_ref().unwrap().panels {
            ShowWindow(p.dock, SW_SHOWNOACTIVATE);
            ShowWindow(p.tray, SW_SHOWNOACTIVATE);
        }
    });
    let hotkey_ok = RegisterHotKey(
        main,
        EXIT_HOTKEY,
        MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
        VK_F12 as u32,
    ) != 0;
    if !hotkey_ok {
        cleanup();
        return Err("Ctrl+Alt+F12 đang được ứng dụng khác sử dụng. Đã khôi phục taskbar.".into());
    }
    RegisterHotKey(
        main,
        FOCUS_HOTKEY,
        MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
        VK_SPACE as u32,
    );
    SetTimer(main, 1, 1000, None);
    if let Some(seconds) = seconds {
        SetTimer(main, 9, seconds.clamp(1, 3600) * 1000, None);
    }
    let hook = SetWinEventHook(
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_NAMECHANGE,
        null_mut(),
        Some(win_event),
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
    );
    let foreground_hook = SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        null_mut(),
        Some(win_event),
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
    );
    let mut msg: MSG = std::mem::zeroed();
    loop {
        let result = GetMessageW(&mut msg, null_mut(), 0, 0);
        if result <= 0 {
            break;
        }
        let handles = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|s| {
                    s.panels
                        .iter()
                        .flat_map(|p| [p.dock, p.tray])
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        let handled = handles.iter().any(|&h| IsDialogMessageW(h, &msg) != 0);
        if !handled {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if !hook.is_null() {
        UnhookWinEvent(hook);
    }
    if !foreground_hook.is_null() {
        UnhookWinEvent(foreground_hook);
    }
    UnregisterHotKey(main, EXIT_HOTKEY);
    UnregisterHotKey(main, FOCUS_HOTKEY);
    cleanup();
    Ok(())
}

unsafe fn create_window(class: &[u16], title: &str, instance: HINSTANCE, preview: bool) -> HWND {
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED
            | if preview {
                WS_EX_APPWINDOW
            } else {
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
            },
        class.as_ptr(),
        wide(title).as_ptr(),
        WS_POPUP | WS_CLIPCHILDREN,
        0,
        0,
        1,
        1,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if !hwnd.is_null() {
        SetLayeredWindowAttributes(hwnd, 0, 232, LWA_ALPHA);
    }
    hwnd
}

unsafe fn create_tooltip(owner: HWND) -> HWND {
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST,
        wide("tooltips_class32").as_ptr(),
        null(),
        WS_POPUP | TTS_ALWAYSTIP,
        0,
        0,
        0,
        0,
        owner,
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, 0, 400);
    hwnd
}

unsafe fn button(
    parent: HWND,
    tooltip: HWND,
    id: usize,
    label: &str,
    x: i32,
    width: i32,
    height: i32,
) {
    let hwnd = CreateWindowExW(
        0,
        wide("BUTTON").as_ptr(),
        wide(label).as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
        x,
        2,
        width,
        height - 4,
        parent,
        id as HMENU,
        GetModuleHandleW(null()),
        null(),
    );
    let ti = TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: parent,
        uId: hwnd as usize,
        lpszText: -1isize as *mut u16,
        ..std::mem::zeroed()
    };
    SendMessageW(tooltip, TTM_ADDTOOLW, 0, &ti as *const _ as LPARAM);
}

unsafe fn clear_buttons(parent: HWND, tooltip: HWND) {
    let mut child = GetWindow(parent, GW_CHILD);
    while !child.is_null() {
        let next = GetWindow(child, GW_HWNDNEXT);
        let ti = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            hwnd: parent,
            uId: child as usize,
            ..std::mem::zeroed()
        };
        SendMessageW(tooltip, TTM_DELTOOLW, 0, &ti as *const _ as LPARAM);
        DestroyWindow(child);
        child = next;
    }
}

unsafe fn position(p: &Panel) {
    let l = &p.layout;
    for (h, x, width) in [
        (p.dock, l.dock_x, l.dock_width),
        (p.tray, l.tray_x, l.tray_width),
    ] {
        SetWindowPos(
            h,
            HWND_TOPMOST,
            p.monitor.left + x,
            p.monitor.top + l.y,
            width,
            l.height,
            SWP_NOACTIVATE,
        );
        let radius = 16 * p.dpi as i32 / 96;
        let region = CreateRoundRectRgn(0, 0, width + 1, l.height + 1, radius * 2, radius * 2);
        if SetWindowRgn(h, region, 1) == 0 {
            DeleteObject(region);
        }
    }
}

unsafe fn register_appbar(p: &Panel) -> bool {
    let mut data: APPBARDATA = std::mem::zeroed();
    data.cbSize = size_of::<APPBARDATA>() as u32;
    data.hWnd = p.dock;
    data.uCallbackMessage = APPBAR_MESSAGE;
    if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
        return false;
    }
    reserve(p);
    true
}

unsafe fn reserve(p: &Panel) {
    let mut data: APPBARDATA = std::mem::zeroed();
    data.cbSize = size_of::<APPBARDATA>() as u32;
    data.hWnd = p.dock;
    data.uEdge = ABE_BOTTOM;
    data.rc = p.monitor;
    SHAppBarMessage(ABM_QUERYPOS, &mut data);
    data.rc.top = data.rc.bottom - p.layout.height - BOTTOM_GAP_DIP * p.dpi as i32 / 96;
    SHAppBarMessage(ABM_SETPOS, &mut data);
    position(p);
}

unsafe fn destroy_panel(p: &Panel, preview: bool) {
    if !preview {
        let mut data: APPBARDATA = std::mem::zeroed();
        data.cbSize = size_of::<APPBARDATA>() as u32;
        data.hWnd = p.dock;
        SHAppBarMessage(ABM_REMOVE, &mut data);
    }
    DestroyWindow(p.tooltip);
    DestroyWindow(p.tray_tooltip);
    DestroyWindow(p.dock);
    DestroyWindow(p.tray);
}

fn cleanup() {
    MAIN.store(0, Ordering::SeqCst);
    let state = STATE.with(|s| s.borrow_mut().take());
    if let Some(state) = state {
        unsafe {
            for p in &state.panels {
                destroy_panel(p, state.preview);
            }
        }
        if !state.preview {
            state.snapshot.restore();
        }
    }
}

fn rebuild() {
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        if state.menu_open {
            return;
        }
        shell::refresh(&mut state.groups);
        for p in &mut state.panels {
            let Some(l) = layout(
                p.monitor.right - p.monitor.left,
                p.monitor.bottom - p.monitor.top,
                p.dpi,
                state.groups.len(),
                state.tray_items.iter().filter(|i| i.promoted).count(),
            ) else {
                continue;
            };
            let signature: Vec<String> = state
                .groups
                .iter()
                .take(l.capacity)
                .map(|g| format!("{}:{}", g.key, g.windows.len()))
                .chain(Some(format!("overflow:{}", l.overflow)))
                .chain(
                    state
                        .tray_items
                        .iter()
                        .take(l.tray_slots)
                        .map(|item| format!("{}:{}", item.name, item.pid)),
                )
                .collect();
            unsafe {
                if signature != p.signature {
                    p.layout = l;
                    position(p);
                    clear_buttons(p.dock, p.tooltip);
                    let padding = 10 * p.dpi as i32 / 96;
                    button(p.dock, p.tooltip, START, "Start", padding, l.slot, l.height);
                    for (index, g) in state.groups.iter().take(l.capacity).enumerate() {
                        button(
                            p.dock,
                            p.tooltip,
                            APP + index,
                            &format!("{} — {} cửa sổ", g.name, g.windows.len()),
                            padding + (index as i32 + 1) * l.slot,
                            l.slot,
                            l.height,
                        );
                    }
                    if l.overflow {
                        button(
                            p.dock,
                            p.tooltip,
                            OVERFLOW,
                            "Ứng dụng khác",
                            padding + (l.capacity as i32 + 1) * l.slot,
                            l.slot,
                            l.height,
                        );
                    }
                    p.signature = signature;
                    clear_buttons(p.tray, p.tray_tooltip);
                }
                if GetWindow(p.tray, GW_CHILD).is_null() {
                    let px = |n| n * p.dpi as i32 / 96;
                    let extra = l.tray_slots as i32 * px(28);
                    for (i, item) in state.tray_items.iter().take(l.tray_slots).enumerate() {
                        button(
                            p.tray,
                            p.tray_tooltip,
                            TRAY_ICON + i,
                            &item.name,
                            px(37) + i as i32 * px(28),
                            px(28),
                            l.height,
                        );
                    }
                    button(
                        p.tray,
                        p.tray_tooltip,
                        NOTIFICATIONS,
                        "Ứng dụng trong system tray",
                        px(5),
                        px(32),
                        l.height,
                    );
                    button(
                        p.tray,
                        p.tray_tooltip,
                        NETWORK,
                        "Mở cài đặt mạng",
                        px(37) + extra,
                        px(34),
                        l.height,
                    );
                    button(
                        p.tray,
                        p.tray_tooltip,
                        SOUND,
                        "Mở cài đặt âm thanh",
                        px(71) + extra,
                        px(34),
                        l.height,
                    );
                    button(
                        p.tray,
                        p.tray_tooltip,
                        BATTERY,
                        "Pin và nguồn điện",
                        px(105) + extra,
                        px(71),
                        l.height,
                    );
                    button(
                        p.tray,
                        p.tray_tooltip,
                        CLOCK,
                        "Ngày và giờ",
                        px(176) + extra,
                        px(78),
                        l.height,
                    );
                }
                InvalidateRect(p.dock, null(), 0);
                InvalidateRect(p.tray, null(), 0);
                RedrawWindow(p.dock, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
                RedrawWindow(p.tray, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
            }
        }
    });
}

unsafe extern "system" fn win_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if object != 0 && event != EVENT_SYSTEM_FOREGROUND {
        return;
    }
    let main = MAIN.load(Ordering::SeqCst) as HWND;
    if main.is_null() {
        return;
    }
    if event == EVENT_OBJECT_SHOW
        && !hwnd.is_null()
        && matches!(
            desktop::class_name(hwnd).as_str(),
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        )
    {
        let hide = STATE.with(|s| {
            s.try_borrow()
                .ok()
                .and_then(|s| s.as_ref().map(|s| !s.preview))
                .unwrap_or(false)
        });
        if hide {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
    if matches!(
        event,
        EVENT_OBJECT_SHOW
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_NAMECHANGE
            | EVENT_SYSTEM_FOREGROUND
    ) {
        SetTimer(main, 2, 180, None);
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Keep panics from unwinding across the Windows callback boundary; the guard restores on abort.
    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            paint_background(hwnd);
            return 0;
        }
        WM_DRAWITEM => {
            draw_button(&*(l as *const DRAWITEMSTRUCT));
            return 1;
        }
        WM_NOTIFY => {
            let info = l as *mut NMTTDISPINFOW;
            if !info.is_null() && (*info).hdr.code == TTN_GETDISPINFOW {
                GetWindowTextW((*info).hdr.idFrom as HWND, (*info).szText.as_mut_ptr(), 80);
                (*info).lpszText = (*info).szText.as_mut_ptr();
                return 0;
            }
        }
        WM_COMMAND => {
            click(hwnd, w & 0xffff);
            return 0;
        }
        WM_CONTEXTMENU => {
            context_menu(hwnd, w as HWND, l);
            return 0;
        }
        WM_TIMER if w == 9 => {
            PostQuitMessage(0);
            return 0;
        }
        WM_TIMER if w == 2 => {
            KillTimer(hwnd, 2);
            rebuild();
            return 0;
        }
        WM_TIMER if w == 1 => {
            tick();
            return 0;
        }
        WM_HOTKEY if w == EXIT_HOTKEY as usize => {
            PostQuitMessage(0);
            return 0;
        }
        WM_HOTKEY if w == FOCUS_HOTKEY as usize => {
            SetForegroundWindow(hwnd);
            SetFocus(GetDlgItem(hwnd, START as i32));
            return 0;
        }
        WM_CLOSE | WM_ENDSESSION => {
            PostQuitMessage(0);
            return 0;
        }
        WM_QUERYENDSESSION => return 1,
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            PostQuitMessage(0);
            return 0;
        }
        APPBAR_MESSAGE => {
            if w == ABN_POSCHANGED as usize {
                SetTimer(MAIN.load(Ordering::SeqCst) as HWND, 3, 200, None);
            }
            return 0;
        }
        WM_TIMER if w == 3 => {
            KillTimer(hwnd, 3);
            STATE.with(|s| {
                if let Some(s) = s.borrow().as_ref() {
                    if !s.preview {
                        for p in &s.panels {
                            let monitor = MonitorFromWindow(p.dock, MONITOR_DEFAULTTONEAREST);
                            let mut info: MONITORINFO = std::mem::zeroed();
                            info.cbSize = size_of::<MONITORINFO>() as u32;
                            if GetMonitorInfoW(monitor, &mut info) != 0
                                && info.rcWork.bottom > p.monitor.bottom - p.layout.height
                            {
                                reserve(p);
                            }
                        }
                    }
                }
            });
            return 0;
        }
        _ => {}
    }
    let recreated = STATE.with(|s| {
        s.try_borrow()
            .ok()
            .is_some_and(|s| s.as_ref().is_some_and(|s| s.taskbar_created == msg))
    });
    if recreated {
        STATE.with(|s| {
            let s = s.borrow();
            if let Some(s) = s.as_ref() {
                if !s.preview {
                    s.snapshot.hide();
                    for p in &s.panels {
                        register_appbar(p);
                    }
                }
            }
        });
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}

fn tick() {
    let mut rebuild_due = false;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else {
            return;
        };
        if !s.preview {
            desktop::hide_taskbars();
        }
        let text = status_text();
        let changed = s.tray_text != text;
        s.tray_text = text;
        unsafe {
            for p in &s.panels {
                let show = s.preview || !shell::fullscreen(&p.monitor);
                for h in [p.dock, p.tray] {
                    if (IsWindowVisible(h) != 0) != show {
                        ShowWindow(h, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
                    }
                }
                if changed {
                    RedrawWindow(p.tray, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
                }
            }
        }
        // Polling is only a recovery path for missed shell events, not a render loop.
        thread_local! { static TICKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
        TICKS.with(|n| {
            n.set(n.get() + 1);
            rebuild_due = n.get() % 5 == 0;
        });
    });
    if rebuild_due {
        let refresh = STATE.with(|s| s.borrow().as_ref().is_some_and(|s| !s.menu_open));
        if refresh {
            if let Ok(items) = crate::tray::snapshot() {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.tray_items = items;
                    }
                });
            }
        }
        rebuild();
    }
}

fn status_text() -> String {
    unsafe {
        let mut time: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut time);
        let mut power: SYSTEM_POWER_STATUS = std::mem::zeroed();
        let battery = if GetSystemPowerStatus(&mut power) != 0
            && power.BatteryFlag != 128
            && power.BatteryLifePercent <= 100
        {
            format!(
                "{}%{}",
                power.BatteryLifePercent,
                if power.ACLineStatus == 1 { " ⚡" } else { "" }
            )
        } else {
            "AC".into()
        };
        format!("{battery}|{:02}:{:02}", time.wHour, time.wMinute)
    }
}

unsafe fn click(owner: HWND, id: usize) {
    match id {
        START => {
            let inputs = [
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_LWIN,
                            ..std::mem::zeroed()
                        },
                    },
                },
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_LWIN,
                            dwFlags: KEYEVENTF_KEYUP,
                            ..std::mem::zeroed()
                        },
                    },
                },
            ];
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
        NETWORK => shell::launch(owner, "ms-settings:network-status"),
        SOUND => shell::launch(owner, "ms-settings:sound"),
        BATTERY => shell::launch(owner, "ms-settings:batterysaver"),
        CLOCK => shell::launch(owner, "ms-settings:dateandtime"),
        OVERFLOW => overflow_menu(owner),
        NOTIFICATIONS => notification_menu(owner),
        id if (TRAY_ICON..TRAY_ICON + 3).contains(&id) => {
            let pid =
                STATE.with(|s| Some(s.borrow().as_ref()?.tray_items.get(id - TRAY_ICON)?.pid));
            if !pid.is_some_and(|p| activate_tray_window(p)) {
                notification_menu(owner);
            }
        }
        id if id >= APP => {
            let action = STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|s| s.groups.get(id - APP))
                    .map(|g| (g.launch.clone(), g.windows.clone()))
            });
            if let Some((target, windows)) = action {
                match windows.len() {
                    0 => shell::launch(owner, &target),
                    1 => shell::activate(windows[0].0),
                    _ => window_menu(owner, &windows),
                }
            }
        }
        _ => {}
    }
}

unsafe fn notification_menu(owner: HWND) {
    let items = crate::tray::snapshot();
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let entries = match items {
        Ok(items) if !items.is_empty() => items,
        result => {
            let message = if let Err(code) = result {
                format!("Không đọc được system tray (mã {code})")
            } else {
                "Chưa tìm thấy biểu tượng đang hoạt động".into()
            };
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(&message).as_ptr());
            menu_at(owner, menu);
            return;
        }
    };
    let mut images = Vec::new();
    let dc = GetDC(owner);
    let edge = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(owner)).max(16);
    for (index, item) in entries.iter().enumerate() {
        let id = index + 1;
        AppendMenuW(
            menu,
            MF_STRING,
            id,
            wide(&item.name.replace('&', "&&")).as_ptr(),
        );
        if !item.icon.0.is_null() {
            if let Some(image) = Surface::new(dc, edge, edge) {
                DrawIconEx(
                    image.dc,
                    0,
                    0,
                    item.icon.0,
                    edge,
                    edge,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
                let info = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as _,
                    fMask: MIIM_BITMAP,
                    hbmpItem: image.bitmap,
                    ..std::mem::zeroed()
                };
                SetMenuItemInfoW(menu, id as _, 0, &info);
                images.push(image);
            }
        }
    }
    ReleaseDC(owner, dc);
    let choice = menu_at(owner, menu) as usize;
    if let Some(item) = choice.checked_sub(1).and_then(|i| entries.get(i)) {
        activate_tray_window(item.pid);
    }
}

unsafe fn activate_tray_window(owner_pid: u32) -> bool {
    let window = STATE.with(|s| {
        s.borrow()
            .as_ref()?
            .groups
            .iter()
            .flat_map(|g| &g.windows)
            .find_map(|(h, _)| {
                let mut pid = 0;
                GetWindowThreadProcessId(*h, &mut pid);
                (pid == owner_pid).then_some(*h)
            })
    });
    if let Some(hwnd) = window {
        shell::activate(hwnd);
        return true;
    }
    false
}

unsafe fn menu_at(owner: HWND, menu: HMENU) -> u32 {
    let mut point = POINT { x: 0, y: 0 };
    GetCursorPos(&mut point);
    if GetKeyState(VK_RETURN as i32) < 0 || GetKeyState(VK_SPACE as i32) < 0 {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(GetFocus(), &mut rect);
        point = POINT {
            x: rect.left,
            y: rect.top,
        };
    }
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.menu_open = true;
        }
    });
    SetForegroundWindow(owner);
    let result = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        owner,
        null(),
    );
    PostMessageW(owner, WM_NULL, 0, 0);
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.menu_open = false;
        }
    });
    DestroyMenu(menu);
    result as u32
}

unsafe fn window_menu(owner: HWND, windows: &[(HWND, String)]) {
    let menu = CreatePopupMenu();
    for (index, (_, title)) in windows.iter().enumerate() {
        AppendMenuW(
            menu,
            MF_STRING,
            index + 1,
            wide(&title.replace('&', "&&")).as_ptr(),
        );
    }
    let selected = menu_at(owner, menu);
    if selected > 0 {
        if let Some((hwnd, _)) = windows.get(selected as usize - 1) {
            shell::activate(*hwnd);
        }
    }
}

unsafe fn overflow_menu(owner: HWND) {
    let items = STATE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref()?;
        let capacity = s.panels.iter().find(|p| p.dock == owner)?.layout.capacity;
        Some(
            s.groups
                .iter()
                .enumerate()
                .skip(capacity)
                .map(|(i, g)| (i, g.name.clone()))
                .collect::<Vec<_>>(),
        )
    });
    let Some(items) = items else {
        return;
    };
    let menu = CreatePopupMenu();
    for (index, name) in items {
        AppendMenuW(
            menu,
            MF_STRING,
            APP + index,
            wide(&name.replace('&', "&&")).as_ptr(),
        );
    }
    let selected = menu_at(owner, menu);
    if selected > 0 {
        click(owner, selected as usize);
    }
}

unsafe fn context_menu(owner: HWND, source: HWND, _: LPARAM) {
    let id = GetDlgCtrlID(source) as usize;
    let menu = CreatePopupMenu();
    if id >= APP {
        AppendMenuW(menu, MF_STRING, 1, wide("Mở cửa sổ mới").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
    }
    AppendMenuW(
        menu,
        MF_STRING,
        2,
        wide("Thoát PuwBar — khôi phục taskbar Windows\tCtrl+Alt+F12").as_ptr(),
    );
    match menu_at(owner, menu) {
        1 => {
            let target = STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .and_then(|s| s.groups.get(id - APP))
                    .map(|g| g.launch.clone())
            });
            if let Some(t) = target {
                shell::launch(owner, &t);
            }
        }
        2 => {
            PostQuitMessage(0);
        }
        _ => {}
    }
}

unsafe fn paint_background(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rect: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rect);
    let canvas = Surface::new(dc, rect.right, rect.bottom);
    let target = canvas.as_ref().map(|s| s.dc).unwrap_or(dc);
    glass_fill(target, rect, 0, rect.bottom);
    let pen = CreatePen(PS_SOLID, 1, rgb(65, 77, 93));
    let old = SelectObject(target, pen);
    let old_brush = SelectObject(target, GetStockObject(HOLLOW_BRUSH));
    let radius = 32 * GetDpiForWindow(hwnd).max(96) as i32 / 96;
    RoundRect(target, 0, 0, rect.right, rect.bottom, radius, radius);
    SelectObject(target, old_brush);
    SelectObject(target, old);
    DeleteObject(pen);
    if let Some(canvas) = canvas {
        canvas.present(dc);
    }
    EndPaint(hwnd, &ps);
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    width: i32,
    height: i32,
}

impl Surface {
    unsafe fn new(target: HDC, width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let dc = CreateCompatibleDC(target);
        if dc.is_null() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut pixels = null_mut();
        let bitmap = CreateDIBSection(target, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
        if bitmap.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bitmap);
        std::ptr::write_bytes(pixels, 0, (width * height * 4) as usize);
        Some(Self {
            dc,
            bitmap,
            old,
            width,
            height,
        })
    }

    unsafe fn present(&self, target: HDC) {
        BitBlt(
            target,
            0,
            0,
            self.width,
            self.height,
            self.dc,
            0,
            0,
            SRCCOPY,
        );
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

unsafe fn glass_fill(dc: HDC, rect: RECT, offset: i32, height: i32) {
    let vertices = [
        TRIVERTEX {
            x: rect.left,
            y: -offset,
            Red: 0x1900,
            Green: 0x2b00,
            Blue: 0x4500,
            Alpha: 0xffff,
        },
        TRIVERTEX {
            x: rect.right,
            y: height - offset,
            Red: 0x0a00,
            Green: 0x1300,
            Blue: 0x2200,
            Alpha: 0xffff,
        },
    ];
    let mesh = GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    };
    GdiGradientFill(
        dc,
        vertices.as_ptr(),
        2,
        (&mesh as *const GRADIENT_RECT).cast(),
        1,
        GRADIENT_FILL_RECT_V,
    );
}

unsafe fn draw_app_icon(dc: HDC, icon: HICON, x: i32, y: i32, edge: i32) {
    // Composite on the destination colour before filtering, preserving transparent icon edges.
    let source = edge.max(48);
    if let Some(surface) = Surface::new(dc, source, source) {
        StretchBlt(
            surface.dc, 0, 0, source, source, dc, x, y, edge, edge, SRCCOPY,
        );
        DrawIconEx(
            surface.dc,
            0,
            0,
            icon,
            source,
            source,
            0,
            null_mut(),
            DI_NORMAL,
        );
        let old = SetStretchBltMode(dc, HALFTONE);
        SetBrushOrgEx(dc, 0, 0, null_mut());
        StretchBlt(
            dc, x, y, edge, edge, surface.dc, 0, 0, source, source, SRCCOPY,
        );
        SetStretchBltMode(dc, old);
    } else {
        DrawIconEx(dc, x, y, icon, edge, edge, 0, null_mut(), DI_NORMAL);
    }
}

unsafe fn draw_button(draw: &DRAWITEMSTRUCT) {
    let canvas = Surface::new(draw.hDC, draw.rcItem.right, draw.rcItem.bottom);
    let dc = canvas.as_ref().map(|s| s.dc).unwrap_or(draw.hDC);
    let r = draw.rcItem;
    let dpi = GetDpiForWindow(draw.hwndItem).max(96) as i32;
    let px = |n| n * dpi / 96;
    let pressed = draw.itemState & ODS_SELECTED != 0;
    let focused = draw.itemState & ODS_FOCUS != 0;
    glass_fill(dc, r, 2, r.bottom + 4);
    if pressed {
        let bg = CreateSolidBrush(rgb(55, 68, 86));
        FillRect(dc, &r, bg);
        DeleteObject(bg);
    }
    let center = (r.right + r.left) / 2;
    let y = (r.bottom - px(32)) / 2;
    let id = draw.CtlID as usize;
    match id {
        START => {
            let brush = CreateSolidBrush(rgb(45, 185, 240));
            let size = px(14);
            let gap = px(2);
            for x in [center - size - gap / 2, center + gap / 2] {
                for top in [y, y + size + gap] {
                    FillRect(
                        dc,
                        &RECT {
                            left: x,
                            top,
                            right: x + size,
                            bottom: top + size,
                        },
                        brush,
                    );
                }
            }
            DeleteObject(brush);
            let brush = CreateSolidBrush(rgb(65, 76, 91));
            FillRect(
                dc,
                &RECT {
                    left: r.right - 1,
                    top: px(10),
                    right: r.right,
                    bottom: r.bottom - px(10),
                },
                brush,
            );
            DeleteObject(brush);
        }
        OVERFLOW => text(dc, "···", r, px(20), "Segoe UI", rgb(235, 240, 248)),
        id if (TRAY_ICON..TRAY_ICON + 3).contains(&id) => STATE.with(|s| {
            if let Some(item) = s
                .borrow()
                .as_ref()
                .and_then(|s| s.tray_items.get(id - TRAY_ICON))
            {
                DrawIconEx(
                    dc,
                    center - px(8),
                    (r.bottom - px(16)) / 2,
                    item.icon.0,
                    px(16),
                    px(16),
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
            }
        }),
        NOTIFICATIONS => text(
            dc,
            "\u{E70E}",
            r,
            px(15),
            "Segoe Fluent Icons",
            rgb(235, 240, 248),
        ),
        NETWORK => text(
            dc,
            "\u{E701}",
            r,
            px(18),
            "Segoe Fluent Icons",
            rgb(235, 240, 248),
        ),
        SOUND => text(
            dc,
            "\u{E767}",
            r,
            px(18),
            "Segoe Fluent Icons",
            rgb(235, 240, 248),
        ),
        BATTERY | CLOCK => {
            let status = status_text();
            let (battery, time) = status.split_once('|').unwrap_or(("AC", "--:--"));
            let mut label = r;
            if id == BATTERY && battery.contains('%') {
                let percent = battery
                    .split('%')
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let glyph = char::from_u32(
                    (if battery.contains('⚡') {
                        0xE85A
                    } else {
                        0xE850
                    }) + (percent.min(100) / 10).min(9),
                )
                .unwrap();
                text(
                    dc,
                    &glyph.to_string(),
                    RECT { right: px(25), ..r },
                    px(18),
                    "Segoe Fluent Icons",
                    rgb(235, 240, 248),
                );
                label.left += px(23);
            }
            if id == CLOCK {
                let brush = CreateSolidBrush(rgb(67, 82, 104));
                FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: px(10),
                        right: 1,
                        bottom: r.bottom - px(10),
                    },
                    brush,
                );
                DeleteObject(brush);
            }
            text(
                dc,
                if id == BATTERY {
                    battery.trim_end_matches(" ⚡")
                } else {
                    time
                },
                label,
                px(14),
                "Segoe UI",
                rgb(235, 240, 248),
            );
        }
        id if id >= APP => STATE.with(|s| {
            let s = s.borrow();
            if let Some(g) = s.as_ref().and_then(|s| s.groups.get(id - APP)) {
                let icon = if g.icon.0.is_null() {
                    LoadIconW(null_mut(), IDI_APPLICATION)
                } else {
                    g.icon.0
                };
                draw_app_icon(dc, icon, center - px(16), y, px(32));
                if !g.pinned
                    && id > APP
                    && s.as_ref().is_some_and(|s| s.groups[id - APP - 1].pinned)
                {
                    let brush = CreateSolidBrush(rgb(67, 82, 104));
                    FillRect(
                        dc,
                        &RECT {
                            left: 0,
                            top: px(12),
                            right: 1,
                            bottom: r.bottom - px(12),
                        },
                        brush,
                    );
                    DeleteObject(brush);
                }
                let foreground = GetForegroundWindow();
                let active = g.windows.iter().any(|(h, _)| *h == foreground);
                if !g.windows.is_empty() {
                    let width = if active { px(18) } else { px(4) };
                    let brush = CreateSolidBrush(if active {
                        rgb(98, 199, 255)
                    } else {
                        rgb(167, 182, 202)
                    });
                    let old = SelectObject(dc, brush);
                    let oldpen = SelectObject(dc, GetStockObject(NULL_PEN));
                    RoundRect(
                        dc,
                        center - width / 2,
                        r.bottom - px(4),
                        center + width / 2,
                        r.bottom - px(1),
                        px(3),
                        px(3),
                    );
                    SelectObject(dc, oldpen);
                    SelectObject(dc, old);
                    DeleteObject(brush);
                }
            }
        }),
        _ => {}
    }
    if focused {
        let inset = RECT {
            left: r.left + 2,
            top: r.top + 2,
            right: r.right - 2,
            bottom: r.bottom - 2,
        };
        DrawFocusRect(dc, &inset);
    }
    if let Some(canvas) = canvas {
        canvas.present(draw.hDC);
    }
}

unsafe fn text(dc: HDC, value: &str, mut rect: RECT, height: i32, font: &str, color: u32) {
    let face = wide(font);
    let handle = CreateFontW(
        -height,
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        face.as_ptr(),
    );
    let old = SelectObject(dc, handle);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, color);
    let mut value = wide(value);
    DrawTextW(
        dc,
        value.as_mut_ptr(),
        value.len() as i32 - 1,
        &mut rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(dc, old);
    DeleteObject(handle);
}

fn diagnose() -> Result<(), String> {
    let snapshot = Snapshot::capture()?;
    let path = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_extension("diagnostics.txt");
    let text = format!(
        "PuwBar 0.1\nMonitors: {}\nTaskbars: {}\nSnapshot: {}\n",
        desktop::monitors().len(),
        snapshot.bars.len(),
        snapshot.encode()
    );
    std::fs::write(path, text).map_err(|e| e.to_string())
}
