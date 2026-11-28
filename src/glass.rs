//! Frosted backdrop for the settings window (the same recipe as PUW Notch): Windows.UI.Composition blurs the sharp
//! desktop behind the window, a rounded geometric clip (anti-aliased) shapes it, and the Direct2D swap chain sits on top.

use std::cell::RefCell;

use windows::core::{implement, Interface, Result, HSTRING, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl};
use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionEffectBrush, CompositionEffectSourceParameter, CompositionStretch, Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{E_INVALIDARG, HWND};
use windows::Win32::Graphics::Direct2D::Common::D2D1_BORDER_MODE_HARD;
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE,
    D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT, IGraphicsEffectD2D1Interop,
    IGraphicsEffectD2D1Interop_Impl,
};
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT};
use windows_numerics::Vector2;

/// Gaussian blur of the sharp desktop behind the window (same value as PUW Notch).
const BLUR: f32 = 10.0;

thread_local! {
    static DISPATCHER: RefCell<Option<DispatcherQueueController>> = const { RefCell::new(None) };
}

/// Composition needs a dispatcher queue on the calling thread.
fn ensure_dispatcher() {
    DISPATCHER.with(|cell| {
        if cell.borrow().is_some() {
            return;
        }
        let opts = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        };
        // An error means the thread already owns a queue; Compositor::new validates that.
        if let Ok(ctrl) = unsafe { CreateDispatcherQueueController(opts) } {
            *cell.borrow_mut() = Some(ctrl);
        }
    });
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlurEffect {
    name: RefCell<HSTRING>,
    blur: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}

impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> Result<windows::core::GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }

    fn GetNamedPropertyMapping(&self, _name: &PCWSTR, _index: *mut u32, mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING) -> Result<()> {
        if !mapping.is_null() {
            unsafe { *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT };
        }
        Err(E_INVALIDARG.into())
    }

    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(3)
    }

    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        let val = match index {
            i if i == D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32 => PropertyValue::CreateSingle(self.blur)?,
            i if i == D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32 => PropertyValue::CreateUInt32(D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED.0 as u32)?,
            i if i == D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32 => PropertyValue::CreateUInt32(D2D1_BORDER_MODE_HARD.0 as u32)?,
            _ => return Err(E_INVALIDARG.into()),
        };
        val.cast()
    }

    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(E_INVALIDARG.into())
        }
    }

    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}

/// Keeps the composition tree alive for as long as the window shows it.
pub struct Glass {
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _root: ContainerVisual,
    _backdrop: SpriteVisual,
    _content: SpriteVisual,
}

impl Glass {
    /// `radius` is the window's corner radius in pixels; the blur is clipped to that rounded rectangle.
    pub fn new(hwnd: HWND, swap: &IDXGISwapChain1, w: i32, h: i32, radius: f32) -> Result<Glass> {
        ensure_dispatcher();
        let compositor = Compositor::new()?;
        let desktop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { desktop.CreateDesktopWindowTarget(hwnd, true)? };
        let interop: ICompositorInterop = compositor.cast()?;
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swap)? };
        let size = Vector2 { X: w as f32, Y: h as f32 };

        let root = compositor.CreateContainerVisual()?;
        root.SetSize(size)?;

        let backdrop = compositor.CreateSpriteVisual()?;
        backdrop.SetSize(size)?;
        backdrop.SetBrush(&blur_brush(&compositor)?)?;
        let shape = compositor.CreateRoundedRectangleGeometry()?;
        shape.SetSize(size)?;
        shape.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
        backdrop.SetClip(&compositor.CreateGeometricClipWithGeometry(&shape)?)?;

        let content = compositor.CreateSpriteVisual()?;
        content.SetSize(size)?;
        let brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::None)?;
        content.SetBrush(&brush)?;

        let children = root.Children()?;
        children.InsertAtTop(&backdrop)?;
        children.InsertAtTop(&content)?;
        target.SetRoot(&root)?;

        Ok(Glass { _compositor: compositor, _target: target, _root: root, _backdrop: backdrop, _content: content })
    }
}

fn blur_brush(compositor: &Compositor) -> Result<CompositionEffectBrush> {
    let source: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&HSTRING::from("Backdrop"))?.cast()?;
    let blur: IGraphicsEffect = GaussianBlurEffect { name: RefCell::new(HSTRING::from("Blur")), blur: BLUR, source }.into();
    let brush = compositor.CreateEffectFactory(&blur)?.CreateBrush()?;
    brush.SetSourceParameter(&HSTRING::from("Backdrop"), &compositor.CreateBackdropBrush()?)?;
    Ok(brush)
}
