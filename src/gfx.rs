//! Direct2D + DirectWrite drawing surface for the settings window, presented through Windows.UI.Composition.
//!
//! The window has no GDI bitmap (`WS_EX_NOREDIRECTIONBITMAP`); a premultiplied-alpha flip swap chain is the only
//! content, so a redraw swaps whole frames (no clear-then-paint flicker) and text goes through DirectWrite.

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::glass::Glass;

const PIXELS: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };

/// `0xAARRGGBB` to a Direct2D colour (straight alpha, the brush premultiplies).
pub fn color(c: u32) -> D2D1_COLOR_F {
    let f = |sh: u32| ((c >> sh) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F { r: f(16), g: f(8), b: f(0), a: f(24) }
}

pub struct Gfx {
    pub rt: ID2D1RenderTarget,
    pub dw: IDWriteFactory,
    pub brush: ID2D1SolidColorBrush,
    pub icon: Option<ID2D1Bitmap>,
    pub title_family: PCWSTR,
    pub icon_family: Option<PCWSTR>,
    swap: IDXGISwapChain1,
    // Kept alive for as long as the window shows the swap chain.
    _ctx: ID2D1DeviceContext,
    _d3d: ID3D11Device,
    _glass: Glass,
}

impl Gfx {
    /// `icon` is premultiplied BGRA pixels (width, height) for the logo, if the exe has one.
    /// `radius` is the window's corner radius in pixels.
    pub unsafe fn new(hwnd: HWND, w: i32, h: i32, radius: f32, icon: Option<(Vec<u8>, u32, u32)>) -> Result<Gfx> {
        let make = |driver: D3D_DRIVER_TYPE| -> Result<ID3D11Device> {
            let mut dev = None;
            D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None)?;
            dev.ok_or_else(|| windows::core::Error::from(E_FAIL))
        };
        let d3d = make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w as u32,
            Height: h as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForComposition(&d3d, &desc, None)?;

        let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let ctx = d2d.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        // Grayscale, not ClearType: the surface has per-pixel alpha.
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let surface: IDXGISurface = swap.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: PIXELS,
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let target_bitmap = ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
        ctx.SetTarget(&target_bitmap);
        let rt: ID2D1RenderTarget = ctx.cast()?;
        let brush = rt.CreateSolidColorBrush(&color(0xFFFFFFFF), None)?;

        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mut fonts = None;
        dw.GetSystemFontCollection(&mut fonts, false)?;
        let has = |name: PCWSTR| -> bool {
            let Some(c) = fonts.as_ref() else { return false };
            let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
            c.FindFamilyName(name, &mut index, &mut exists).is_ok() && exists.as_bool()
        };
        let title_family = if has(w!("Segoe UI Variable Display")) { w!("Segoe UI Variable Display") } else { w!("Segoe UI") };
        let icon_family = if has(w!("Segoe Fluent Icons")) {
            Some(w!("Segoe Fluent Icons"))
        } else if has(w!("Segoe MDL2 Assets")) {
            Some(w!("Segoe MDL2 Assets"))
        } else {
            None
        };

        let icon = icon.and_then(|(px, iw, ih)| {
            let bp = D2D1_BITMAP_PROPERTIES { pixelFormat: PIXELS, dpiX: 96.0, dpiY: 96.0 };
            rt.CreateBitmap(D2D_SIZE_U { width: iw, height: ih }, Some(px.as_ptr().cast()), iw * 4, &bp).ok()
        });

        let glass = Glass::new(hwnd, &swap, w, h, radius)?;

        Ok(Gfx {
            rt,
            dw,
            brush,
            icon,
            title_family,
            icon_family,
            swap,
            _ctx: ctx,
            _d3d: d3d,
            _glass: glass,
        })
    }

    pub unsafe fn set_color(&self, c: u32) {
        self.brush.SetColor(&color(c));
    }

    pub unsafe fn present(&self) -> Result<()> {
        self.swap.Present(0, DXGI_PRESENT(0)).ok()
    }
}
