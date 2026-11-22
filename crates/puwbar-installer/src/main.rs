#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod install;
mod shortcut;

use autostart::{wide, AutostartOutcome};
use install::{
    default_install_dir, launch_installed_app, perform_install, perform_uninstall, InstallOptions,
    APP_NAME, APP_VERSION,
};
use std::{
    mem::size_of,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE},
        Gdi::*,
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
    },
    UI::{
        Controls::{
            InitCommonControlsEx, DRAWITEMSTRUCT, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
            ODS_SELECTED,
        },
        HiDpi::{
            GetDpiForWindow, SetProcessDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        },
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::*,
    },
};

const ID_EDIT_DIR: isize = 101;
const ID_BTN_LANG: isize = 102;
const ID_BTN_PRIMARY: isize = 103;
const ID_BTN_CANCEL: isize = 104;

const WIN_W: i32 = 540;
const WIN_H: i32 = 455;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ready,
    Done,
    Failed,
}

#[derive(Clone, Copy)]
struct Theme {
    dark: bool,
    bg: u32,
    card: u32,
    border: u32,
    text: u32,
    muted: u32,
    accent: u32,
    accent_hover: u32,
    ok: u32,
    err: u32,
    uac_bg: u32,
    uac_border: u32,
    uac_text: u32,
    btn_bg: u32,
    btn_border: u32,
    edit_bg: u32,
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

fn theme_for(dark: bool) -> Theme {
    if dark {
        Theme {
            dark: true,
            bg: rgb(24, 24, 27),
            card: rgb(35, 35, 40),
            border: rgb(63, 63, 70),
            text: rgb(244, 244, 245),
            muted: rgb(161, 161, 170),
            accent: rgb(59, 130, 246),
            accent_hover: rgb(37, 99, 235),
            ok: rgb(34, 197, 94),
            err: rgb(248, 113, 113),
            uac_bg: rgb(30, 41, 59),
            uac_border: rgb(59, 130, 246),
            uac_text: rgb(191, 219, 254),
            btn_bg: rgb(46, 46, 52),
            btn_border: rgb(74, 74, 82),
            edit_bg: rgb(32, 32, 37),
        }
    } else {
        Theme {
            dark: false,
            bg: rgb(245, 245, 247),
            card: rgb(255, 255, 255),
            border: rgb(212, 212, 216),
            text: rgb(24, 24, 27),
            muted: rgb(100, 100, 110),
            accent: rgb(37, 99, 235),
            accent_hover: rgb(29, 78, 216),
            ok: rgb(22, 163, 74),
            err: rgb(220, 38, 38),
            uac_bg: rgb(239, 246, 255),
            uac_border: rgb(147, 197, 253),
            uac_text: rgb(30, 58, 138),
            btn_bg: rgb(255, 255, 255),
            btn_border: rgb(200, 200, 208),
            edit_bg: rgb(255, 255, 255),
        }
    }
}

struct AppState {
    mode: Mode,
    is_vi: bool,
    theme: Theme,
    dpi: i32,
    install_dir: String,
    create_start_menu: bool,
    create_desktop: bool,
    enable_startup_task: bool,
    launch_after_install: bool,
    status: Status,
    message: String,
    installed_exe: Option<PathBuf>,
    hwnd_edit: HWND,
    hwnd_lang: HWND,
    hwnd_primary: HWND,
    hwnd_cancel: HWND,
    font_title: HFONT,
    font_bold: HFONT,
    font_body: HFONT,
    font_small: HFONT,
    brush_bg: HBRUSH,
    brush_edit: HBRUSH,
    checkbox_rects: [RECT; 4],
}

impl AppState {
    fn s(&self, px: i32) -> i32 {
        (px * self.dpi + 48) / 96
    }

    fn read_dir_from_edit(&mut self) {
        if self.hwnd_edit.is_null() {
            return;
        }
        unsafe {
            let mut buf = [0u16; 1024];
            let len = GetWindowTextW(self.hwnd_edit, buf.as_mut_ptr(), buf.len() as i32) as usize;
            let text = String::from_utf16_lossy(&buf[..len]).trim().to_owned();
            if !text.is_empty() {
                self.install_dir = text;
            }
        }
    }

    fn update_control_labels(&self) {
        unsafe {
            let lang_txt = wide(if self.is_vi { "EN" } else { "VI" });
            SetWindowTextW(self.hwnd_lang, lang_txt.as_ptr());

            let primary_txt = if self.status == Status::Done {
                if self.is_vi { "Hoàn tất" } else { "Finish" }
            } else {
                match (self.mode, self.is_vi, self.enable_startup_task) {
                    (Mode::Install, true, true) => "Cài đặt (UAC)",
                    (Mode::Install, true, false) => "Cài đặt",
                    (Mode::Install, false, true) => "Install (UAC)",
                    (Mode::Install, false, false) => "Install",
                    (Mode::Uninstall, true, _) => "Gỡ cài đặt",
                    (Mode::Uninstall, false, _) => "Uninstall",
                }
            };
            let primary_w = wide(primary_txt);
            SetWindowTextW(self.hwnd_primary, primary_w.as_ptr());

            let cancel_w = wide(if self.is_vi { "Đóng" } else { "Cancel" });
            SetWindowTextW(self.hwnd_cancel, cancel_w.as_ptr());

            ShowWindow(
                self.hwnd_cancel,
                if self.status == Status::Done {
                    SW_HIDE
                } else {
                    SW_SHOW
                },
            );
            if !self.hwnd_edit.is_null() {
                EnableWindow(self.hwnd_edit, i32::from(self.status != Status::Done));
            }
            InvalidateRect(self.hwnd_lang, null(), 1);
            InvalidateRect(self.hwnd_primary, null(), 1);
            InvalidateRect(self.hwnd_cancel, null(), 1);
        }
    }

    fn run_primary(&mut self, hwnd: HWND) {
        if self.status == Status::Done {
            if self.mode == Mode::Install && self.launch_after_install {
                if let Some(ref exe) = self.installed_exe {
                    launch_installed_app(exe);
                }
            }
            unsafe {
                DestroyWindow(hwnd);
            }
            return;
        }

        self.read_dir_from_edit();
        match self.mode {
            Mode::Install => {
                let opts = InstallOptions {
                    install_dir: PathBuf::from(&self.install_dir),
                    create_start_menu: self.create_start_menu,
                    create_desktop: self.create_desktop,
                    enable_startup_task: self.enable_startup_task,
                };
                match perform_install(&opts) {
                    Ok(res) => {
                        self.installed_exe = Some(res.exe_path);
                        self.status = Status::Done;
                        self.message = match (res.autostart, self.is_vi) {
                            (AutostartOutcome::TaskSchedulerElevated, true) => format!(
                                "Đã cài đặt {APP_NAME} v{APP_VERSION}! Đã bật khởi động tức thì (Task Scheduler + UAC)."
                            ),
                            (AutostartOutcome::TaskSchedulerElevated, false) => format!(
                                "{APP_NAME} v{APP_VERSION} installed! Zero-delay Task Scheduler startup enabled."
                            ),
                            (AutostartOutcome::RegistryRunFallback, true) => format!(
                                "Đã cài đặt {APP_NAME} v{APP_VERSION}! (Từ chối UAC — đã dùng khởi động thường Registry Run.)"
                            ),
                            (AutostartOutcome::RegistryRunFallback, false) => format!(
                                "{APP_NAME} v{APP_VERSION} installed! (UAC declined — fell back to Registry Run.)"
                            ),
                            (AutostartOutcome::Disabled, true) => {
                                format!("Đã cài đặt thành công {APP_NAME} v{APP_VERSION}!")
                            }
                            (AutostartOutcome::Disabled, false) => {
                                format!("{APP_NAME} v{APP_VERSION} installed successfully!")
                            }
                        };
                    }
                    Err(err) => {
                        self.status = Status::Failed;
                        self.message = err;
                    }
                }
            }
            Mode::Uninstall => match perform_uninstall(&PathBuf::from(&self.install_dir)) {
                Ok(()) => {
                    self.status = Status::Done;
                    self.message = if self.is_vi {
                        "Đã gỡ cài đặt PuwBar và khôi phục taskbar gốc.".into()
                    } else {
                        "PuwBar has been uninstalled and the native taskbar restored.".into()
                    };
                }
                Err(err) => {
                    self.status = Status::Failed;
                    self.message = err;
                }
            },
        }
        self.update_control_labels();
        unsafe {
            InvalidateRect(hwnd, null(), 1);
        }
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        unsafe {
            for f in [
                self.font_title,
                self.font_bold,
                self.font_body,
                self.font_small,
            ] {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
            if !self.brush_bg.is_null() {
                DeleteObject(self.brush_bg);
            }
            if !self.brush_edit.is_null() {
                DeleteObject(self.brush_edit);
            }
        }
    }
}

fn detect_windows_dark_theme() -> bool {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut light = 0u32;
    let mut bytes = size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut light).cast(),
            &mut bytes,
        ) == 0
    };
    if ok {
        light == 0
    } else {
        true
    }
}

unsafe fn make_font(pt: i32, weight: i32, dpi: i32) -> HFONT {
    let height = -((pt * dpi + 36) / 72);
    let face = wide("Segoe UI");
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        face.as_ptr(),
    )
}

unsafe fn draw_text(hdc: HDC, font: HFONT, color: u32, text: &str, mut rect: RECT, flags: u32) {
    let old = SelectObject(hdc, font);
    SetTextColor(hdc, color);
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut w = wide(text);
    // Strip trailing null from length for DrawTextW.
    let len = w.len().saturating_sub(1) as i32;
    DrawTextW(hdc, w.as_mut_ptr(), len, &mut rect, flags);
    SelectObject(hdc, old);
}

unsafe fn draw_round_box(hdc: HDC, r: RECT, fill: u32, border: u32, radius: i32) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_b = SelectObject(hdc, brush);
    let old_p = SelectObject(hdc, pen);
    RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    DeleteObject(brush);
    DeleteObject(pen);
}

unsafe fn draw_checkbox(
    hdc: HDC,
    st: &AppState,
    rect: RECT,
    checked: bool,
    disabled: bool,
    label: &str,
) {
    let box_sz = st.s(16);
    let cy = (rect.top + rect.bottom) / 2;
    let b = RECT {
        left: rect.left,
        top: cy - box_sz / 2,
        right: rect.left + box_sz,
        bottom: cy - box_sz / 2 + box_sz,
    };

    let (fill, border) = if checked {
        (st.theme.accent, st.theme.accent)
    } else {
        (st.theme.card, st.theme.border)
    };
    draw_round_box(hdc, b, fill, border, st.s(4));

    if checked {
        let pen = CreatePen(PS_SOLID, st.s(2).max(2), rgb(255, 255, 255));
        let old_p = SelectObject(hdc, pen);
        MoveToEx(hdc, b.left + st.s(3), cy, null_mut());
        LineTo(hdc, b.left + st.s(7), cy + st.s(4));
        LineTo(hdc, b.right - st.s(3), cy - st.s(4));
        SelectObject(hdc, old_p);
        DeleteObject(pen);
    }

    let text_rc = RECT {
        left: b.right + st.s(8),
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };
    let color = if disabled {
        st.theme.muted
    } else {
        st.theme.text
    };
    draw_text(
        hdc,
        st.font_body,
        color,
        label,
        text_rc,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
}

unsafe fn paint_window(hwnd: HWND, st: &mut AppState) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    FillRect(hdc, &client, st.brush_bg);

    let pad = st.s(22);
    let right = client.right - pad;

    // Header title + subtitle
    let title = match (st.mode, st.is_vi) {
        (Mode::Install, true) => format!("Cài đặt {APP_NAME} v{APP_VERSION}"),
        (Mode::Install, false) => format!("Install {APP_NAME} v{APP_VERSION}"),
        (Mode::Uninstall, true) => format!("Gỡ cài đặt {APP_NAME} v{APP_VERSION}"),
        (Mode::Uninstall, false) => format!("Uninstall {APP_NAME} v{APP_VERSION}"),
    };
    draw_text(
        hdc,
        st.font_title,
        st.theme.text,
        &title,
        RECT {
            left: pad,
            top: st.s(16),
            right: right - st.s(54),
            bottom: st.s(44),
        },
        DT_LEFT | DT_SINGLELINE,
    );

    let subtitle = if st.is_vi {
        "Thanh taskbar kính mờ nổi cho Windows 11 — cài đặt cố định vào máy"
    } else {
        "Floating frosted glass taskbar for Windows 11 — per-user installation"
    };
    draw_text(
        hdc,
        st.font_small,
        st.theme.muted,
        subtitle,
        RECT {
            left: pad,
            top: st.s(46),
            right: right - st.s(54),
            bottom: st.s(66),
        },
        DT_LEFT | DT_SINGLELINE,
    );

    // Separator
    let sep_y = st.s(74);
    let sep_pen = CreatePen(PS_SOLID, 1, st.theme.border);
    let old_p = SelectObject(hdc, sep_pen);
    MoveToEx(hdc, pad, sep_y, null_mut());
    LineTo(hdc, right, sep_y);
    SelectObject(hdc, old_p);
    DeleteObject(sep_pen);

    let mut y = st.s(86);
    st.checkbox_rects = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 4];

    match st.mode {
        Mode::Install => {
            let dir_label = if st.is_vi {
                "Thư mục cài đặt:"
            } else {
                "Install location:"
            };
            draw_text(
                hdc,
                st.font_bold,
                st.theme.text,
                dir_label,
                RECT {
                    left: pad,
                    top: y,
                    right,
                    bottom: y + st.s(20),
                },
                DT_LEFT | DT_SINGLELINE,
            );
            // EDIT control occupies y + 24 .. y + 52
            y += st.s(62);

            let disabled = st.status == Status::Done;
            let row_h = st.s(24);
            let labels = if st.is_vi {
                [
                    "Tạo shortcut trong Start Menu",
                    "Tạo shortcut ngoài Desktop",
                    "Chạy cùng Windows khi đăng nhập (xin quyền Quản trị viên UAC)",
                    "Mở PuwBar ngay sau khi cài xong",
                ]
            } else {
                [
                    "Create Start Menu shortcut",
                    "Create Desktop shortcut",
                    "Start with Windows at logon (requests Administrator UAC)",
                    "Launch PuwBar after installation",
                ]
            };
            let checked = [
                st.create_start_menu,
                st.create_desktop,
                st.enable_startup_task,
                st.launch_after_install,
            ];

            for i in 0..3 {
                let rc = RECT {
                    left: pad,
                    top: y,
                    right,
                    bottom: y + row_h,
                };
                st.checkbox_rects[i] = rc;
                draw_checkbox(hdc, st, rc, checked[i], disabled, labels[i]);
                y += st.s(26);
            }

            if st.enable_startup_task {
                y += st.s(4);
                let box_h = st.s(62);
                let uac_rc = RECT {
                    left: pad,
                    top: y,
                    right,
                    bottom: y + box_h,
                };
                draw_round_box(
                    hdc,
                    uac_rc,
                    st.theme.uac_bg,
                    st.theme.uac_border,
                    st.s(8),
                );
                let uac_note = if st.is_vi {
                    "Windows sẽ hiện hộp thoại UAC xin quyền Quản trị viên để đăng ký tác vụ khởi động tức thì (Task Scheduler, độ trễ 0 giây). Nếu từ chối UAC, PuwBar sẽ tự chuyển sang khởi động thường (Registry Run)."
                } else {
                    "Windows will show a UAC prompt to register a zero-delay Task Scheduler logon task (HighestAvailable). If declined, PuwBar falls back to standard Registry Run."
                };
                draw_text(
                    hdc,
                    st.font_small,
                    st.theme.uac_text,
                    uac_note,
                    RECT {
                        left: uac_rc.left + st.s(10),
                        top: uac_rc.top + st.s(7),
                        right: uac_rc.right - st.s(10),
                        bottom: uac_rc.bottom - st.s(6),
                    },
                    DT_LEFT | DT_WORDBREAK,
                );
                y += box_h + st.s(6);
            }

            let rc3 = RECT {
                left: pad,
                top: y,
                right,
                bottom: y + row_h,
            };
            st.checkbox_rects[3] = rc3;
            draw_checkbox(hdc, st, rc3, checked[3], disabled, labels[3]);
            y += st.s(30);
        }
        Mode::Uninstall => {
            let desc = if st.is_vi {
                format!(
                    "Gỡ cài đặt sẽ dừng PuwBar (khôi phục taskbar gốc Windows 11), xóa tác vụ khởi động cùng Windows (có thể hỏi UAC), xóa shortcut và thư mục:\n\n{}",
                    st.install_dir
                )
            } else {
                format!(
                    "Uninstalling will stop PuwBar (restoring the native Windows 11 taskbar), remove startup tasks (may prompt UAC), shortcuts, and:\n\n{}",
                    st.install_dir
                )
            };
            draw_text(
                hdc,
                st.font_body,
                st.theme.text,
                &desc,
                RECT {
                    left: pad,
                    top: y + st.s(4),
                    right,
                    bottom: y + st.s(110),
                },
                DT_LEFT | DT_WORDBREAK,
            );
            y += st.s(120);
        }
    }

    // Unsigned & official source notice (same as puw-notch)
    let notice = if st.is_vi {
        "Bản cài đặt chưa có chữ ký số (Unsigned) · Chỉ tải từ github.com/erikpuw/puwbar"
    } else {
        "Unsigned installer build · Download only from github.com/erikpuw/puwbar"
    };
    draw_text(
        hdc,
        st.font_small,
        st.theme.muted,
        notice,
        RECT {
            left: pad,
            top: y,
            right,
            bottom: y + st.s(18),
        },
        DT_LEFT | DT_SINGLELINE,
    );
    y += st.s(22);

    // Status message
    if !st.message.is_empty() {
        let color = match st.status {
            Status::Done => st.theme.ok,
            Status::Failed => st.theme.err,
            Status::Ready => st.theme.accent,
        };
        draw_text(
            hdc,
            st.font_bold,
            color,
            &st.message,
            RECT {
                left: pad,
                top: y,
                right,
                bottom: client.bottom - st.s(54),
            },
            DT_LEFT | DT_WORDBREAK,
        );
    }

    EndPaint(hwnd, &ps);
}

unsafe fn draw_owner_button(dis: &DRAWITEMSTRUCT, st: &AppState) {
    let is_primary = dis.CtlID == ID_BTN_PRIMARY as u32;
    let pressed = (dis.itemState & ODS_SELECTED) != 0;
    let (fill, border, text_color) = if is_primary {
        (
            if pressed {
                st.theme.accent_hover
            } else {
                st.theme.accent
            },
            st.theme.accent,
            rgb(255, 255, 255),
        )
    } else {
        (
            if pressed {
                st.theme.border
            } else {
                st.theme.btn_bg
            },
            st.theme.btn_border,
            st.theme.text,
        )
    };

    FillRect(dis.hDC, &dis.rcItem, st.brush_bg);
    draw_round_box(dis.hDC, dis.rcItem, fill, border, st.s(8));

    let mut buf = [0u16; 64];
    let len = GetWindowTextW(dis.hwndItem, buf.as_mut_ptr(), buf.len() as i32) as usize;
    let label = String::from_utf16_lossy(&buf[..len]);
    let font = if is_primary {
        st.font_bold
    } else {
        st.font_body
    };
    draw_text(
        dis.hDC,
        font,
        text_color,
        &label,
        dis.rcItem,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *ptr;

    match msg {
        WM_CREATE => {
            let dark_val: i32 = i32::from(st.theme.dark);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark_val).cast(),
                size_of::<i32>() as u32,
            );

            let inst = GetModuleHandleW(null());
            let pad = st.s(22);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            let right = client.right - pad;

            // Language toggle button (top-right)
            st.hwnd_lang = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("EN").as_ptr(),
                WS_CHILD | WS_VISIBLE | BS_OWNERDRAW as u32,
                right - st.s(44),
                st.s(18),
                st.s(44),
                st.s(28),
                hwnd,
                ID_BTN_LANG as HMENU,
                inst,
                null(),
            );

            // Install path EDIT box (Install mode only)
            if st.mode == Mode::Install {
                st.hwnd_edit = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    wide("EDIT").as_ptr(),
                    wide(&st.install_dir).as_ptr(),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                    pad,
                    st.s(110),
                    right - pad,
                    st.s(28),
                    hwnd,
                    ID_EDIT_DIR as HMENU,
                    inst,
                    null(),
                );
                SendMessageW(st.hwnd_edit, WM_SETFONT, st.font_body as WPARAM, 1);
            }

            // Bottom buttons (right-aligned)
            let btn_h = st.s(34);
            let btn_y = client.bottom - pad - btn_h;
            let cancel_w = st.s(92);
            let primary_w = st.s(138);

            st.hwnd_cancel = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Đóng").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                right - cancel_w,
                btn_y,
                cancel_w,
                btn_h,
                hwnd,
                ID_BTN_CANCEL as HMENU,
                inst,
                null(),
            );

            st.hwnd_primary = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Cài đặt (UAC)").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                right - cancel_w - st.s(10) - primary_w,
                btn_y,
                primary_w,
                btn_h,
                hwnd,
                ID_BTN_PRIMARY as HMENU,
                inst,
                null(),
            );

            st.update_control_labels();
            0
        }
        WM_PAINT => {
            paint_window(hwnd, st);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DRAWITEM => {
            let dis = &*(lparam as *const DRAWITEMSTRUCT);
            draw_owner_button(dis, st);
            1
        }
        WM_CTLCOLOREDIT => {
            let hdc = wparam as HDC;
            SetTextColor(hdc, st.theme.text);
            SetBkColor(hdc, st.theme.edit_bg);
            st.brush_edit as isize
        }
        WM_LBUTTONUP => {
            if st.mode == Mode::Install && st.status != Status::Done {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                for i in 0..4 {
                    let r = st.checkbox_rects[i];
                    if x >= r.left && x <= r.right && y >= r.top && y <= r.bottom {
                        match i {
                            0 => st.create_start_menu = !st.create_start_menu,
                            1 => st.create_desktop = !st.create_desktop,
                            2 => st.enable_startup_task = !st.enable_startup_task,
                            3 => st.launch_after_install = !st.launch_after_install,
                            _ => {}
                        }
                        st.update_control_labels();
                        InvalidateRect(hwnd, null(), 1);
                        break;
                    }
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as isize;
            match id {
                ID_BTN_LANG => {
                    st.is_vi = !st.is_vi;
                    st.update_control_labels();
                    InvalidateRect(hwnd, null(), 1);
                }
                ID_BTN_PRIMARY => {
                    st.run_primary(hwnd);
                }
                ID_BTN_CANCEL => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let self_is_uninstaller = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()))
        .map(|n| n.contains("uninstall"))
        .unwrap_or(false);

    if args.iter().any(|a| a == "--silent-install" || a == "/S") {
        let opts = InstallOptions {
            install_dir: default_install_dir(),
            create_start_menu: true,
            create_desktop: false,
            enable_startup_task: true,
        };
        let _ = perform_install(&opts);
        return;
    }
    if args.iter().any(|a| a == "--silent-uninstall") {
        let dir = default_install_dir();
        let _ = perform_uninstall(&dir);
        return;
    }

    let mode = if self_is_uninstaller || args.iter().any(|a| a == "--uninstall") {
        Mode::Uninstall
    } else {
        Mode::Install
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&icc);

        let inst = GetModuleHandleW(null());
        let class_name = wide("PuwBarSetupWindow");
        let icon = LoadImageW(
            inst,
            std::ptr::without_provenance::<u16>(1),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        ) as HICON;

        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: inst,
            hIcon: icon,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: icon,
        };
        RegisterClassExW(&wc);

        let theme = theme_for(detect_windows_dark_theme());
        let desktop_dpi = GetDpiForWindow(GetDesktopWindow()) as i32;
        let dpi = if desktop_dpi > 0 { desktop_dpi } else { 96 };

        let scale = |px: i32| (px * dpi + 48) / 96;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: scale(WIN_W),
            bottom: scale(WIN_H),
        };
        AdjustWindowRectEx(&mut rc, style, 0, 0);
        let full_w = rc.right - rc.left;
        let full_h = rc.bottom - rc.top;

        // Center window on primary monitor work area.
        let mut work = RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        };
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            (&raw mut work).cast(),
            0,
        );
        let x = work.left + ((work.right - work.left) - full_w) / 2;
        let y = work.top + ((work.bottom - work.top) - full_h) / 2;

        let mut state = Box::new(AppState {
            mode,
            is_vi: true,
            theme,
            dpi,
            install_dir: default_install_dir().display().to_string(),
            create_start_menu: true,
            create_desktop: false,
            enable_startup_task: true,
            launch_after_install: true,
            status: Status::Ready,
            message: String::new(),
            installed_exe: None,
            hwnd_edit: null_mut(),
            hwnd_lang: null_mut(),
            hwnd_primary: null_mut(),
            hwnd_cancel: null_mut(),
            font_title: make_font(15, FW_SEMIBOLD as i32, dpi),
            font_bold: make_font(10, FW_SEMIBOLD as i32, dpi),
            font_body: make_font(10, FW_NORMAL as i32, dpi),
            font_small: make_font(9, FW_NORMAL as i32, dpi),
            brush_bg: CreateSolidBrush(theme.bg),
            brush_edit: CreateSolidBrush(theme.edit_bg),
            checkbox_rects: [RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            }; 4],
        });

        let title = wide(&format!("{APP_NAME} v{APP_VERSION} Setup"));
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            full_w,
            full_h,
            null_mut(),
            null_mut(),
            inst,
            (&raw mut *state).cast(),
        );

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
