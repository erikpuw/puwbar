//! Settings window: a dark, rounded, custom-drawn panel with a side navigation (GDI+), opened by a left click on
//! the tray icon. Every change goes through the same `tap::apply_*` functions the tray menu uses.

use crate::{
    desktop::wide,
    gfx::{color, Gfx},
    tap,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ptr::{null, null_mut},
    time::Instant,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_numerics::{Matrix3x2, Vector2};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::{
            CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetMonitorInfoW, InvalidateRect,
            MonitorFromPoint, ScreenToClient, ValidateRect, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
            MONITORINFO, MONITOR_DEFAULTTONEAREST,
        },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::WM_MOUSELEAVE,
        HiDpi::{
            GetDpiForMonitor, GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            MDT_EFFECTIVE_DPI,
        },
        Input::KeyboardAndMouse::{ReleaseCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

const W: f32 = 760.0;
const H: f32 = 560.0;
const NAV_W: f32 = 212.0;
const DRAG_H: f32 = 44.0;
const RADIUS: f32 = 16.0;
/// Screen y where the scrolling body starts, just under the title block.
const BODY_TOP: f32 = 108.0;
/// Bottom edge (page coordinates) of each page's content, in page order; a page scrolls only when it reaches past the window.
const PAGE_BOTTOM: [f32; 5] = [422.0, 416.0, 464.0, 342.0, 494.0];

fn max_scroll(page: u8) -> f32 {
    (PAGE_BOTTOM[(page as usize).min(PAGE_BOTTOM.len() - 1)] - (H - 12.0)).max(0.0)
}
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    /// Animated values keyed by (element, channel): current and target, 0..1. Channel 0 hover, 1 selected/on.
    anims: HashMap<(Act, u8), (f32, f32)>,
    last_tick: Instant,
    /// When the current page was entered, for the fade and slide.
    page_start: Instant,
    /// Windows' "animation effects" setting is off: jump straight to the end state.
    reduce: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static GFX: RefCell<Option<Gfx>> = const { RefCell::new(None) };
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
            WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
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
        // Stay DPI aware until the window is fully set up: a region or size set from an unaware thread gets scaled again.
        if hwnd.is_null() {
            SetThreadDpiAwarenessContext(old);
            return;
        }
        // The window region below makes the corners; keep DWM from rounding them a second time (33, DWMWCP_DONOTROUND = 1).
        let round: i32 = 1;
        DwmSetWindowAttribute(hwnd, 33, (&round as *const i32).cast(), 4);
        // Dark frame (DWMWA_USE_IMMERSIVE_DARK_MODE = 20) on a transient acrylic backdrop (DWMWA_SYSTEMBACKDROP_TYPE = 38, 3).
        let on: i32 = 1;
        DwmSetWindowAttribute(hwnd, 20, (&on as *const i32).cast(), 4);
        let gfx = match Gfx::new(windows::Win32::Foundation::HWND(hwnd as _), w, h, RADIUS * k, icon_pixels()) {
            Ok(g) => g,
            Err(_) => {
                DestroyWindow(hwnd);
                SetThreadDpiAwarenessContext(old);
                return;
            }
        };
        GFX.with(|g| *g.borrow_mut() = Some(gfx));
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
                anims: HashMap::new(),
                last_tick: Instant::now(),
                page_start: Instant::now(),
                reduce: !animations_enabled(),
            })
        });
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        frame(hwnd);
        SetThreadDpiAwarenessContext(old);
    }
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
            frame(hwnd);
            return 0;
        }
        WM_TIMER if w == FRAME_TIMER => {
            frame(hwnd);
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
                let max = max_scroll(st.page);
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
            GFX.with(|g| *g.borrow_mut() = None);
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
            st.page_start = Instant::now();
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

struct Pen<'a> {
    g: &'a Gfx,
    k: f32,
    hits: Vec<(R, Act)>,
    hover: Option<Act>,
    en: bool,
    /// Vertical shift of everything drawn from now on (page scroll and the title offset), so hit areas follow.
    dy: f32,
    /// Hit areas above this screen y are cut off (scrolled out from under the fixed title).
    clip_top: f32,
    /// Current animated values (read) and the targets this frame asks for (written).
    vals: HashMap<(Act, u8), f32>,
    targets: RefCell<HashMap<(Act, u8), f32>>,
    /// Page entrance: opacity 0..1 and a short upward slide in px.
    fade: f32,
    slide: f32,
}

/// Linear blend of two `0xAARRGGBB` colours.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        (x + (y - x) * t).round() as u32
    };
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// False when Windows has client-area animation effects turned off (the reduced-motion preference).
unsafe fn animations_enabled() -> bool {
    let mut on: i32 = 1;
    SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut on as *mut i32).cast(), 0);
    on != 0
}

fn rc(r: R) -> D2D_RECT_F {
    D2D_RECT_F { left: r.0, top: r.1, right: r.0 + r.2, bottom: r.1 + r.3 }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

impl Pen<'_> {
    fn t(&self, vi: &'static str, en: &'static str) -> &'static str {
        if self.en {
            en
        } else {
            vi
        }
    }

    unsafe fn fill(&self, r: R, rad: f32, c: u32) {
        self.g.set_color(c);
        if rad <= 0.0 {
            self.g.rt.FillRectangle(&rc(r), &self.g.brush);
        } else {
            let rr = D2D1_ROUNDED_RECT { rect: rc(r), radiusX: rad.min(r.2 / 2.0), radiusY: rad.min(r.3 / 2.0) };
            self.g.rt.FillRoundedRectangle(&rr, &self.g.brush);
        }
    }

    unsafe fn stroke(&self, r: R, rad: f32, c: u32, width: f32) {
        self.g.set_color(c);
        let half = width / 2.0;
        let inner = (r.0 + half, r.1 + half, r.2 - width, r.3 - width);
        let rad = (rad - half).max(0.0);
        let rr = D2D1_ROUNDED_RECT { rect: rc(inner), radiusX: rad.min(inner.2 / 2.0), radiusY: rad.min(inner.3 / 2.0) };
        self.g.rt.DrawRoundedRectangle(&rr, &self.g.brush, width, None);
    }

    unsafe fn circle(&self, cx: f32, cy: f32, rad: f32, c: u32) {
        self.g.set_color(c);
        self.g.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad, radiusY: rad }, &self.g.brush);
    }

    unsafe fn line(&self, a: (f32, f32), b: (f32, f32), c: u32, width: f32) {
        self.g.set_color(c);
        self.g.rt.DrawLine(pt(a.0, a.1), pt(b.0, b.1), &self.g.brush, width, None);
    }

    /// Diagonal two-colour gradient inside a rounded rect (a stand-in wallpaper for the style previews).
    unsafe fn gradient(&self, r: R, rad: f32, c1: u32, c2: u32) {
        let fade = |c: u32| {
            let mut x = color(c);
            x.a *= self.g.alpha.get();
            x
        };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: fade(c1) }, D2D1_GRADIENT_STOP { position: 1.0, color: fade(c2) }];
        let Ok(coll) = self.g.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(r.0, r.1), endPoint: pt(r.0 + r.2, r.1 + r.3) };
        let Ok(brush) = self.g.rt.CreateLinearGradientBrush(&props, None, &coll) else { return };
        let rr = D2D1_ROUNDED_RECT { rect: rc(r), radiusX: rad, radiusY: rad };
        self.g.rt.FillRoundedRectangle(&rr, &brush);
    }

    unsafe fn text_in(&self, font: (PCWSTR, f32, bool), r: R, s: &str, c: u32, align: usize) {
        let (family, size, bold) = font;
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let Ok(fmt) = self.g.dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("vi-VN")) else {
            return;
        };
        let (horizontal, wrap) = match align {
            0 => (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP),
            1 => (DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_NO_WRAP),
            2 => (DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP),
            _ => (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_WRAP),
        };
        let _ = fmt.SetTextAlignment(horizontal);
        let _ = fmt.SetParagraphAlignment(if align < 3 { DWRITE_PARAGRAPH_ALIGNMENT_CENTER } else { DWRITE_PARAGRAPH_ALIGNMENT_NEAR });
        let _ = fmt.SetWordWrapping(wrap);
        self.g.set_color(c);
        let text: Vec<u16> = s.encode_utf16().collect();
        self.g.rt.DrawText(&text, &fmt, &rc(r), &self.g.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }

    /// `align`: 0 left, 1 center, 2 right (single line, vertically centred); 3 wrapping paragraph, top-left.
    unsafe fn text(&self, r: R, s: &str, size: f32, bold: bool, c: u32, align: usize) {
        self.text_in((w!("Segoe UI"), size, bold), r, s, c, align);
    }

    unsafe fn title(&self, r: R, s: &str) {
        self.text_in((self.g.title_family, 24.0, true), r, s, TEXT, 0);
    }

    /// One glyph of the Segoe Fluent Icons font, centred in `r`.
    unsafe fn icon(&self, r: R, glyph: &str, size: f32, c: u32) {
        if let Some(family) = self.g.icon_family {
            self.text_in((family, size, false), r, glyph, c, 1);
        }
    }

    fn hot(&self, a: Act) -> bool {
        self.hover == Some(a)
    }

    /// The animated value for `key`, easing towards `target`; it starts at the target so nothing animates on first sight.
    fn anim(&self, key: (Act, u8), target: f32) -> f32 {
        self.targets.borrow_mut().insert(key, target);
        self.vals.get(&key).copied().unwrap_or(target)
    }

    /// Eased 0..1 hover amount for an element.
    fn hov(&self, a: Act) -> f32 {
        self.anim((a, 0), if self.hot(a) { 1.0 } else { 0.0 })
    }

    fn hit(&mut self, r: R, a: Act) {
        let top = (r.1 + self.dy).max(self.clip_top);
        let bottom = r.1 + self.dy + r.3;
        if bottom > top {
            self.hits.push(((r.0, top, r.2, bottom - top), a));
        }
    }

    unsafe fn switch(&mut self, x: f32, y: f32, on: bool, act: Act) {
        let t = self.anim((act, 1), if on { 1.0 } else { 0.0 });
        self.fill((x, y, 42.0, 22.0), 11.0, mix(OFF, ACCENT, t));
        self.circle(x + 11.0 + 20.0 * t, y + 11.0, 8.0, 0xFFFFFFFF);
    }

    unsafe fn button(&mut self, r: R, label: &str, a: Act, danger: bool) {
        let hot = self.hov(a);
        let base = if danger { 0x26FF6B6B } else { CARD };
        let over = if danger { 0x4DFF6B6B } else { CARD_HOT };
        self.fill(r, 9.0, mix(base, over, hot));
        self.stroke(r, 9.0, if danger { 0x66FF6B6B } else { LINE }, 1.0);
        self.text(r, label, 13.0, false, if danger { DANGER } else { TEXT }, 1);
        self.hit(r, a);
    }
}

const FRAME_TIMER: usize = 1;

/// Draws a frame and keeps a 60 Hz timer running only while something is still moving.
unsafe fn frame(hwnd: HWND) {
    if render(hwnd) {
        SetTimer(hwnd, FRAME_TIMER, 16, None);
    } else {
        KillTimer(hwnd, FRAME_TIMER);
    }
}

/// One frame: advance the animations, Direct2D into the swap chain, present. Returns true while more frames are needed.
/// Redraws replace the whole frame, so nothing flashes.
unsafe fn render(hwnd: HWND) -> bool {
    ValidateRect(hwnd, null());
    let k = GetDpiForWindow(hwnd) as f32 / 96.0;
    let mut snap = None;
    with_state(|st| {
        let now = Instant::now();
        let dt = now.duration_since(st.last_tick).as_secs_f32().min(0.05);
        st.last_tick = now;
        let reduce = st.reduce;
        // Exponential approach: interruptible (a new target just bends the motion) and never overshoots.
        let ease = 1.0 - (-dt * 16.0).exp();
        for (cur, target) in st.anims.values_mut() {
            *cur = if reduce || (*target - *cur).abs() < 0.003 { *target } else { *cur + (*target - *cur) * ease };
        }
        let t = if reduce { 1.0 } else { (now.duration_since(st.page_start).as_secs_f32() / 0.3).min(1.0) };
        let vals: HashMap<(Act, u8), f32> = st.anims.iter().map(|(key, v)| (*key, v.0)).collect();
        snap = Some((st.page, st.hover, st.prefs.style, st.prefs.surfaces, st.prefs.autostart, st.prefs.english, st.scroll, vals, t));
    });
    let Some((page, hover, style, surfaces, autostart, english, scroll, vals, t)) = snap else {
        return false;
    };
    let entering = t < 1.0;
    let eased = 1.0 - (1.0 - t).powi(3);
    let mut hits = Vec::new();
    let mut targets = HashMap::new();
    GFX.with(|cell| {
        let cell = cell.borrow();
        let Some(g) = cell.as_ref() else { return };
        g.rt.BeginDraw();
        g.rt.Clear(Some(&color(0x00000000)));
        g.rt.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 });
        let mut p = Pen { g, k, hits: Vec::new(), hover, en: english, dy: 0.0, clip_top: f32::MIN, vals, targets: RefCell::new(HashMap::new()), fade: eased, slide: (1.0 - eased) * 8.0 };
        draw(&mut p, page, scroll, style, surfaces, autostart);
        hits = std::mem::take(&mut p.hits);
        targets = p.targets.take();
        let _ = g.rt.EndDraw(None, None);
        let _ = g.present();
    });
    let mut moving = entering;
    with_state(|st| {
        st.hits = hits;
        for (key, target) in targets {
            st.anims.entry(key).and_modify(|a| a.1 = target).or_insert((target, target));
        }
        moving |= st.anims.values().any(|(cur, target)| (cur - target).abs() >= 0.003);
    });
    moving
}

/// The exe's own ICON resource as premultiplied BGRA pixels, for the logo.
unsafe fn icon_pixels() -> Option<(Vec<u8>, u32, u32)> {
    const SIZE: i32 = 256;
    let icon = LoadImageW(GetModuleHandleW(null()), 1 as _, IMAGE_ICON, SIZE, SIZE, 0);
    if icon.is_null() {
        return None;
    }
    let mut info: ICONINFO = std::mem::zeroed();
    if GetIconInfo(icon, &mut info) == 0 {
        DestroyIcon(icon);
        return None;
    }
    let dc = CreateCompatibleDC(null_mut());
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = SIZE;
    bi.bmiHeader.biHeight = -SIZE;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    let lines = GetDIBits(dc, info.hbmColor, 0, SIZE as u32, px.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS);
    DeleteDC(dc);
    DeleteObject(info.hbmColor);
    DeleteObject(info.hbmMask);
    DestroyIcon(icon);
    if lines == 0 {
        return None;
    }
    let pixels = px.as_chunks_mut::<4>().0;
    if pixels.iter().all(|c| c[3] == 0) {
        pixels.iter_mut().for_each(|c| c[3] = 255);
    }
    for c in pixels.iter_mut() {
        let a = c[3] as u32;
        for v in &mut c[..3] {
            *v = (*v as u32 * a / 255) as u8;
        }
    }
    Some((px, SIZE as u32, SIZE as u32))
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

/// The app icon, drawn into a `s` x `s` tile.
unsafe fn logo(p: &Pen, x: f32, y: f32, s: f32) {
    if let Some(bitmap) = &p.g.icon {
        p.g.rt.DrawBitmap(bitmap, Some(&rc((x, y, s, s))), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
    }
}

unsafe fn draw(p: &mut Pen, page: u8, scroll: f32, style: u32, surfaces: u32, autostart: bool) {
    // Both fills follow the window's rounded shape; a square fill would stick out at the corners.
    p.fill((0.0, 0.0, W, H), RADIUS, BG);
    p.g.rt.PushAxisAlignedClip(&rc((0.0, 0.0, NAV_W, H)), D2D1_ANTIALIAS_MODE_ALIASED);
    p.fill((0.0, 0.0, NAV_W + RADIUS * 2.0, H), RADIUS, NAV);
    p.g.rt.PopAxisAlignedClip();
    p.line((NAV_W, 0.0), (NAV_W, H), LINE, 1.0);

    // Page content: shifted up a little (the title sits near y 44) and by the scroll offset, clipped to the content area.
    let k = p.k;
    let base = Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 };
    p.g.alpha.set(p.fade);
    p.g.rt.SetTransform(&Matrix3x2 { M32: (-12.0 + p.slide) * k, ..base });
    let (title, hint) = match page {
        0 => (p.t("Tổng quan", "Overview"), p.t("Tình trạng hiện tại của PuwBar. Bấm một thẻ để chỉnh phần đó.", "How PuwBar is set up now. Click a card to change that part.")),
        1 => (p.t("Giao diện", "Look"), p.t("Chọn kiểu taskbar. Áp dụng ngay, không cần khởi động lại.", "Pick the taskbar style. It applies at once, no restart.")),
        2 => (p.t("Bề mặt kính", "Glass surfaces"), p.t("Chọn các cửa sổ hệ thống được phủ kính.", "Choose which system windows get the glass.")),
        3 => (p.t("Hệ thống", "System"), p.t("Cách PuwBar chạy trên máy này.", "How PuwBar runs on this PC.")),
        _ => (p.t("Giới thiệu", "About"), ""),
    };
    p.title((CX, 58.0, CW, 32.0), title);
    p.text((CX, 90.0, CW, 22.0), hint, 12.0, false, MUTED, 0);
    // Only the body scrolls, under the fixed title, so nothing slides over the close button.
    p.g.rt.SetTransform(&base);
    p.g.rt.PushAxisAlignedClip(&rc((NAV_W + 1.0, BODY_TOP, W - NAV_W, H - BODY_TOP)), D2D1_ANTIALIAS_MODE_ALIASED);
    p.dy = -12.0 - scroll;
    p.clip_top = BODY_TOP;
    p.g.rt.SetTransform(&Matrix3x2 { M32: (p.dy + p.slide) * k, ..base });
    match page {
        0 => page_overview(p, style, surfaces, autostart),
        1 => page_look(p, style),
        2 => page_surfaces(p, surfaces),
        3 => page_system(p, autostart),
        _ => page_about(p),
    }
    p.g.rt.SetTransform(&base);
    p.g.rt.PopAxisAlignedClip();
    p.g.alpha.set(1.0);
    p.dy = 0.0;
    p.clip_top = f32::MIN;

    let max = max_scroll(page);
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
        let sel = p.anim((act, 1), if page as usize == i { 1.0 } else { 0.0 });
        let hov = p.hov(act);
        let alpha = (0x0F as f32 * hov).max(0x1A as f32 * sel) as u32;
        if alpha > 0 {
            p.fill(r, 8.0, (alpha << 24) | 0x00FFFFFF);
        }
        p.icon((r.0 + 8.0, r.1, 28.0, r.3), NAV_ICONS[i], 16.0, mix(MUTED, ACCENT, sel));
        p.text((r.0 + 42.0, r.1, r.2 - 42.0, r.3), name, 13.0, false, mix(MUTED, TEXT, sel), 0);
        p.hit(r, act);
    }

    let close = (W - 44.0, 8.0, 30.0, 30.0);
    let close_hot = p.hov(Act::Close);
    if close_hot > 0.01 {
        p.fill(close, 8.0, mix(0x00FFFFFF, CARD_HOT, close_hot));
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
    let hov = p.hov(act);
    p.fill(r, 14.0, mix(CARD, CARD_HOT, hov));
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
        let hov = p.hov(act);
        if hov > 0.01 {
            p.fill((r.0 + 4.0, r.1 + 4.0, r.2 - 8.0, r.3 - 8.0), 10.0, mix(0x00FFFFFF, 0x12FFFFFF, hov));
        }
        p.text((r.0 + 18.0, r.1, r.2 - 90.0, r.3), label, 13.5, false, TEXT, 0);
        p.switch(r.0 + r.2 - 60.0, r.1 + 12.0, on, act);
        p.hit(r, act);
        if i < 2 {
            p.line((r.0 + 16.0, r.1 + r.3), (r.0 + r.2 - 16.0, r.1 + r.3), LINE, 1.0);
        }
    }
}

unsafe fn page_look(p: &mut Pen, style: u32) {
    let (cw, ch, gap) = ((CW - 12.0) / 2.0, 88.0, 12.0);
    for (i, (vi, en, dvi, den)) in STYLES.iter().enumerate() {
        let (col, row) = ((i % 2) as f32, (i / 2) as f32);
        let r = (CX + col * (cw + gap), 128.0 + row * (ch + gap), cw, ch);
        let act = Act::Style(i as u32);
        let on = style == i as u32;
        let sel = p.anim((act, 1), if on { 1.0 } else { 0.0 });
        let hov = p.hov(act);
        p.fill(r, 12.0, mix(mix(CARD, CARD_HOT, hov), ACCENT_SOFT, sel));
        p.stroke(r, 12.0, mix(LINE, ACCENT, sel), 1.0 + 0.5 * sel);
        let thumb = (r.0 + 12.0, r.1 + 12.0, 64.0, ch - 24.0);
        p.gradient(thumb, 8.0, 0xFF4A7BD6, 0xFF8A5BD0);
        pill_art(p, (thumb.0 + 5.0, thumb.1 + thumb.3 - 24.0, 54.0, 16.0), i, 3);
        let (name, desc) = if p.en { (en, den) } else { (vi, dvi) };
        p.text((r.0 + 84.0, r.1 + 13.0, cw - 92.0, 20.0), name, 13.0, true, TEXT, 0);
        p.text((r.0 + 84.0, r.1 + 37.0, cw - 92.0, 40.0), desc, 11.0, false, MUTED, 3);
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
        let hov = p.hov(act);
        p.fill(r, 12.0, mix(CARD, CARD_HOT, hov));
        p.text((r.0 + 16.0, r.1, r.2 - 90.0, r.3), name, 13.5, false, TEXT, 0);
        p.switch(r.0 + r.2 - 58.0, r.1 + 10.0, surfaces & bit != 0, act);
        p.hit(r, act);
    }
}

unsafe fn page_system(p: &mut Pen, autostart: bool) {
    let row = (CX, 128.0, CW, 56.0);
    let row_hot = p.hov(Act::Autostart);
    p.fill(row, 12.0, mix(CARD, CARD_HOT, row_hot));
    p.text((row.0 + 16.0, row.1 + 8.0, row.2 - 90.0, 20.0), p.t("Khởi động cùng Windows", "Start with Windows"), 13.5, false, TEXT, 0);
    p.text((row.0 + 16.0, row.1 + 29.0, row.2 - 90.0, 18.0), p.t("Cần quyền quản trị một lần để tạo tác vụ.", "Needs admin rights once to create the task."), 11.0, false, MUTED, 0);
    p.switch(row.0 + row.2 - 58.0, row.1 + 17.0, autostart, Act::Autostart);
    p.hit(row, Act::Autostart);

    let lang = (CX, 194.0, CW, 56.0);
    p.fill(lang, 12.0, CARD);
    p.text((lang.0 + 16.0, lang.1, 160.0, lang.3), p.t("Ngôn ngữ", "Language"), 13.5, false, TEXT, 0);
    let en = p.en;
    for (i, (label, is_en)) in [("Tiếng Việt", false), ("English", true)].iter().enumerate() {
        let r = (lang.0 + lang.2 - 232.0 + i as f32 * 112.0, lang.1 + 13.0, 104.0, 30.0);
        let act = Act::Lang(*is_en);
        let on = en == *is_en;
        let sel = p.anim((act, 1), if on { 1.0 } else { 0.0 });
        let hov = p.hov(act);
        p.fill(r, 8.0, mix(mix(CARD, CARD_HOT, hov), ACCENT_SOFT, sel));
        if sel > 0.01 {
            p.stroke(r, 8.0, mix(ACCENT & 0x00FFFFFF, ACCENT, sel), 1.2);
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
    let gh_hot = p.hov(Act::Github);
    p.fill(gh, 19.0, mix(ACCENT, 0xFF4BE07A, gh_hot));
    p.text(gh, p.t("Mở GitHub", "Open GitHub"), 13.0, true, 0xFF0B1A10, 1);
    p.hit(gh, Act::Github);
    let dn = (mid - 95.0, 456.0, 150.0, 38.0);
    let dn_hot = p.hov(Act::Donate);
    p.fill(dn, 19.0, mix(0x00FFFFFF, CARD_HOT, dn_hot));
    p.stroke(dn, 19.0, 0x66FFFFFF, 1.0);
    p.text(dn, p.t("Ủng hộ PayPal", "Donate (PayPal)"), 13.0, false, TEXT, 1);
    p.hit(dn, Act::Donate);
    let ex = (mid + 65.0, 456.0, 170.0, 38.0);
    let ex_hot = p.hov(Act::Exit);
    p.fill(ex, 19.0, mix(0x00FFFFFF, CARD_HOT, ex_hot));
    p.stroke(ex, 19.0, 0x66FFFFFF, 1.0);
    p.text(ex, p.t("Thoát PuwBar", "Exit PuwBar"), 13.0, false, TEXT, 1);
    p.hit(ex, Act::Exit);
}
