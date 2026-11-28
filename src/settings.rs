//! Settings window: a dark, rounded, custom-drawn panel with a side navigation (GDI+), opened by a left click on
//! the tray icon. Every change goes through the same `tap::apply_*` functions the tray menu uses.

use crate::{desktop::wide, tap};
use std::{
    cell::RefCell,
    ptr::{null, null_mut},
    sync::Once,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute},
        Gdi::{
            AlphaBlend, BeginPaint, BitBlt, CreateCompatibleDC, CreateRoundRectRgn, CreateDIBSection, DeleteDC, DeleteObject, EndPaint, SetWindowRgn,
            BITMAPINFO, BITMAPINFOHEADER, BLACKNESS, BLENDFUNCTION, DIB_RGB_COLORS,
            GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient, SelectObject, MONITORINFO,
            MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
        },
        GdiPlus as gp,
    },
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    UI::{
        HiDpi::{
            GetDpiForMonitor, GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            MDT_EFFECTIVE_DPI,
        },
        Controls::{MARGINS, WM_MOUSELEAVE},
        Shell::ShellExecuteW,
        Input::KeyboardAndMouse::{ReleaseCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE},
        WindowsAndMessaging::*,
    },
};

const W: f32 = 760.0;
const H: f32 = 560.0;
const NAV_W: f32 = 212.0;
const DRAG_H: f32 = 44.0;
const RADIUS: f32 = 16.0;
/// How far the Look page scrolls (stage + five style cards are taller than the window).
const LOOK_SCROLL: f32 = 120.0;
const PAD: f32 = 30.0;
const CX: f32 = NAV_W + PAD;
const CW: f32 = W - CX - PAD;

// The window sits on the DWM acrylic backdrop, so every fill is a translucent veil over it.
const BG: u32 = 0xC20E0E12;
const NAV: u32 = 0x0AFFFFFF;
const CARD: u32 = 0x0FFFFFFF;
const CARD_HOT: u32 = 0x1AFFFFFF;
const LINE: u32 = 0x14FFFFFF;
const TEXT: u32 = 0xFFF5F6F8;
const MUTED: u32 = 0x99FFFFFF;
const ACCENT: u32 = 0xFF33D649;
const ACCENT_SOFT: u32 = 0x2033D649;
const OFF: u32 = 0x66787880;
const DANGER: u32 = 0xFFFF6B6B;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Nav(u8),
    Style(u32),
    Surface(u32),
    Autostart,
    Lang(bool),
    Reapply,
    Exit,
    Close,
    Github,
    Donate,
}

type R = (f32, f32, f32, f32);

struct State {
    hwnd: HWND,
    owner: HWND,
    page: u8,
    hover: Option<Act>,
    hits: Vec<(R, Act)>,
    prefs: tap::Prefs,
    tracking: bool,
    scroll: f32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn startup_gdiplus() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let mut token = 0usize;
        let input = gp::GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        gp::GdiplusStartup(&mut token, &input, null_mut());
    });
}

/// Opens the window, or brings the existing one to the front.
pub fn open(owner: HWND) {
    unsafe {
        let existing = STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd));
        if let Some(hwnd) = existing {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            return;
        }
        startup_gdiplus();
        let hinst = GetModuleHandleW(null());
        let class = wide("PuwBarSettings");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfnWndProc = Some(proc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassExW(&wc);

        // The window is per-monitor DPI aware even though the tray process is not, so it stays sharp.
        let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let mon = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 || dx < 96 {
            dx = 96;
        }
        let k = dx as f32 / 96.0;
        let (w, h) = ((W * k) as i32, (H * k) as i32);
        let x = mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2;
        let y = mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top - h) / 2;
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("PuwBar").as_ptr(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        SetThreadDpiAwarenessContext(old);
        if hwnd.is_null() {
            return;
        }
        // The window region below makes the corners; keep DWM from rounding them a second time (33, DWMWCP_DONOTROUND = 1).
        let round: i32 = 1;
        DwmSetWindowAttribute(hwnd, 33, (&round as *const i32).cast(), 4);
        // Dark frame (DWMWA_USE_IMMERSIVE_DARK_MODE = 20) on a transient acrylic backdrop (DWMWA_SYSTEMBACKDROP_TYPE = 38, 3).
        let on: i32 = 1;
        DwmSetWindowAttribute(hwnd, 20, (&on as *const i32).cast(), 4);
        blur_behind(hwnd);
        let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
        let corner = (RADIUS * 2.0 * k) as i32;
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, corner, corner), 1);
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                owner,
                page: 0,
                hover: None,
                hits: Vec::new(),
                prefs: tap::prefs(),
                tracking: false,
                scroll: 0.0,
            })
        });
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

/// Acrylic blur of everything behind the window (the undocumented accent policy that shell flyouts use).
unsafe fn blur_behind(hwnd: HWND) {
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        color: u32,
        anim: u32,
    }
    #[repr(C)]
    struct CompAttr {
        attr: u32,
        data: *mut std::ffi::c_void,
        size: u32,
    }
    type SetWca = unsafe extern "system" fn(HWND, *mut CompAttr) -> i32;
    let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
    let Some(f) = GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast()) else {
        return;
    };
    let set: SetWca = std::mem::transmute(f);
    // State 4 = ACCENT_ENABLE_ACRYLICBLURBEHIND; colour is AABBGGRR, almost no tint, the veil is painted by the window itself.
    let mut policy = AccentPolicy { state: 4, flags: 2, color: 0x01130F0E, anim: 0 };
    let mut data = CompAttr { attr: 19, data: (&mut policy as *mut AccentPolicy).cast(), size: std::mem::size_of::<AccentPolicy>() as u32 };
    set(hwnd, &mut data);
}

fn xy(l: LPARAM, k: f32) -> (f32, f32) {
    ((l & 0xffff) as i16 as f32 / k, ((l >> 16) & 0xffff) as i16 as f32 / k)
}

fn hit_at(x: f32, y: f32) -> Option<Act> {
    STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, a)| *a))
    })
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            f(st);
        }
    });
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            paint(hwnd);
            return 0;
        }
        WM_NCHITTEST => {
            let mut pt = POINT { x: (l & 0xffff) as i16 as i32, y: ((l >> 16) & 0xffff) as i16 as i32 };
            ScreenToClient(hwnd, &mut pt);
            let k = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (x, y) = (pt.x as f32 / k, pt.y as f32 / k);
            // Top strip drags the window, except where the close button sits.
            if y < DRAG_H && x < W - 56.0 {
                return HTCAPTION as LRESULT;
            }
            return HTCLIENT as LRESULT;
        }
        WM_MOUSEMOVE => {
            let k = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (x, y) = xy(l, k);
            let hot = hit_at(x, y);
            let mut changed = false;
            with_state(|st| {
                if !st.tracking {
                    st.tracking = true;
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut t);
                }
                if st.hover != hot {
                    st.hover = hot;
                    changed = true;
                }
            });
            if changed {
                InvalidateRect(hwnd, null(), 0);
            }
            return 0;
        }
        WM_MOUSELEAVE => {
            with_state(|st| {
                st.tracking = false;
                st.hover = None;
            });
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        WM_LBUTTONDOWN => {
            let k = GetDpiForWindow(hwnd) as f32 / 96.0;
            let (x, y) = xy(l, k);
            if let Some(act) = hit_at(x, y) {
                ReleaseCapture();
                run(hwnd, act);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            let notches = ((w >> 16) & 0xffff) as i16 as f32 / 120.0;
            with_state(|st| {
                let max = if st.page == 1 { LOOK_SCROLL } else { 0.0 };
                st.scroll = (st.scroll - notches * 48.0).clamp(0.0, max);
            });
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        WM_KEYDOWN if w == VK_ESCAPE as usize => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_ACTIVATE if (w & 0xffff) != 0 => {
            // Autostart is changed through an elevated task, so re-read everything when the window is reached again.
            with_state(|st| st.prefs = tap::prefs());
            InvalidateRect(hwnd, null(), 0);
        }
        WM_DESTROY => {
            STATE.with(|s| *s.borrow_mut() = None);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, w, l)
}

fn inside(r: R, x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

unsafe fn run(hwnd: HWND, act: Act) {
    match act {
        Act::Close => {
            DestroyWindow(hwnd);
            return;
        }
        Act::Exit => {
            DestroyWindow(hwnd);
            tap::quit();
            return;
        }
        Act::Nav(p) => with_state(|st| {
            st.page = p;
            st.scroll = 0.0;
        }),
        Act::Style(v) => {
            tap::apply_style(v);
            with_state(|st| st.prefs.style = v);
        }
        Act::Surface(bit) => {
            tap::toggle_surface(bit);
            with_state(|st| st.prefs.surfaces ^= bit);
        }
        Act::Autostart => {
            let mut next = None;
            with_state(|st| next = Some(!st.prefs.autostart));
            if let Some(next) = next {
                tap::apply_autostart(next);
                with_state(|st| st.prefs.autostart = next);
            }
        }
        Act::Lang(en) => {
            let mut owner = null_mut();
            with_state(|st| {
                st.prefs.english = en;
                owner = st.owner;
            });
            tap::apply_language(owner, en);
        }
        Act::Reapply => tap::reapply(),
        Act::Github => {
            ShellExecuteW(null_mut(), wide("open").as_ptr(), wide("https://github.com/erikpuw/puwbar").as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
        Act::Donate => {
            ShellExecuteW(null_mut(), wide("open").as_ptr(), wide("https://paypal.me/erikpuw").as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
    }
    InvalidateRect(hwnd, null(), 0);
}

// ---------------------------------------------------------------- painting

struct Pen {
    g: *mut gp::GpGraphics,
    fam: *mut gp::GpFontFamily,
    dfam: *mut gp::GpFontFamily,
    /// Vertical shift of everything drawn from now on (page scroll and the title offset), so hit areas follow.
    dy: f32,
    ifam: *mut gp::GpFontFamily,
    fmt: [*mut gp::GpStringFormat; 4],
    k: f32,
    hits: Vec<(R, Act)>,
    hover: Option<Act>,
    en: bool,
}

impl Pen {
    fn t(&self, vi: &'static str, en: &'static str) -> &'static str {
        if self.en {
            en
        } else {
            vi
        }
    }

    unsafe fn path(&self, r: R, rad: f32) -> *mut gp::GpPath {
        let (x, y, w, h) = (r.0 * self.k, r.1 * self.k, r.2 * self.k, r.3 * self.k);
        let d = (rad * self.k * 2.0).min(w).min(h).max(1.0);
        let mut p = null_mut();
        gp::GdipCreatePath(gp::FillModeAlternate, &mut p);
        gp::GdipAddPathArc(p, x, y, d, d, 180.0, 90.0);
        gp::GdipAddPathArc(p, x + w - d, y, d, d, 270.0, 90.0);
        gp::GdipAddPathArc(p, x + w - d, y + h - d, d, d, 0.0, 90.0);
        gp::GdipAddPathArc(p, x, y + h - d, d, d, 90.0, 90.0);
        gp::GdipClosePathFigure(p);
        p
    }

    unsafe fn fill(&self, r: R, rad: f32, color: u32) {
        let mut b = null_mut();
        gp::GdipCreateSolidFill(color, &mut b);
        let p = self.path(r, rad);
        gp::GdipFillPath(self.g, b.cast(), p);
        gp::GdipDeletePath(p);
        gp::GdipDeleteBrush(b.cast());
    }

    unsafe fn stroke(&self, r: R, rad: f32, color: u32, width: f32) {
        let mut pen = null_mut();
        gp::GdipCreatePen1(color, width * self.k, gp::UnitPixel, &mut pen);
        let p = self.path(r, rad);
        gp::GdipDrawPath(self.g, pen, p);
        gp::GdipDeletePath(p);
        gp::GdipDeletePen(pen);
    }

    unsafe fn circle(&self, cx: f32, cy: f32, rad: f32, color: u32) {
        let mut b = null_mut();
        gp::GdipCreateSolidFill(color, &mut b);
        let k = self.k;
        gp::GdipFillEllipse(self.g, b.cast(), (cx - rad) * k, (cy - rad) * k, rad * 2.0 * k, rad * 2.0 * k);
        gp::GdipDeleteBrush(b.cast());
    }

    unsafe fn line(&self, a: (f32, f32), b: (f32, f32), color: u32, width: f32) {
        let mut pen = null_mut();
        gp::GdipCreatePen1(color, width * self.k, gp::UnitPixel, &mut pen);
        let k = self.k;
        gp::GdipDrawLine(self.g, pen, a.0 * k, a.1 * k, b.0 * k, b.1 * k);
        gp::GdipDeletePen(pen);
    }

    /// Diagonal two-colour gradient inside a rounded rect (a stand-in wallpaper for the style previews).
    unsafe fn gradient(&self, r: R, rad: f32, c1: u32, c2: u32) {
        let k = self.k;
        let a = gp::PointF { X: r.0 * k, Y: r.1 * k };
        let b = gp::PointF { X: (r.0 + r.2) * k, Y: (r.1 + r.3) * k };
        let mut br = null_mut();
        gp::GdipCreateLineBrush(&a, &b, c1, c2, 3, &mut br); // 3 = WrapModeTileFlipXY, no seam
        let path = self.path(r, rad);
        gp::GdipFillPath(self.g, br.cast(), path);
        gp::GdipDeletePath(path);
        gp::GdipDeleteBrush(br.cast());
    }

    /// `align`: 0 left, 1 center, 2 right (single line, vertically centred); 3 wrapping paragraph, top-left.
    unsafe fn text(&self, r: R, s: &str, size: f32, bold: bool, color: u32, align: usize) {
        let mut font = null_mut();
        gp::GdipCreateFont(self.fam, size * self.k, if bold { 1 } else { 0 }, gp::UnitPixel, &mut font);
        let mut b = null_mut();
        gp::GdipCreateSolidFill(color, &mut b);
        let rect = gp::RectF { X: r.0 * self.k, Y: r.1 * self.k, Width: r.2 * self.k, Height: r.3 * self.k };
        let w = wide(s);
        gp::GdipDrawString(self.g, w.as_ptr(), -1, font, &rect, self.fmt[align], b.cast());
        gp::GdipDeleteBrush(b.cast());
        gp::GdipDeleteFont(font);
    }

    /// One glyph of the Segoe Fluent Icons font, centred in `r`.
    unsafe fn icon(&self, r: R, glyph: &str, size: f32, color: u32) {
        if self.ifam.is_null() {
            return;
        }
        let mut font = null_mut();
        gp::GdipCreateFont(self.ifam, size * self.k, 0, gp::UnitPixel, &mut font);
        let mut b = null_mut();
        gp::GdipCreateSolidFill(color, &mut b);
        let rect = gp::RectF { X: r.0 * self.k, Y: r.1 * self.k, Width: r.2 * self.k, Height: r.3 * self.k };
        gp::GdipDrawString(self.g, wide(glyph).as_ptr(), -1, font, &rect, self.fmt[1], b.cast());
        gp::GdipDeleteBrush(b.cast());
        gp::GdipDeleteFont(font);
    }

    fn hot(&self, a: Act) -> bool {
        self.hover == Some(a)
    }

    fn hit(&mut self, r: R, a: Act) {
        self.hits.push(((r.0, r.1 + self.dy, r.2, r.3), a));
    }

    unsafe fn switch(&mut self, x: f32, y: f32, on: bool) {
        self.fill((x, y, 42.0, 22.0), 11.0, if on { ACCENT } else { OFF });
        self.circle(if on { x + 31.0 } else { x + 11.0 }, y + 11.0, 8.0, 0xFFFFFFFF);
    }

    unsafe fn button(&mut self, r: R, label: &str, a: Act, danger: bool) {
        let hot = self.hot(a);
        let base = if danger { 0x26FF6B6B } else { CARD };
        let over = if danger { 0x4DFF6B6B } else { CARD_HOT };
        self.fill(r, 9.0, if hot { over } else { base });
        self.stroke(r, 9.0, if danger { 0x66FF6B6B } else { LINE }, 1.0);
        self.text(r, label, 13.0, false, if danger { DANGER } else { TEXT }, 1);
        self.hit(r, a);
    }
}


unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let k = GetDpiForWindow(hwnd) as f32 / 96.0;
    let (pw, ph) = ((W * k) as i32, (H * k) as i32);
    let mem = CreateCompatibleDC(hdc);
    // A top-down 32-bit surface that starts fully transparent, so the acrylic backdrop shows through unpainted areas.
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = pw;
    bi.bmiHeader.biHeight = -ph;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    let mut bits: *mut std::ffi::c_void = null_mut();
    let bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    let old = SelectObject(mem, bmp);

    let mut snap = None;
    with_state(|st| snap = Some((st.page, st.hover, st.prefs.style, st.prefs.surfaces, st.prefs.autostart, st.prefs.english, st.scroll)));
    if let (Some((page, hover, style, surfaces, autostart, english, scroll)), false) = (snap, bits.is_null()) {
        let mut g = null_mut();
        gp::GdipCreateFromHDC(mem, &mut g);
        gp::GdipSetSmoothingMode(g, gp::SmoothingModeAntiAlias);
        gp::GdipSetTextRenderingHint(g, gp::TextRenderingHintAntiAliasGridFit);
        let mut fam = null_mut();
        gp::GdipCreateFontFamilyFromName(wide("Segoe UI").as_ptr(), null_mut(), &mut fam);
        let mut dfam = null_mut();
        if gp::GdipCreateFontFamilyFromName(wide("Segoe UI Variable Display").as_ptr(), null_mut(), &mut dfam) != 0 {
            dfam = fam;
        }
        let mut ifam = null_mut();
        if gp::GdipCreateFontFamilyFromName(wide("Segoe Fluent Icons").as_ptr(), null_mut(), &mut ifam) != 0 {
            ifam = null_mut();
            if gp::GdipCreateFontFamilyFromName(wide("Segoe MDL2 Assets").as_ptr(), null_mut(), &mut ifam) != 0 {
                ifam = null_mut();
            }
        }
        let mut fmt = [null_mut(); 4];
        for (i, f) in fmt.iter_mut().enumerate() {
            let single = i < 3;
            gp::GdipCreateStringFormat(if single { 0x1000 } else { 0 }, 0, f); // 0x1000 = StringFormatFlagsNoWrap
            gp::GdipSetStringFormatAlign(*f, if single { i as i32 } else { 0 });
            gp::GdipSetStringFormatLineAlign(*f, if single { 1 } else { 0 });
        }
        let mut p = Pen { g, fam, dfam, dy: 0.0, ifam, fmt, k, hits: Vec::new(), hover, en: english };
        draw(&mut p, page, scroll, style, surfaces, autostart);
        let hits = std::mem::take(&mut p.hits);
        for f in fmt {
            gp::GdipDeleteStringFormat(f);
        }
        if !ifam.is_null() {
            gp::GdipDeleteFontFamily(ifam);
        }
        if dfam != fam {
            gp::GdipDeleteFontFamily(dfam);
        }
        gp::GdipDeleteFontFamily(fam);
        gp::GdipDeleteGraphics(g);
        with_state(|st| st.hits = hits);

        // AlphaBlend wants premultiplied alpha; GDI+ leaves straight alpha.
        let px = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (pw * ph) as usize);
        for v in px.iter_mut() {
            let a = *v >> 24;
            if a < 255 {
                let ch = |sh: u32| (((*v >> sh) & 0xff) * a + 127) / 255;
                *v = (a << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0);
            }
        }
        BitBlt(hdc, 0, 0, pw, ph, null_mut(), 0, 0, BLACKNESS);
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
        AlphaBlend(hdc, 0, 0, pw, ph, mem, 0, 0, pw, ph, blend);
    }

    SelectObject(mem, old);
    DeleteObject(bmp);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

const STYLES: [(&str, &str, &str, &str); 5] = [
    ("Kính mờ", "Frosted glass", "Kính nổi có blur, tự tăng tương phản.", "Floating glass with blur and auto contrast."),
    ("Kính trong suốt", "Crystal clear", "Kính nổi không blur, có ánh sáng bề mặt.", "Floating glass without blur, lit surface."),
    ("Không viền", "Borderless", "Không nền, không viền, icon phản chiếu.", "No fill, no rim, icons reflect on the floor."),
    ("Kính mờ phẳng", "Flat frosted", "Kính phẳng, viền mảnh, không bóng đổ.", "Flat glass, hairline rim, no shadow."),
    ("Trong suốt hoàn toàn", "Fully transparent", "Không nền, không viền, không blur.", "No fill, no rim, no blur, no shadow."),
];

/// Segoe Fluent Icons glyphs for the five pages.
const NAV_ICONS: [&str; 5] = ["\u{E80F}", "\u{E790}", "\u{E71D}", "\u{E713}", "\u{E946}"];

/// The app icon from the exe's own ICON resource, drawn into a `s` x `s` tile.
unsafe fn logo(p: &Pen, x: f32, y: f32, s: f32) {
    thread_local! {
        static ICON: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    if ICON.with(|c| c.get()) == 0 {
        let icon = LoadImageW(GetModuleHandleW(null()), 1 as _, IMAGE_ICON, 256, 256, 0);
        if !icon.is_null() {
            let mut bitmap = null_mut();
            gp::GdipCreateBitmapFromHICON(icon, &mut bitmap);
            DestroyIcon(icon);
            ICON.with(|c| c.set(bitmap as usize));
        }
    }
    let bitmap = ICON.with(|c| c.get()) as *mut gp::GpBitmap;
    if !bitmap.is_null() {
        gp::GdipSetInterpolationMode(p.g, gp::InterpolationModeHighQualityBicubic);
        let k = p.k;
        gp::GdipDrawImageRect(p.g, bitmap.cast(), x * k, y * k, s * k, s * k);
    }
}

unsafe fn draw(p: &mut Pen, page: u8, scroll: f32, style: u32, surfaces: u32, autostart: bool) {
    p.fill((0.0, 0.0, W, H), 0.0, BG);
    p.fill((0.0, 0.0, NAV_W, H), 0.0, NAV);
    p.line((NAV_W, 0.0), (NAV_W, H), LINE, 1.0);

    // Page content: shifted up a little (the title sits near y 44) and by the scroll offset, clipped to the content area.
    let k = p.k;
    p.dy = -12.0 - scroll;
    gp::GdipSetClipRect(p.g, (NAV_W + 1.0) * k, 0.0, (W - NAV_W) * k, H * k, 0);
    gp::GdipTranslateWorldTransform(p.g, 0.0, p.dy * k, 0);
    let (title, hint) = match page {
        0 => (p.t("Tổng quan", "Overview"), p.t("Tình trạng hiện tại của PuwBar. Bấm một thẻ để chỉnh phần đó.", "How PuwBar is set up now. Click a card to change that part.")),
        1 => (p.t("Giao diện", "Look"), p.t("Chọn kiểu taskbar. Áp dụng ngay, không cần khởi động lại.", "Pick the taskbar style. It applies at once, no restart.")),
        2 => (p.t("Bề mặt kính", "Glass surfaces"), p.t("Chọn các cửa sổ hệ thống được phủ kính.", "Choose which system windows get the glass.")),
        3 => (p.t("Hệ thống", "System"), p.t("Cách PuwBar chạy trên máy này.", "How PuwBar runs on this PC.")),
        _ => (p.t("Giới thiệu", "About"), ""),
    };
    let body_fam = p.fam;
    p.fam = p.dfam;
    p.text((CX, 58.0, CW, 32.0), title, 24.0, true, TEXT, 0);
    p.fam = body_fam;
    p.text((CX, 90.0, CW, 22.0), hint, 12.0, false, MUTED, 0);
    match page {
        0 => page_overview(p, style, surfaces, autostart),
        1 => page_look(p, style),
        2 => page_surfaces(p, surfaces),
        3 => page_system(p, autostart),
        _ => page_about(p),
    }
    gp::GdipResetWorldTransform(p.g);
    gp::GdipResetClip(p.g);
    p.dy = 0.0;

    let max = if page == 1 { LOOK_SCROLL } else { 0.0 };
    if max > 0.0 {
        let track = H - 24.0;
        let thumb = track * H / (H + max);
        p.fill((W - 8.0, 12.0 + scroll / max * (track - thumb), 3.0, thumb), 1.5, 0x40FFFFFF);
    }

    // Navigation
    logo(p, 18.0, 20.0, 30.0);
    p.text((58.0, 19.0, NAV_W - 70.0, 18.0), "PuwBar", 14.0, true, TEXT, 0);
    p.text((58.0, 36.0, NAV_W - 70.0, 16.0), p.t(concat!("Phiên bản ", env!("CARGO_PKG_VERSION")), concat!("Version ", env!("CARGO_PKG_VERSION"))), 11.0, false, MUTED, 0);
    let pages = [
        p.t("Tổng quan", "Overview"),
        p.t("Giao diện", "Look"),
        p.t("Bề mặt kính", "Glass surfaces"),
        p.t("Hệ thống", "System"),
        p.t("Giới thiệu", "About"),
    ];
    for (i, name) in pages.iter().enumerate() {
        let r = (12.0, 78.0 + i as f32 * 38.0, NAV_W - 24.0, 36.0);
        let act = Act::Nav(i as u8);
        let on = page as usize == i;
        if on {
            p.fill(r, 8.0, 0x1AFFFFFF);
        } else if p.hot(act) {
            p.fill(r, 8.0, 0x0FFFFFFF);
        }
        p.icon((r.0 + 8.0, r.1, 28.0, r.3), NAV_ICONS[i], 16.0, if on { ACCENT } else { MUTED });
        p.text((r.0 + 42.0, r.1, r.2 - 42.0, r.3), name, 13.0, false, if on { TEXT } else { MUTED }, 0);
        p.hit(r, act);
    }

    let close = (W - 44.0, 8.0, 30.0, 30.0);
    if p.hot(Act::Close) {
        p.fill(close, 8.0, CARD_HOT);
    }
    p.line((close.0 + 10.0, close.1 + 10.0), (close.0 + 20.0, close.1 + 20.0), MUTED, 1.4);
    p.line((close.0 + 20.0, close.1 + 10.0), (close.0 + 10.0, close.1 + 20.0), MUTED, 1.4);
    p.hit(close, Act::Close);

    // Rim and the thin highlight along the top edge.
    p.stroke((0.5, 0.5, W - 1.0, H - 1.0), RADIUS, 0x21FFFFFF, 1.0);
    p.line((RADIUS, 1.0), (W - RADIUS, 1.0), 0x1AFFFFFF, 1.0);
}

/// A clickable card; registers its own hit area.
unsafe fn card(p: &mut Pen, r: R, act: Act) {
    p.fill(r, 14.0, if p.hot(act) { CARD_HOT } else { CARD });
    p.stroke(r, 14.0, LINE, 1.0);
    p.hit(r, act);
}

unsafe fn page_overview(p: &mut Pen, style: u32, surfaces: u32, autostart: bool) {
    let big_w = (CW - 12.0) * 0.6;
    let big = (CX, 128.0, big_w, 112.0);
    card(p, big, Act::Nav(1));
    let (vi, en, _, _) = STYLES[(style as usize).min(STYLES.len() - 1)];
    let on_count = tap::SURFACE_BITS.iter().filter(|b| surfaces & **b != 0).count();
    p.text((big.0 + 18.0, big.1 + 12.0, big.2 - 36.0, 18.0), p.t("Giao diện", "Look"), 12.0, false, MUTED, 0);
    p.text((big.0 + 18.0, big.1 + 34.0, big.2 - 36.0, 32.0), if p.en { en } else { vi }, 20.0, true, TEXT, 0);
    let status = if p.en { format!("{on_count} of 7 glass surfaces on") } else { format!("{on_count} trên 7 khu vực kính đang bật") };
    p.text((big.0 + 18.0, big.1 + 74.0, big.2 - 36.0, 22.0), &status, 12.0, false, ACCENT, 0);

    let sx = CX + big_w + 12.0;
    let sw = CW - big_w - 12.0;
    let c1 = (sx, 128.0, sw, 50.0);
    card(p, c1, Act::Nav(2));
    p.text((c1.0 + 16.0, c1.1 + 5.0, sw - 24.0, 18.0), p.t("Khu vực kính", "Glass surfaces"), 11.0, false, MUTED, 0);
    let val = if p.en { format!("{on_count} of 7") } else { format!("{on_count} trên 7") };
    p.text((c1.0 + 16.0, c1.1 + 23.0, sw - 24.0, 22.0), &val, 13.5, true, TEXT, 0);
    let c2 = (sx, 190.0, sw, 50.0);
    card(p, c2, Act::Nav(3));
    p.text((c2.0 + 16.0, c2.1 + 5.0, sw - 24.0, 18.0), p.t("Ngôn ngữ", "Language"), 11.0, false, MUTED, 0);
    p.text((c2.0 + 16.0, c2.1 + 23.0, sw - 24.0, 22.0), if p.en { "English" } else { "Tiếng Việt" }, 13.5, true, TEXT, 0);

    p.text((CX + 4.0, 258.0, CW, 20.0), p.t("Bật nhanh", "Quick toggles"), 12.0, false, MUTED, 0);
    let group = (CX, 284.0, CW, 138.0);
    p.fill(group, 14.0, CARD);
    p.stroke(group, 14.0, LINE, 1.0);
    let rows = [
        (p.t("Khởi động cùng Windows", "Start with Windows"), Act::Autostart, autostart),
        (p.t("Kính cho Start Menu", "Glass for Start Menu"), Act::Surface(tap::SURFACE_BITS[0]), surfaces & tap::SURFACE_BITS[0] != 0),
        (p.t("Kính cho Control Center", "Glass for Control Center"), Act::Surface(tap::SURFACE_BITS[1]), surfaces & tap::SURFACE_BITS[1] != 0),
    ];
    for (i, (label, act, on)) in rows.into_iter().enumerate() {
        let r = (group.0, group.1 + i as f32 * 46.0, group.2, 46.0);
        if p.hot(act) {
            p.fill((r.0 + 4.0, r.1 + 4.0, r.2 - 8.0, r.3 - 8.0), 10.0, 0x12FFFFFF);
        }
        p.text((r.0 + 18.0, r.1, r.2 - 90.0, r.3), label, 13.5, false, TEXT, 0);
        p.switch(r.0 + r.2 - 60.0, r.1 + 12.0, on);
        p.hit(r, act);
        if i < 2 {
            p.line((r.0 + 16.0, r.1 + r.3), (r.0 + r.2 - 16.0, r.1 + r.3), LINE, 1.0);
        }
    }
}

unsafe fn page_look(p: &mut Pen, style: u32) {
    // Live preview: the chosen style on a stand-in wallpaper.
    let stage = (CX, 128.0, CW, 150.0);
    p.gradient(stage, 14.0, 0xFF4A7BD6, 0xFF8A5BD0);
    p.stroke(stage, 14.0, LINE, 1.0);
    pill_art(p, (stage.0 + (CW - 300.0) / 2.0, stage.1 + stage.3 - 62.0, 300.0, 44.0), style as usize, 6);

    let (cw, ch, gap) = ((CW - 12.0) / 2.0, 118.0, 12.0);
    for (i, (vi, en, dvi, den)) in STYLES.iter().enumerate() {
        let (col, row) = ((i % 2) as f32, (i / 2) as f32);
        let r = (CX + col * (cw + gap), 290.0 + row * (ch + gap), cw, ch);
        let act = Act::Style(i as u32);
        let on = style == i as u32;
        p.fill(r, 12.0, if on { ACCENT_SOFT } else if p.hot(act) { CARD_HOT } else { CARD });
        p.stroke(r, 12.0, if on { ACCENT } else { LINE }, if on { 1.5 } else { 1.0 });
        let thumb = (r.0 + 12.0, r.1 + 12.0, 64.0, 76.0);
        p.gradient(thumb, 8.0, 0xFF4A7BD6, 0xFF8A5BD0);
        pill_art(p, (thumb.0 + 5.0, thumb.1 + thumb.3 - 24.0, 54.0, 16.0), i, 3);
        let (name, desc) = if p.en { (en, den) } else { (vi, dvi) };
        p.text((r.0 + 84.0, r.1 + 12.0, cw - 92.0, 20.0), name, 13.0, true, TEXT, 0);
        p.text((r.0 + 84.0, r.1 + 36.0, cw - 92.0, 60.0), desc, 11.0, false, MUTED, 3);
        p.hit(r, act);
    }
}

/// A taskbar pill in a given style with `n` app icons, drawn over whatever is behind it.
unsafe fn pill_art(p: &Pen, pill: R, style: usize, n: usize) {
    match style {
        0 => {
            p.fill(pill, pill.3 * 0.36, 0x40FFFFFF);
            p.stroke(pill, pill.3 * 0.36, 0x77FFFFFF, 1.0);
        }
        1 => {
            p.fill(pill, pill.3 * 0.36, 0x1AFFFFFF);
            p.stroke(pill, pill.3 * 0.36, 0xB3FFFFFF, 1.0);
        }
        3 => {
            p.fill(pill, pill.3 * 0.36, 0x4D101018);
            p.stroke(pill, pill.3 * 0.36, 0x66FFFFFF, 0.7);
        }
        _ => {}
    }
    let s = pill.3 * 0.46;
    let pad = pill.3 * 0.5;
    let step = if n > 1 { (pill.2 - 2.0 * pad - s) / (n - 1) as f32 } else { 0.0 };
    for i in 0..n {
        let x = pill.0 + pad + i as f32 * step;
        p.fill((x, pill.1 + (pill.3 - s) / 2.0, s, s), s * 0.3, 0xE6FFFFFF);
        if style == 2 {
            p.fill((x, pill.1 + pill.3 + 2.0, s, s * 0.6), s * 0.3, 0x44FFFFFF);
        }
    }
}

unsafe fn page_surfaces(p: &mut Pen, surfaces: u32) {
    let names = [
        "Start Menu",
        "Control Center",
        p.t("Thông báo", "Notifications"),
        p.t("Lịch", "Calendar"),
        "Jump List",
        "Tooltip",
        p.t("Xem trước cửa sổ", "Window previews"),
    ];
    for (i, (name, bit)) in names.iter().zip(tap::SURFACE_BITS).enumerate() {
        let r = (CX, 128.0 + i as f32 * 49.0, CW, 42.0);
        let act = Act::Surface(bit);
        p.fill(r, 12.0, if p.hot(act) { CARD_HOT } else { CARD });
        p.text((r.0 + 16.0, r.1, r.2 - 90.0, r.3), name, 13.5, false, TEXT, 0);
        p.switch(r.0 + r.2 - 58.0, r.1 + 10.0, surfaces & bit != 0);
        p.hit(r, act);
    }
}

unsafe fn page_system(p: &mut Pen, autostart: bool) {
    let row = (CX, 128.0, CW, 56.0);
    p.fill(row, 12.0, if p.hot(Act::Autostart) { CARD_HOT } else { CARD });
    p.text((row.0 + 16.0, row.1 + 8.0, row.2 - 90.0, 20.0), p.t("Khởi động cùng Windows", "Start with Windows"), 13.5, false, TEXT, 0);
    p.text((row.0 + 16.0, row.1 + 29.0, row.2 - 90.0, 18.0), p.t("Cần quyền quản trị một lần để tạo tác vụ.", "Needs admin rights once to create the task."), 11.0, false, MUTED, 0);
    p.switch(row.0 + row.2 - 58.0, row.1 + 17.0, autostart);
    p.hit(row, Act::Autostart);

    let lang = (CX, 194.0, CW, 56.0);
    p.fill(lang, 12.0, CARD);
    p.text((lang.0 + 16.0, lang.1, 160.0, lang.3), p.t("Ngôn ngữ", "Language"), 13.5, false, TEXT, 0);
    let en = p.en;
    for (i, (label, is_en)) in [("Tiếng Việt", false), ("English", true)].iter().enumerate() {
        let r = (lang.0 + lang.2 - 232.0 + i as f32 * 112.0, lang.1 + 13.0, 104.0, 30.0);
        let act = Act::Lang(*is_en);
        let on = en == *is_en;
        p.fill(r, 8.0, if on { ACCENT_SOFT } else if p.hot(act) { CARD_HOT } else { CARD });
        if on {
            p.stroke(r, 8.0, ACCENT, 1.2);
        }
        p.text(r, label, 12.5, on, if on { TEXT } else { MUTED }, 1);
        p.hit(r, act);
    }

    let label = p.t("Áp dụng lại giao diện", "Reapply taskbar glass");
    p.button((CX, 270.0, 220.0, 38.0), label, Act::Reapply, false);
    let label = p.t("Thoát PuwBar", "Exit PuwBar");
    p.button((CX + 232.0, 270.0, 170.0, 38.0), label, Act::Exit, true);
    p.text((CX, 320.0, CW, 22.0), p.t("Phím tắt thoát nhanh: Ctrl+Alt+F12", "Quick exit shortcut: Ctrl+Alt+F12"), 11.5, false, MUTED, 0);
}

unsafe fn page_about(p: &mut Pen) {
    let mid = CX + CW / 2.0;
    logo(p, mid - 34.0, 124.0, 68.0);
    p.text((CX, 204.0, CW, 32.0), "PuwBar", 22.0, true, TEXT, 1);
    p.text((CX, 234.0, CW, 20.0), p.t(concat!("Phiên bản ", env!("CARGO_PKG_VERSION")), concat!("Version ", env!("CARGO_PKG_VERSION"))), 12.0, false, MUTED, 1);

    let bullets = [
        p.t("Chia taskbar thành các cụm kính nổi, giữ nguyên chức năng gốc", "Splits the taskbar into floating glass pills, native features kept"),
        p.t("Năm kiểu giao diện, từ kính mờ tới trong suốt hoàn toàn", "Five styles, from frosted glass to fully transparent"),
        p.t("Phủ kính cho Start, Quick Settings, Thông báo, Lịch, Jump List", "Glass on Start, Quick Settings, Notifications, Calendar, Jump List"),
        p.t("Taskbar ngang hoặc dọc, căn giữa hoặc căn trái", "Horizontal or vertical taskbar, centered or left-aligned"),
        p.t("Thoát PuwBar là taskbar trở về nguyên bản", "Quit PuwBar and the taskbar goes back to stock"),
        p.t("Nhẹ, viết bằng Rust và C++, không WebView", "Light, written in Rust and C++, no WebView"),
    ];
    for (i, line) in bullets.iter().enumerate() {
        let y = 270.0 + i as f32 * 24.0;
        p.circle(CX + 44.0, y + 11.0, 2.5, ACCENT);
        p.text((CX + 58.0, y, CW - 80.0, 22.0), line, 12.5, false, 0xFFD9DBE2, 0);
    }
    p.text((CX, 424.0, CW, 20.0), p.t("Thấy PuwBar có ích thì mời tác giả một ly cà phê.", "If PuwBar helps, buy the author a coffee."), 11.5, false, MUTED, 1);

    let gh = (mid - 235.0, 456.0, 130.0, 38.0);
    let ghot = p.hot(Act::Github);
    p.fill(gh, 19.0, if ghot { 0xFF4BE07A } else { ACCENT });
    p.text(gh, p.t("Mở GitHub", "Open GitHub"), 13.0, true, 0xFF0B1A10, 1);
    p.hit(gh, Act::Github);
    let dn = (mid - 95.0, 456.0, 150.0, 38.0);
    p.fill(dn, 19.0, if p.hot(Act::Donate) { CARD_HOT } else { 0x00000000 });
    p.stroke(dn, 19.0, 0x66FFFFFF, 1.0);
    p.text(dn, p.t("Ủng hộ PayPal", "Donate (PayPal)"), 13.0, false, TEXT, 1);
    p.hit(dn, Act::Donate);
    let ex = (mid + 65.0, 456.0, 170.0, 38.0);
    p.fill(ex, 19.0, if p.hot(Act::Exit) { CARD_HOT } else { 0x00000000 });
    p.stroke(ex, 19.0, 0x66FFFFFF, 1.0);
    p.text(ex, p.t("Thoát PuwBar", "Exit PuwBar"), 13.0, false, TEXT, 1);
    p.hit(ex, Act::Exit);
}
