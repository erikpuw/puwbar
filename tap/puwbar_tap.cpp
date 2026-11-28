#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <unknwn.h>
#include <ocidl.h>
#include <xamlom.h>
#include <d2d1_1.h>
#include <d2d1effects.h>
#include <windows.graphics.effects.interop.h>

#ifdef GetCurrentTime
#undef GetCurrentTime
#endif

#include <winrt/base.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Foundation.Numerics.h>
#include <winrt/Windows.Graphics.Effects.h>
#include <winrt/Windows.UI.h>
#include <winrt/Windows.UI.Composition.h>
#include <winrt/Windows.UI.Core.h>
#include <winrt/Windows.UI.Xaml.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.UI.Xaml.Hosting.h>
#include <winrt/Windows.UI.Xaml.Media.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

namespace xaml = winrt::Windows::UI::Xaml;
namespace controls = winrt::Windows::UI::Xaml::Controls;
namespace media = winrt::Windows::UI::Xaml::Media;
namespace comp = winrt::Windows::UI::Composition;
namespace ge = winrt::Windows::Graphics::Effects;
namespace ge_interop = ABI::Windows::Graphics::Effects;
namespace num = winrt::Windows::Foundation::Numerics;

// {7F4B2C91-3E8A-4D15-9C62-8A1F5E3B9D42}
static constexpr GUID CLSID_PuwBarTap = {
    0x7f4b2c91, 0x3e8a, 0x4d15, {0x9c, 0x62, 0x8a, 0x1f, 0x5e, 0x3b, 0x9d, 0x42}
};

static std::atomic<long> g_dllRefCount{0};
static HMODULE g_hModule = nullptr;

static constexpr uint32_t SURF_START_MENU     = 1u << 0;
static constexpr uint32_t SURF_CONTROL_CENTER = 1u << 1;
static constexpr uint32_t SURF_NOTIFICATIONS  = 1u << 2;
static constexpr uint32_t SURF_CALENDAR       = 1u << 3;
static constexpr uint32_t SURF_JUMP_LIST      = 1u << 4;
static constexpr uint32_t SURF_TOOLTIP        = 1u << 5;
static constexpr uint32_t SURF_THUMBNAIL      = 1u << 6;
static constexpr uint32_t SURF_ALL            = 0x7Fu;

static constexpr int STYLE_FROSTED_GLASS = 0; // Default frosted glass
static constexpr int STYLE_CRYSTAL_CLEAR = 1; // Transparent glass with glossy reflection
static constexpr int STYLE_ICON_REFLECT  = 2; // Completely borderless/transparent + real icon reflection
static constexpr int STYLE_FLAT_FROSTED  = 3; // v1.0.7 look: popup glass, 0.65px catch-light rim, no shadow
static constexpr int STYLE_CLEAR         = 4; // Fully transparent pills: no fill, no blur, no rim, no shadow, no reflection

struct GlassConfig {
    uint32_t surfaces = SURF_ALL;
    int style = STYLE_FROSTED_GLASS;
    bool stop = false;

    uint32_t packed() const {
        return (static_cast<uint32_t>(style) << 16) | (surfaces & SURF_ALL);
    }
};

// Reads "<hex-mask>[:<style>]" or "stop" from puwbar_dark_tint.cfg next to puwbar_tap_*.dll.
static GlassConfig ReadGlassConfig() {
    GlassConfig cfg;
    wchar_t modPath[MAX_PATH]{};
    DWORD len = GetModuleFileNameW(g_hModule, modPath, MAX_PATH);
    if (len == 0 || len >= MAX_PATH) return cfg;
    std::wstring path(modPath, len);
    size_t slash = path.find_last_of(L"\\/");
    if (slash == std::wstring::npos) return cfg;
    path.resize(slash + 1);
    path += L"puwbar_dark_tint.cfg";
    HANDLE h = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (h == INVALID_HANDLE_VALUE) return cfg;
    char buf[24]{};
    DWORD read = 0;
    bool ok = ReadFile(h, buf, sizeof(buf) - 1, &read, nullptr) != 0 && read >= 1;
    CloseHandle(h);
    if (!ok) return cfg;
    if (buf[0] == 's') {
        cfg.stop = true;
        return cfg;
    }
    char* endPtr = nullptr;
    unsigned long mask = strtoul(buf, &endPtr, 16);
    if (endPtr == buf) return cfg; // unreadable: keep defaults
    cfg.surfaces = static_cast<uint32_t>(mask) & SURF_ALL;
    if (*endPtr == ':' && endPtr[1] != '\0') {
        int parsedStyle = static_cast<int>(strtol(endPtr + 1, nullptr, 10));
        if (parsedStyle == STYLE_CRYSTAL_CLEAR || parsedStyle == STYLE_ICON_REFLECT
            || parsedStyle == STYLE_FLAT_FROSTED || parsedStyle == STYLE_CLEAR) {
            cfg.style = parsedStyle;
        } else {
            cfg.style = STYLE_FROSTED_GLASS;
        }
    }
    return cfg;
}

struct ElementMeta {
    InstanceHandle parent = 0;
    std::wstring type;
    std::wstring name;
};

struct TrackedElement {
    winrt::weak_ref<xaml::DependencyObject> target;
    std::wstring type;
    std::wstring name;
    std::wstring parentType;
};

struct SavedProp {
    winrt::weak_ref<xaml::DependencyObject> target;
    xaml::DependencyProperty prop{nullptr};
    winrt::Windows::Foundation::IInspectable original{nullptr};
    bool wasUnset = false;
};

struct SavedPropCallback {
    winrt::weak_ref<xaml::DependencyObject> target;
    xaml::DependencyProperty prop{nullptr};
    int64_t token = 0;
};

struct SavedSizeCallback {
    winrt::weak_ref<xaml::FrameworkElement> target;
    winrt::event_token token{};
};

struct SavedAcrylic {
    winrt::weak_ref<media::AcrylicBrush> brush;
    winrt::Windows::UI::Color tintColor{};
    double tintOpacity = 1.0;
    winrt::Windows::Foundation::IReference<double> tintLum{nullptr};
};

struct SavedTranslation {
    winrt::weak_ref<xaml::UIElement> el;
    num::float3 original{};
};

// Minimal D2D1 effect wrappers for always-on fast Backdrop Gaussian blur + glass tint.
struct GaussianBlurEffect : winrt::implements<GaussianBlurEffect, ge::IGraphicsEffect, ge::IGraphicsEffectSource, ge_interop::IGraphicsEffectD2D1Interop> {
    winrt::hstring m_name{L"Blur"};
    ge::IGraphicsEffectSource m_source{nullptr};
    float m_blur = 12.0f;

    winrt::hstring Name() const { return m_name; }
    void Name(winrt::hstring const& v) { m_name = v; }

    HRESULT STDMETHODCALLTYPE GetEffectId(GUID* id) noexcept override {
        if (!id) return E_POINTER;
        *id = CLSID_D2D1GaussianBlur;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetNamedPropertyMapping(LPCWSTR, UINT*, ge_interop::GRAPHICS_EFFECT_PROPERTY_MAPPING*) noexcept override {
        return E_INVALIDARG;
    }
    HRESULT STDMETHODCALLTYPE GetPropertyCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 3;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetProperty(UINT index, ABI::Windows::Foundation::IPropertyValue** value) noexcept override {
        if (!value) return E_POINTER;
        try {
            winrt::Windows::Foundation::IInspectable boxed{nullptr};
            if (index == D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION) {
                boxed = winrt::box_value(m_blur);
            } else if (index == D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION) {
                boxed = winrt::box_value(static_cast<uint32_t>(D2D1_GAUSSIANBLUR_OPTIMIZATION_SPEED));
            } else if (index == D2D1_GAUSSIANBLUR_PROP_BORDER_MODE) {
                boxed = winrt::box_value(static_cast<uint32_t>(D2D1_BORDER_MODE_HARD));
            } else {
                return E_INVALIDARG;
            }
            return winrt::get_unknown(boxed)->QueryInterface(IID_PPV_ARGS(value));
        } catch (...) {
            return E_FAIL;
        }
    }
    HRESULT STDMETHODCALLTYPE GetSource(UINT index, ge_interop::IGraphicsEffectSource** source) noexcept override {
        if (!source || index != 0 || !m_source) return E_INVALIDARG;
        return winrt::get_unknown(m_source)->QueryInterface(IID_PPV_ARGS(source));
    }
    HRESULT STDMETHODCALLTYPE GetSourceCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 1;
        return S_OK;
    }
};

// Auto contrast: turns the (blurred) backdrop into a black layer whose alpha follows the backdrop's
// luminance, alpha = slope * (luma - threshold) clamped to 0..1. Composited over the backdrop, bright
// wallpaper gets a dark veil (white text stays readable, the glass stays visible) while dark wallpaper
// is left untouched. Runs entirely in the GPU effect graph, so it follows windows behind the bar too.
struct LumaDarkenEffect : winrt::implements<LumaDarkenEffect, ge::IGraphicsEffect, ge::IGraphicsEffectSource, ge_interop::IGraphicsEffectD2D1Interop> {
    winrt::hstring m_name{L"Luma"};
    ge::IGraphicsEffectSource m_source{nullptr};
    float m_matrix[20]{};

    LumaDarkenEffect(float slope, float threshold) {
        // D2D 5x4 color matrix: row = input channel (R, G, B, A, constant), column = output channel (R, G, B, A).
        // RGB outputs stay 0 (black); only alpha is driven, from Rec.709 luma.
        m_matrix[3] = slope * 0.2126f;
        m_matrix[7] = slope * 0.7152f;
        m_matrix[11] = slope * 0.0722f;
        m_matrix[19] = -slope * threshold;
    }

    winrt::hstring Name() const { return m_name; }
    void Name(winrt::hstring const& v) { m_name = v; }

    HRESULT STDMETHODCALLTYPE GetEffectId(GUID* id) noexcept override {
        if (!id) return E_POINTER;
        *id = CLSID_D2D1ColorMatrix;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetNamedPropertyMapping(LPCWSTR, UINT*, ge_interop::GRAPHICS_EFFECT_PROPERTY_MAPPING*) noexcept override {
        return E_INVALIDARG;
    }
    HRESULT STDMETHODCALLTYPE GetPropertyCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 3;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetProperty(UINT index, ABI::Windows::Foundation::IPropertyValue** value) noexcept override {
        if (!value) return E_POINTER;
        try {
            winrt::Windows::Foundation::IInspectable boxed{nullptr};
            if (index == D2D1_COLORMATRIX_PROP_COLOR_MATRIX) {
                boxed = winrt::Windows::Foundation::PropertyValue::CreateSingleArray(m_matrix);
            } else if (index == D2D1_COLORMATRIX_PROP_ALPHA_MODE) {
                boxed = winrt::box_value(static_cast<uint32_t>(D2D1_COLORMATRIX_ALPHA_MODE_PREMULTIPLIED));
            } else if (index == D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT) {
                boxed = winrt::box_value(true);
            } else {
                return E_INVALIDARG;
            }
            return winrt::get_unknown(boxed)->QueryInterface(IID_PPV_ARGS(value));
        } catch (...) {
            return E_FAIL;
        }
    }
    HRESULT STDMETHODCALLTYPE GetSource(UINT index, ge_interop::IGraphicsEffectSource** source) noexcept override {
        if (!source || index != 0 || !m_source) return E_INVALIDARG;
        return winrt::get_unknown(m_source)->QueryInterface(IID_PPV_ARGS(source));
    }
    HRESULT STDMETHODCALLTYPE GetSourceCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 1;
        return S_OK;
    }
};

struct ColorTintEffect : winrt::implements<ColorTintEffect, ge::IGraphicsEffect, ge::IGraphicsEffectSource, ge_interop::IGraphicsEffectD2D1Interop> {
    winrt::hstring m_name{L"Tint"};
    // Premultiplied RGBA of a flat dark-smoke veil with the given alpha.
    float m_color[4];

    explicit ColorTintEffect(float alpha)
        : m_color{0.053f * alpha, 0.067f * alpha, 0.100f * alpha, alpha} {}

    winrt::hstring Name() const { return m_name; }
    void Name(winrt::hstring const& v) { m_name = v; }

    HRESULT STDMETHODCALLTYPE GetEffectId(GUID* id) noexcept override {
        if (!id) return E_POINTER;
        *id = CLSID_D2D1Flood;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetNamedPropertyMapping(LPCWSTR, UINT*, ge_interop::GRAPHICS_EFFECT_PROPERTY_MAPPING*) noexcept override {
        return E_INVALIDARG;
    }
    HRESULT STDMETHODCALLTYPE GetPropertyCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 1;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetProperty(UINT index, ABI::Windows::Foundation::IPropertyValue** value) noexcept override {
        if (!value || index != D2D1_FLOOD_PROP_COLOR) return E_INVALIDARG;
        try {
            auto boxed = winrt::Windows::Foundation::PropertyValue::CreateSingleArray(m_color);
            return winrt::get_unknown(boxed)->QueryInterface(IID_PPV_ARGS(value));
        } catch (...) {
            return E_FAIL;
        }
    }
    HRESULT STDMETHODCALLTYPE GetSource(UINT, ge_interop::IGraphicsEffectSource**) noexcept override {
        return E_INVALIDARG;
    }
    HRESULT STDMETHODCALLTYPE GetSourceCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 0;
        return S_OK;
    }
};

struct CompositeEffect : winrt::implements<CompositeEffect, ge::IGraphicsEffect, ge::IGraphicsEffectSource, ge_interop::IGraphicsEffectD2D1Interop> {
    winrt::hstring m_name{L"Composite"};
    ge::IGraphicsEffectSource m_src0{nullptr};
    ge::IGraphicsEffectSource m_src1{nullptr};

    winrt::hstring Name() const { return m_name; }
    void Name(winrt::hstring const& v) { m_name = v; }

    HRESULT STDMETHODCALLTYPE GetEffectId(GUID* id) noexcept override {
        if (!id) return E_POINTER;
        *id = CLSID_D2D1Composite;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetNamedPropertyMapping(LPCWSTR, UINT*, ge_interop::GRAPHICS_EFFECT_PROPERTY_MAPPING*) noexcept override {
        return E_INVALIDARG;
    }
    HRESULT STDMETHODCALLTYPE GetPropertyCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 1;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetProperty(UINT index, ABI::Windows::Foundation::IPropertyValue** value) noexcept override {
        if (!value || index != D2D1_COMPOSITE_PROP_MODE) return E_INVALIDARG;
        try {
            auto boxed = winrt::box_value(static_cast<uint32_t>(D2D1_COMPOSITE_MODE_SOURCE_OVER));
            return winrt::get_unknown(boxed)->QueryInterface(IID_PPV_ARGS(value));
        } catch (...) {
            return E_FAIL;
        }
    }
    HRESULT STDMETHODCALLTYPE GetSource(UINT index, ge_interop::IGraphicsEffectSource** source) noexcept override {
        if (!source) return E_POINTER;
        auto const& s = (index == 0) ? m_src0 : (index == 1 ? m_src1 : ge::IGraphicsEffectSource{nullptr});
        if (!s) return E_INVALIDARG;
        return winrt::get_unknown(s)->QueryInterface(IID_PPV_ARGS(source));
    }
    HRESULT STDMETHODCALLTYPE GetSourceCount(UINT* count) noexcept override {
        if (!count) return E_POINTER;
        *count = 2;
        return S_OK;
    }
};

// Used when the composition brush cannot be built at all.
static winrt::Windows::UI::Color FallbackGlassColor() {
    return winrt::Windows::UI::Color{110, 16, 20, 30};
}

// Blur per kind of surface, tuned by eye: the taskbar pills stay light so the wallpaper still shows through
// the thin bar, while every popup (Start, Quick Settings, Notifications, Calendar, Jump Lists, the tray
// overflow popup, tooltips) shares one stronger blur, or its large surface reads as noisy behind the text.
// Every surface shares the same auto-contrast layer on top of its blur.
static constexpr float kPillBlur = 6.0f;
static constexpr float kPopupBlur = 12.0f;

// What sits on top of the blurred, auto-contrast backdrop:
//   Plain: nothing (popups, so they match the pills' see-through feel),
//   Lit:   the shared surface lighting (the Frosted pill),
//   Smoke: a fixed dark-smoke veil (the Flat Frosted pill, the v1.0.7 look).
enum class GlassLook { Plain, Lit, Smoke };
static constexpr float kSmokeAlpha = 0.16f;

// Surface lighting shared by Crystal Clear (as its whole background) and the Frosted pill (as a layer
// over the blurred backdrop), so both read as the same glass slab: a soft sheen right under the top
// edge that fades out, a ~4% neutral dim in the core so icons keep contrast on bright wallpapers,
// and a faint bounce along the bottom.
struct GlassSurfaceStop {
    double offset;
    uint8_t alpha;
    uint8_t rgb;
};
static constexpr GlassSurfaceStop kGlassSurface[] = {
    {0.00, 56, 255}, // ~22% sheen right under the top edge
    {0.22, 20, 255}, // ~8% sheen fading softly
    {0.55, 10, 0},   // ~4% neutral dim in the clear core
    {0.85, 8, 0},    // ~3% dim
    {1.00, 20, 255}, // ~8% faint bottom bounce
};

struct PuwGlassBrush : media::XamlCompositionBrushBaseT<PuwGlassBrush> {
    comp::Compositor m_compositor{nullptr};
    GlassLook m_look = GlassLook::Plain;

    PuwGlassBrush(comp::Compositor compositor, GlassLook look)
        : m_compositor(std::move(compositor)), m_look(look) {
        FallbackColor(FallbackGlassColor());
    }

    comp::CompositionLinearGradientBrush MakeSheen() const {
        auto g = m_compositor.CreateLinearGradientBrush();
        g.StartPoint(num::float2{0.5f, 0.0f});
        g.EndPoint(num::float2{0.5f, 1.0f});
        // Over a blurred backdrop the lighting reads stronger than over a sharp one, so it is eased.
        constexpr float kBlurredScale = 0.6f;
        for (auto const& s : kGlassSurface) {
            auto a = static_cast<uint8_t>(s.alpha * kBlurredScale + 0.5f);
            g.ColorStops().Append(m_compositor.CreateColorGradientStop(
                static_cast<float>(s.offset), winrt::Windows::UI::Color{a, s.rgb, s.rgb, s.rgb}));
        }
        return g;
    }

    // Graph: blur -> [auto contrast darkening] -> [sheen | smoke veil]. Effect node names must be
    // unique, so the luma branch has its own blur node (both read the same "Backdrop" source).
    comp::CompositionEffectBrush BuildBrush(bool adaptive) const {
        float const blurAmount = (m_look != GlassLook::Plain) ? kPillBlur : kPopupBlur;
        auto blur = winrt::make_self<GaussianBlurEffect>();
        blur->m_source = comp::CompositionEffectSourceParameter(L"Backdrop");
        blur->m_blur = blurAmount;
        ge::IGraphicsEffectSource base = blur.as<ge::IGraphicsEffectSource>();
        if (adaptive) {
            auto lumaBlur = winrt::make_self<GaussianBlurEffect>();
            lumaBlur->m_name = L"LumaBlur";
            lumaBlur->m_source = comp::CompositionEffectSourceParameter(L"Backdrop");
            lumaBlur->m_blur = blurAmount;
            // Only the bright half is darkened: 0 below ~50% luma, ~14% black at 60%, ~35% at 75%, ~70% at pure
            // white. Everything below the threshold keeps its full contrast so the wallpaper stays visible.
            auto luma = winrt::make_self<LumaDarkenEffect>(1.4f, 0.5f);
            luma->m_source = lumaBlur.as<ge::IGraphicsEffectSource>();
            auto adapt = winrt::make_self<CompositeEffect>();
            adapt->m_name = L"Adaptive";
            adapt->m_src0 = base;
            adapt->m_src1 = luma.as<ge::IGraphicsEffectSource>();
            base = adapt.as<ge::IGraphicsEffectSource>();
        }
        ge::IGraphicsEffectSource root = base;
        if (m_look != GlassLook::Plain) {
            auto composite = winrt::make_self<CompositeEffect>();
            composite->m_src0 = base;
            if (m_look == GlassLook::Lit) {
                composite->m_src1 = comp::CompositionEffectSourceParameter(L"Sheen");
            } else {
                composite->m_src1 = winrt::make_self<ColorTintEffect>(kSmokeAlpha).as<ge::IGraphicsEffectSource>();
            }
            root = composite.as<ge::IGraphicsEffectSource>();
        }

        auto effectBrush = m_compositor.CreateEffectFactory(root.as<ge::IGraphicsEffect>()).CreateBrush();
        effectBrush.SetSourceParameter(L"Backdrop", m_compositor.CreateBackdropBrush());
        if (m_look == GlassLook::Lit) {
            effectBrush.SetSourceParameter(L"Sheen", MakeSheen());
        }
        return effectBrush;
    }

    void OnConnected() {
        if (CompositionBrush() || !m_compositor) return;
        // If the auto-contrast graph is rejected, fall back to the same glass without it, never to nothing.
        for (bool adaptive : {true, false}) {
            try {
                CompositionBrush(BuildBrush(adaptive));
                return;
            } catch (...) {
            }
        }
    }

    void OnDisconnected() {
        // Keep CompositionBrush alive during XAML popup slide-out/dismiss transitions
        // (e.g. Jump List close animation); WinRT releases it automatically on destruction.
    }
};

static media::Brush MakePuwGlass(xaml::DependencyObject const& obj, GlassLook look = GlassLook::Plain) {
    try {
        if (auto el = obj.try_as<xaml::UIElement>()) {
            auto visual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(el);
            if (visual) {
                if (auto compositor = visual.Compositor()) {
                    return winrt::make<PuwGlassBrush>(compositor, look);
                }
            }
        }
    } catch (...) {
    }
    return media::SolidColorBrush(FallbackGlassColor());
}

// Average brightness (0..1) of the part of the screen the context menu opens over, from a tiny downscale of
// the composed desktop. The menu opens above the mouse pointer (it was right-clicked there), so the area is
// a ~500px wide, ~620px tall block just above the pointer; without a pointer it falls back to the strip above
// the taskbar. Native acrylic cannot take the glass's per-pixel auto-contrast layer, so the context menus use
// this single measured value for the same purpose.
static float SampleMenuBackdropLuma() {
    constexpr int kW = 24;
    constexpr int kH = 8;
    float luma = 0.5f;
    int const sw = GetSystemMetrics(SM_CXSCREEN);
    int const sh = GetSystemMetrics(SM_CYSCREEN);
    int x0 = 0;
    int x1 = sw;
    int y0 = sh * 55 / 100;
    int y1 = sh * 95 / 100;
    POINT pt{};
    if (GetCursorPos(&pt)) {
        x0 = std::clamp<int>(pt.x - 250, 0, sw - 1);
        x1 = std::clamp<int>(pt.x + 250, x0 + 1, sw);
        y0 = std::clamp<int>(pt.y - 640, 0, sh - 1);
        y1 = std::clamp<int>(pt.y - 20, y0 + 1, sh);
    }
    HDC screen = GetDC(nullptr);
    if (!screen) return luma;
    HDC mem = CreateCompatibleDC(screen);
    BITMAPINFO bi{};
    bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = kW;
    bi.bmiHeader.biHeight = -kH;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    void* bits = nullptr;
    HBITMAP bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
    if (mem && bmp && bits) {
        HGDIOBJ old = SelectObject(mem, bmp);
        SetStretchBltMode(mem, HALFTONE);
        SetBrushOrgEx(mem, 0, 0, nullptr);
        if (StretchBlt(mem, 0, 0, kW, kH, screen, x0, y0, x1 - x0, y1 - y0, SRCCOPY)) {
            auto const* px = static_cast<uint32_t const*>(bits);
            double sum = 0.0;
            for (int i = 0; i < kW * kH; ++i) {
                uint32_t p = px[i];
                sum += 0.2126 * ((p >> 16) & 0xFF) + 0.7152 * ((p >> 8) & 0xFF) + 0.0722 * (p & 0xFF);
            }
            luma = static_cast<float>(sum / (kW * kH) / 255.0);
        }
        SelectObject(mem, old);
    }
    if (bmp) DeleteObject(bmp);
    if (mem) DeleteDC(mem);
    ReleaseDC(nullptr, screen);
    return luma;
}

static media::SolidColorBrush MakePuwBorder() {
    // Delicate 15% white hairline glass edge for popups
    return media::SolidColorBrush(winrt::Windows::UI::Color{38, 255, 255, 255});
}

static media::LinearGradientBrush MakeGlassSlabRim(float strength = 1.0f) {
    // Glass thickness cue shared by Frosted and Crystal Clear pills: bright catch-light on top,
    // the side walls turn dark (light passing through the slab edge), then a bottom bounce.
    // `strength` scales every alpha: the blurred Frosted pill reads brighter than the clear one, so its rim is softer.
    media::LinearGradientBrush rim;
    rim.StartPoint(winrt::Windows::Foundation::Point{0.5f, 0.0f});
    rim.EndPoint(winrt::Windows::Foundation::Point{0.5f, 1.0f});

    auto addStop = [&](double offset, uint8_t a, uint8_t rgb) {
        media::GradientStop stop;
        stop.Offset(offset);
        stop.Color(winrt::Windows::UI::Color{static_cast<uint8_t>(a * strength + 0.5f), rgb, rgb, rgb});
        rim.GradientStops().Append(stop);
    };
    addStop(0.00, 150, 255); // ~59% top catch-light
    addStop(0.12, 46, 255);  // ~18% shoulder
    addStop(0.40, 36, 0);    // ~14% dark side wall
    addStop(0.75, 28, 0);    // ~11% dark side wall
    addStop(0.92, 40, 255);  // ~16% lower curve
    addStop(1.00, 90, 255);  // ~35% bottom bounce
    return rim;
}

static media::LinearGradientBrush MakeCrystalClearBackground() {
    // Zero blur: the clear glass is just the shared surface lighting over the untouched backdrop.
    media::LinearGradientBrush bg;
    bg.StartPoint(winrt::Windows::Foundation::Point{0.5f, 0.0f});
    bg.EndPoint(winrt::Windows::Foundation::Point{0.5f, 1.0f});
    for (auto const& s : kGlassSurface) {
        media::GradientStop stop;
        stop.Offset(s.offset);
        stop.Color(winrt::Windows::UI::Color{s.alpha, s.rgb, s.rgb, s.rgb});
        bg.GradientStops().Append(stop);
    }
    return bg;
}

static media::Brush MakeTaskbarPillBackground(xaml::DependencyObject const& obj, int style) {
    if (style == STYLE_ICON_REFLECT || style == STYLE_CLEAR) {
        return media::SolidColorBrush(winrt::Windows::UI::Color{0, 0, 0, 0});
    }
    if (style == STYLE_CRYSTAL_CLEAR) {
        return MakeCrystalClearBackground();
    }
    if (style == STYLE_FLAT_FROSTED) {
        return MakePuwGlass(obj, GlassLook::Smoke);
    }
    // Frosted pill: the shared light blur (the macOS dock glass stays quite see-through) plus the
    // shared surface lighting so it reads as a lit glass slab.
    return MakePuwGlass(obj, GlassLook::Lit);
}

static media::LinearGradientBrush MakeHairlineCatchRim() {
    // v1.0.7 hairline catch-light: ~28% top edge, ~10% sides, ~20% bottom bounce, all white.
    media::LinearGradientBrush rim;
    rim.StartPoint(winrt::Windows::Foundation::Point{0.5f, 0.0f});
    rim.EndPoint(winrt::Windows::Foundation::Point{0.5f, 1.0f});

    auto addStop = [&](double offset, uint8_t a) {
        media::GradientStop stop;
        stop.Offset(offset);
        stop.Color(winrt::Windows::UI::Color{a, 255, 255, 255});
        rim.GradientStops().Append(stop);
    };
    addStop(0.00, 71); // ~28% top edge
    addStop(0.18, 30); // ~12% shoulder
    addStop(0.55, 26); // ~10% sides
    addStop(0.88, 33); // ~13% lower curve
    addStop(1.00, 51); // ~20% bottom bounce
    return rim;
}

static media::Brush MakeTaskbarPillBorder(int style) {
    if (style == STYLE_ICON_REFLECT || style == STYLE_CLEAR) {
        return media::SolidColorBrush(winrt::Windows::UI::Color{0, 0, 0, 0});
    }
    if (style == STYLE_FLAT_FROSTED) {
        return MakeHairlineCatchRim();
    }
    return MakeGlassSlabRim(style == STYLE_CRYSTAL_CLEAR ? 1.0f : 0.6f);
}

static xaml::Thickness TaskbarPillBorderThickness(int style) {
    if (style == STYLE_ICON_REFLECT || style == STYLE_CLEAR) {
        return xaml::Thickness{0.0, 0.0, 0.0, 0.0};
    }
    double t = (style == STYLE_FLAT_FROSTED) ? 0.65 : 1.0;
    return xaml::Thickness{t, t, t, t};
}

static float TaskbarPillShadowZ(int style) {
    // Keep elevation low: ThemeShadow blur scales with Z and gets clipped by the taskbar
    // XAML island bounds, leaving a hard horizontal cut line at the taskbar edge.
    if (style == STYLE_ICON_REFLECT || style == STYLE_FLAT_FROSTED || style == STYLE_CLEAR) return 0.0f;
    return (style == STYLE_CRYSTAL_CLEAR) ? 4.0f : 3.0f;
}

// Reads Windows 11 Taskbar alignment setting: 0 = Left/Top, 1 (default) = Center.
static bool IsTaskbarAlignedStart() {
    DWORD val = 1;
    DWORD size = sizeof(val);
    if (RegGetValueW(
            HKEY_CURRENT_USER,
            L"Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced",
            L"TaskbarAl",
            RRF_RT_REG_DWORD,
            nullptr,
            &val,
            &size) == ERROR_SUCCESS) {
        return val == 0;
    }
    return false;
}

enum class TaskbarEdge { Bottom, Top, Left, Right };

static TaskbarEdge GetTaskbarEdge() {
    HWND tray = FindWindowW(L"Shell_TrayWnd", nullptr);
    if (!tray) return TaskbarEdge::Bottom;
    RECT rc{};
    if (!GetWindowRect(tray, &rc)) return TaskbarEdge::Bottom;
    int w = rc.right - rc.left;
    int h = rc.bottom - rc.top;
    HMONITOR mon = MonitorFromWindow(tray, MONITOR_DEFAULTTONEAREST);
    MONITORINFO mi{};
    mi.cbSize = sizeof(mi);
    if (mon && GetMonitorInfoW(mon, &mi)) {
        if (h > w) {
            int midX = (mi.rcMonitor.left + mi.rcMonitor.right) / 2;
            return ((rc.left + rc.right) / 2 < midX) ? TaskbarEdge::Left : TaskbarEdge::Right;
        }
        int midY = (mi.rcMonitor.top + mi.rcMonitor.bottom) / 2;
        return ((rc.top + rc.bottom) / 2 < midY) ? TaskbarEdge::Top : TaskbarEdge::Bottom;
    }
    return (h > w) ? TaskbarEdge::Left : TaskbarEdge::Bottom;
}

// Windows makes the Start button (LaunchListButton) wider than the other buttons when a horizontal taskbar is
// start-aligned: trim the left overhang so both pill ends match. A vertical taskbar needs no margin: the button is
// only 1px taller there, and a negative margin makes it taller still (measured 55px instead of 45px).
static xaml::Thickness LaunchButtonMargin() {
    TaskbarEdge edge = GetTaskbarEdge();
    if (edge == TaskbarEdge::Left || edge == TaskbarEdge::Right) {
        return xaml::Thickness{0.0, 0.0, 0.0, 0.0};
    }
    return xaml::Thickness{IsTaskbarAlignedStart() ? -7.0 : 0.0, 0.0, 0.0, 0.0};
}

// Height of the system tray block (TrayNotifyWnd) in XAML pixels. On a vertical taskbar it sits at the bottom, and the
// app group must stay clear of it: the group is centred in the whole window otherwise and overlaps the tray.
static double TrayReservedHeight() {
    HWND tray = FindWindowW(L"Shell_TrayWnd", nullptr);
    HWND notify = tray ? FindWindowExW(tray, nullptr, L"TrayNotifyWnd", nullptr) : nullptr;
    RECT rc{};
    if (!notify || !GetWindowRect(notify, &rc)) return 0.0;
    double scale = static_cast<double>(GetDpiForWindow(tray)) / 96.0;
    return scale > 0.0 ? static_cast<double>(rc.bottom - rc.top) / scale : 0.0;
}

// Smooth GPU implicit animation for horizontal position (Offset only — no Size lag so no empty ghost slot appears).
static void EnableSmoothShellSlide(xaml::UIElement const& el) {
    try {
        auto visual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(el);
        if (!visual) return;
        auto compositor = visual.Compositor();
        if (!compositor) return;

        // Balanced ease-in-out curve (~300ms) timed to match Windows 11's native app icon slide-up animation.
        auto ease = compositor.CreateCubicBezierEasingFunction(
            num::float2{0.35f, 0.0f}, num::float2{0.20f, 1.0f});

        auto offsetAnim = compositor.CreateVector3KeyFrameAnimation();
        offsetAnim.Target(L"Offset");
        offsetAnim.InsertExpressionKeyFrame(1.0f, L"this.FinalValue", ease);
        offsetAnim.Duration(std::chrono::milliseconds(300));

        auto implicit = compositor.CreateImplicitAnimationCollection();
        implicit.Insert(L"Offset", offsetAnim);
        visual.ImplicitAnimations(implicit);
    } catch (...) {
    }
}

class PuwBarTap : public IObjectWithSite, public IVisualTreeServiceCallback2 {
public:
    PuwBarTap() {
        g_dllRefCount.fetch_add(1, std::memory_order_relaxed);
    }

    ~PuwBarTap() {
        StopWatcher();
        g_dllRefCount.fetch_sub(1, std::memory_order_relaxed);
    }

    // IUnknown
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IObjectWithSite)) {
            *ppv = static_cast<IObjectWithSite*>(this);
        } else if (riid == __uuidof(IVisualTreeServiceCallback) || riid == __uuidof(IVisualTreeServiceCallback2)) {
            *ppv = static_cast<IVisualTreeServiceCallback2*>(this);
        } else {
            return E_NOINTERFACE;
        }
        AddRef();
        return S_OK;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(m_ref.fetch_add(1, std::memory_order_relaxed) + 1);
    }

    ULONG STDMETHODCALLTYPE Release() override {
        long count = m_ref.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (count == 0) {
            delete this;
        }
        return static_cast<ULONG>(count);
    }

    // IObjectWithSite
    HRESULT STDMETHODCALLTYPE SetSite(IUnknown* pUnkSite) override {
        IVisualTreeService3* vtsToAdvise = nullptr;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            if (!pUnkSite) {
                DetachLocked();
                return S_OK;
            }
            DetachLocked();
            m_site = pUnkSite;
            m_site->AddRef();

            pUnkSite->QueryInterface(__uuidof(IXamlDiagnostics), reinterpret_cast<void**>(&m_diag));
            pUnkSite->QueryInterface(__uuidof(IVisualTreeService3), reinterpret_cast<void**>(&m_vts));
            if (m_vts) {
                vtsToAdvise = m_vts;
                vtsToAdvise->AddRef();
            }
        }
        if (vtsToAdvise) {
            HRESULT hr = vtsToAdvise->AdviseVisualTreeChange(this);
            vtsToAdvise->Release();
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_advised = SUCCEEDED(hr);
            }
        }

        if (HANDLE ready = OpenEventW(EVENT_MODIFY_STATE, FALSE, L"Local\\PuwBar.TapReady")) {
            SetEvent(ready);
            CloseHandle(ready);
        }

        StartWatcher();
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE GetSite(REFIID riid, void** ppvSite) override {
        std::lock_guard<std::recursive_mutex> lock(m_mutex);
        if (!ppvSite) return E_POINTER;
        *ppvSite = nullptr;
        if (!m_site) return E_FAIL;
        return m_site->QueryInterface(riid, ppvSite);
    }

    // IVisualTreeServiceCallback
    HRESULT STDMETHODCALLTYPE OnVisualTreeChange(
        ParentChildRelation relation,
        VisualElement element,
        VisualMutationType mutationType) override {
        try {
            if (mutationType == Remove) {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_meta.erase(element.Handle);
                return S_OK;
            }
            if (mutationType != Add) {
                return S_OK;
            }

            ElementMeta meta;
            meta.parent = relation.Parent;
            if (element.Type) meta.type = element.Type;
            if (element.Name) meta.name = element.Name;

            std::wstring parentType;
            bool inMainTaskbarBackground = false;
            IXamlDiagnostics* diag = nullptr;
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_meta[element.Handle] = meta;
                auto it = m_meta.find(relation.Parent);
                if (it != m_meta.end()) {
                    parentType = it->second.type;
                }
                inMainTaskbarBackground = HasAncestorNameLocked(relation.Parent, L"BackgroundControl");
                diag = m_diag;
                if (diag) diag->AddRef();
            }

            if (!diag) return S_OK;

            IInspectable* raw = nullptr;
            HRESULT hr = diag->GetIInspectableFromHandle(element.Handle, &raw);
            diag->Release();
            if (FAILED(hr) || !raw) return S_OK;

            winrt::Windows::Foundation::IInspectable insp{raw, winrt::take_ownership_from_abi};
            if (auto depObj = insp.try_as<xaml::DependencyObject>()) {
                CaptureDispatcher(depObj);
                std::wstring effectiveParent = inMainTaskbarBackground ? L"Taskbar.BackgroundControl" : parentType;
                if (IsInteresting(meta.type, meta.name, effectiveParent)) {
                    {
                        std::lock_guard<std::recursive_mutex> lock(m_mutex);
                        m_tracked.push_back(TrackedElement{
                            winrt::make_weak(depObj),
                            meta.type,
                            meta.name,
                            effectiveParent,
                        });
                    }
                    auto dispatcher = depObj.Dispatcher();
                    auto typeCopy = meta.type;
                    auto nameCopy = meta.name;
                    auto parentTypeCopy = effectiveParent;
                    AddRef();
                    if (dispatcher && !dispatcher.HasThreadAccess()) {
                        dispatcher.RunAsync(
                            winrt::Windows::UI::Core::CoreDispatcherPriority::High,
                            [this, depObj, typeCopy, nameCopy, parentTypeCopy]() {
                                ApplyStyleIfMatch(depObj, typeCopy, nameCopy, parentTypeCopy);
                                Release();
                            });
                    } else {
                        ApplyStyleIfMatch(depObj, typeCopy, nameCopy, parentTypeCopy);
                        Release();
                    }
                }
            }
        } catch (...) {
        }
        return S_OK;
    }

    // IVisualTreeServiceCallback2
    HRESULT STDMETHODCALLTYPE OnElementStateChanged(
        InstanceHandle,
        VisualElementState,
        LPCWSTR) override {
        return S_OK;
    }

private:
    void CaptureDispatcher(xaml::DependencyObject const& obj) {
        try {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            if (!m_dispatcher) {
                m_dispatcher = obj.Dispatcher();
            }
        } catch (...) {
        }
    }

    bool HasAncestorNameLocked(InstanceHandle h, std::wstring const& targetName) const {
        InstanceHandle cur = h;
        for (int depth = 0; depth < 16 && cur != 0; ++depth) {
            auto it = m_meta.find(cur);
            if (it == m_meta.end()) break;
            if (it->second.name == targetName) return true;
            cur = it->second.parent;
        }
        return false;
    }

    static bool IsInteresting(
        std::wstring const& type,
        std::wstring const& name,
        std::wstring const& parentType) {
        return type == L"Taskbar.TaskbarFrame"
            || (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame")
            || (type == L"Taskbar.ExperienceToggleButton" && name == L"LaunchListButton")
            || (type == L"Taskbar.TaskbarBackground" && name == L"BackgroundControl")
            || ((name == L"BackgroundFill" || name == L"BackgroundStroke")
                && parentType == L"Taskbar.BackgroundControl")
            || name == L"HoverFlyoutBackgroundControl"
            || name == L"HoverFlyoutBackground"
            || name == L"SystemTrayFrameGrid"
            || name == L"OverflowFlyoutBackgroundBorder"
            || type == L"Windows.UI.Xaml.Controls.FlyoutPresenter"
            || type == L"Windows.UI.Xaml.Controls.MenuFlyoutPresenter"
            || type == L"Windows.UI.Xaml.Controls.ToolTip"
            || name == L"NotificationCenterGrid"
            || name == L"CalendarCenterGrid"
            || name == L"ControlCenterRegion"
            || name == L"MediaTransportControlsRegion"
            || name == L"JumpListRestyledAcrylic"
            || name == L"SystemItemsAcrylic"
            || name == L"JumpListAcrylic"
            || name == L"AcrylicBorder"
            || name == L"RunningIndicator"
            || (name == L"IconPanel" && parentType == L"Taskbar.TaskListButton")
            || (name == L"ExperienceToggleButtonRootPanel"
                && parentType == L"Taskbar.AugmentedEntryPointButton");
    }

    void SaveAndSet(
        xaml::DependencyObject const& obj,
        xaml::DependencyProperty const& prop,
        winrt::Windows::Foundation::IInspectable const& value) {
        try {
            auto local = obj.ReadLocalValue(prop);
            SavedProp saved;
            saved.target = winrt::make_weak(obj);
            saved.prop = prop;
            saved.wasUnset = (local == xaml::DependencyProperty::UnsetValue());
            if (!saved.wasUnset) {
                saved.original = local;
            }
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_saved.push_back(std::move(saved));
            }
            obj.SetValue(prop, value);
        } catch (...) {
        }
    }

    void StyleNativeAcrylicInPlace(media::AcrylicBrush const& ac) {
        try {
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_savedAcrylics.push_back(SavedAcrylic{
                    winrt::make_weak(ac),
                    ac.TintColor(),
                    ac.TintOpacity(),
                    ac.TintLuminosityOpacity(),
                });
            }
            ac.AlwaysUseFallback(false);
            // Same idea as the glass's auto contrast, with one value measured from the desktop: a neutral dark
            // veil that stays light on dark wallpapers and gets denser as the wallpaper gets brighter. A neutral
            // tint and a low luminosity opacity keep it from turning the wallpaper's hue into a dark brown/blue.
            float const luma = SampleMenuBackdropLuma();
            // Same threshold and slope as the glass's luma layer (LumaDarkenEffect in BuildBrush), on top of a
            // light base veil.
            float const over = luma > 0.5f ? luma - 0.5f : 0.0f;
            double const veil = std::clamp(0.12 + 1.4 * over, 0.12, 0.70);
            ac.TintColor(winrt::Windows::UI::Color{255, 12, 14, 18});
            ac.TintOpacity(veil);
            ac.TintLuminosityOpacity(0.10);
        } catch (...) {
        }
    }

    void LockGlassBackground(xaml::DependencyObject const& obj, xaml::DependencyProperty const& prop) {
        try {
            auto token = obj.RegisterPropertyChangedCallback(
                prop,
                [](xaml::DependencyObject const& sender, xaml::DependencyProperty const& dp) {
                    try {
                        auto cur = sender.GetValue(dp);
                        if (!cur || cur.try_as<media::AcrylicBrush>() || cur.try_as<media::SolidColorBrush>()) {
                            sender.SetValue(dp, MakePuwGlass(sender));
                        }
                    } catch (...) {
                    }
                });
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            m_propCallbacks.push_back({winrt::make_weak(obj), prop, token});
        } catch (...) {
        }
    }

    void ApplyLayoutProps(
        xaml::DependencyObject const& obj,
        std::wstring const& type,
        std::wstring const& name,
        std::wstring const& parentType,
        bool save) {
        TaskbarEdge edge = GetTaskbarEdge();
        bool vertical = (edge == TaskbarEdge::Left || edge == TaskbarEdge::Right);
        bool startAligned = IsTaskbarAlignedStart();

        auto setProp = [&](xaml::DependencyProperty const& prop, winrt::Windows::Foundation::IInspectable const& val) {
            if (save) {
                SaveAndSet(obj, prop, val);
            } else {
                try { obj.SetValue(prop, val); } catch (...) {}
            }
        };

        if (type == L"Taskbar.TaskbarFrame") {
            if (vertical) {
                // Vertical taskbar: keep Windows' native Width (~48px), only auto-size Height.
                setProp(xaml::FrameworkElement::HeightProperty(), winrt::box_value(std::numeric_limits<double>::quiet_NaN()));
                setProp(xaml::FrameworkElement::HorizontalAlignmentProperty(), winrt::box_value(xaml::HorizontalAlignment::Stretch));
                setProp(
                    xaml::FrameworkElement::VerticalAlignmentProperty(),
                    winrt::box_value(startAligned ? xaml::VerticalAlignment::Top : xaml::VerticalAlignment::Center));
                setProp(
                    xaml::FrameworkElement::MarginProperty(),
                    winrt::box_value(xaml::Thickness{0.0, 4.0, 0.0, 4.0 + TrayReservedHeight()}));
            } else {
                // Horizontal taskbar: keep Windows' native Height (~48px), only auto-size Width.
                setProp(xaml::FrameworkElement::WidthProperty(), winrt::box_value(std::numeric_limits<double>::quiet_NaN()));
                setProp(
                    xaml::FrameworkElement::HorizontalAlignmentProperty(),
                    winrt::box_value(startAligned ? xaml::HorizontalAlignment::Left : xaml::HorizontalAlignment::Center));
                setProp(xaml::FrameworkElement::VerticalAlignmentProperty(), winrt::box_value(xaml::VerticalAlignment::Stretch));
                setProp(controls::Control::PaddingProperty(), winrt::box_value(xaml::Thickness{0.0, 0.0, 0.0, 0.0}));
                setProp(
                    xaml::FrameworkElement::MarginProperty(),
                    winrt::box_value(startAligned ? xaml::Thickness{8.0, 0.0, 250.0, 0.0} : xaml::Thickness{250.0, 0.0, 250.0, 0.0}));
            }
            return;
        }

        if (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame") {
            if (vertical) {
                // Symmetric horizontal margin (2px each side) so icons stay perfectly centered inside the vertical pill.
                setProp(controls::Grid::PaddingProperty(), winrt::box_value(xaml::Thickness{0.0, 4.0, 0.0, 4.0}));
                setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(xaml::Thickness{2.0, 0.0, 2.0, 0.0}));
            } else {
                // When left-aligned, Windows widens LaunchListButton (45px vs 40px); trim left padding to keep both ends visually symmetric.
                double leftPad = startAligned ? 0.0 : 3.0;
                setProp(controls::Grid::PaddingProperty(), winrt::box_value(xaml::Thickness{leftPad, 0.0, 3.0, 0.0}));
                bool reflectMode = (ReadGlassConfig().style == STYLE_ICON_REFLECT) && (edge == TaskbarEdge::Bottom);
                xaml::Thickness rootMargin = reflectMode
                    ? xaml::Thickness{0.0, 0.0, 0.0, 6.0}
                    : (edge == TaskbarEdge::Top ? xaml::Thickness{0.0, 4.0, 0.0, 2.0} : xaml::Thickness{0.0, 2.0, 0.0, 4.0});
                setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(rootMargin));
            }
            return;
        }

        if (type == L"Taskbar.ExperienceToggleButton" && name == L"LaunchListButton") {
            setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(LaunchButtonMargin()));
            return;
        }

        if (name == L"SystemTrayFrameGrid") {
            bool reflectMode = (ReadGlassConfig().style == STYLE_ICON_REFLECT) && (edge == TaskbarEdge::Bottom);
            xaml::Thickness margin = vertical
                ? xaml::Thickness{2.0, 0.0, 2.0, 8.0}
                : (reflectMode
                    ? xaml::Thickness{0.0, 0.0, 8.0, 6.0}
                    : (edge == TaskbarEdge::Top ? xaml::Thickness{0.0, 4.0, 8.0, 2.0} : xaml::Thickness{0.0, 2.0, 8.0, 4.0}));
            xaml::Thickness pad = vertical
                ? xaml::Thickness{0.0, 6.0, 0.0, 6.0}
                : xaml::Thickness{6.0, 0.0, 6.0, 0.0};
            setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(margin));
            if (obj.try_as<controls::StackPanel>()) {
                setProp(controls::StackPanel::PaddingProperty(), winrt::box_value(pad));
            } else if (obj.try_as<controls::Grid>()) {
                setProp(controls::Grid::PaddingProperty(), winrt::box_value(pad));
            }
            return;
        }

        if (name == L"IconPanel" && parentType == L"Taskbar.TaskListButton") {
            // A vertical taskbar's button slot is 42px but the stock icon panel is 48px wide, so the icons spill
            // 6px to one side and sit off-centre next to the Start button. Cap the panel at the slot width.
            setProp(
                xaml::FrameworkElement::MaxWidthProperty(),
                winrt::box_value(vertical ? 42.0 : std::numeric_limits<double>::infinity()));
            return;
        }

        if (name == L"RunningIndicator") {
            if (edge == TaskbarEdge::Right) {
                setProp(xaml::FrameworkElement::HorizontalAlignmentProperty(), winrt::box_value(xaml::HorizontalAlignment::Right));
            } else if (edge == TaskbarEdge::Left) {
                setProp(xaml::FrameworkElement::HorizontalAlignmentProperty(), winrt::box_value(xaml::HorizontalAlignment::Left));
            } else {
                setProp(xaml::FrameworkElement::HorizontalAlignmentProperty(), winrt::box_value(xaml::HorizontalAlignment::Center));
            }
            return;
        }

        if (name == L"ExperienceToggleButtonRootPanel"
            && parentType == L"Taskbar.AugmentedEntryPointButton") {
            bool reflectMode = (ReadGlassConfig().style == STYLE_ICON_REFLECT) && (edge == TaskbarEdge::Bottom);
            xaml::Thickness margin = vertical
                ? (edge == TaskbarEdge::Right ? xaml::Thickness{2.0, 8.0, 4.0, 0.0} : xaml::Thickness{4.0, 8.0, 2.0, 0.0})
                : (reflectMode
                    ? xaml::Thickness{8.0, 0.0, 0.0, 6.0}
                    : (edge == TaskbarEdge::Top ? xaml::Thickness{8.0, 4.0, 0.0, 2.0} : xaml::Thickness{8.0, 2.0, 0.0, 4.0}));
            xaml::Thickness pad = vertical
                ? xaml::Thickness{0.0, 4.0, 0.0, 4.0}
                : xaml::Thickness{6.0, 0.0, 6.0, 0.0};
            setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(margin));
            if (obj.try_as<controls::Grid>()) {
                setProp(controls::Grid::PaddingProperty(), winrt::box_value(pad));
            }
        }
    }

    struct ClockSpanInfo {
        bool found = false;
        float left = 1e9f;
        float right = -1.0f;
        float dateTop = 0.0f;
        float dateHeight = 0.0f;
    };

    // Finds exact bounds of TimeInnerTextBlock & DateInnerTextBlock (verified from Windows 11 SystemTray.dll,
    // nested ~16-18 levels deep inside SystemTrayFrameGrid -> OmniButtonRight -> ClockButton -> DateTimeIconContent).
    static void ScanSystemTrayClock(
        xaml::DependencyObject const& node,
        xaml::UIElement const& trayRootEl,
        float midlineBottomY,
        ClockSpanInfo& info,
        bool namedOnly,
        int depth = 0) {
        if (!node || depth > 32) return;
        try {
            if (node.try_as<controls::TextBlock>()) {
                if (auto fe = node.try_as<xaml::FrameworkElement>()) {
                    double aw = fe.ActualWidth();
                    double ah = fe.ActualHeight();
                    if (fe.Visibility() == xaml::Visibility::Visible && aw >= 12.0 && ah >= 6.0) {
                        auto nm = fe.Name();
                        auto pt = fe.TransformToVisual(trayRootEl).TransformPoint(winrt::Windows::Foundation::Point{0.0f, 0.0f});
                        float bottom = pt.Y + static_cast<float>(ah);
                        bool named = (nm == L"DateInnerTextBlock" || nm == L"TimeInnerTextBlock");
                        bool isClockPart = named || (!namedOnly && bottom > midlineBottomY);
                        if (isClockPart) {
                            float l = pt.X - 3.0f;
                            float r = pt.X + static_cast<float>(aw) + 3.0f;
                            if (l < info.left) info.left = l;
                            if (r > info.right) info.right = r;
                            if (nm == L"DateInnerTextBlock" || bottom > info.dateTop + info.dateHeight) {
                                info.dateTop = pt.Y;
                                info.dateHeight = static_cast<float>(ah);
                                info.found = true;
                            }
                        }
                    }
                }
            }
            int count = media::VisualTreeHelper::GetChildrenCount(node);
            for (int i = 0; i < count; ++i) {
                ScanSystemTrayClock(
                    media::VisualTreeHelper::GetChild(node, i),
                    trayRootEl,
                    midlineBottomY,
                    info,
                    namedOnly,
                    depth + 1);
            }
        } catch (...) {
        }
    }

    struct ReflectionBand {
        comp::CompositionVisualSurface surface{nullptr};
        comp::SpriteVisual sprite{nullptr};
    };

    // Floor reflection tuning (Leopard-dock style): the mirror is spread from the icon base down
    // to the bottom of the taskbar frame, flattened to fit that space like a glass floor.
    static constexpr float kReflMinScaleY = 0.30f;
    static constexpr float kReflMaxScaleY = 0.85f;
    static constexpr float kReflIconGap = 1.0f;  // thin air gap between icon base and its reflection
    static constexpr float kReflDateGap = 0.8f;  // clock date line has no indicator below it

    static ReflectionBand CreateReflectionBand(
        comp::Compositor const& compositor,
        comp::Visual const& sourceVisual) {
        constexpr float maskEndY = 0.05f; // fade runs almost to the floor (screen bottom)
        auto visSurface = compositor.CreateVisualSurface();
        visSurface.SourceVisual(sourceVisual);

        auto surfBrush = compositor.CreateSurfaceBrush(visSurface);
        surfBrush.Stretch(comp::CompositionStretch::Fill);
        surfBrush.BitmapInterpolationMode(comp::CompositionBitmapInterpolationMode::Linear);

        // No blur: on 18-24px icons it dissolves the mirror; the gradient fade gives the softness.
        auto gradMask = compositor.CreateLinearGradientBrush();
        gradMask.MappingMode(comp::CompositionMappingMode::Relative);
        gradMask.StartPoint(num::float2{0.5f, 1.0f});
        gradMask.EndPoint(num::float2{0.5f, maskEndY});
        gradMask.ColorStops().Append(compositor.CreateColorGradientStop(0.0f, winrt::Windows::UI::Color{138, 255, 255, 255}));
        gradMask.ColorStops().Append(compositor.CreateColorGradientStop(0.45f, winrt::Windows::UI::Color{58, 255, 255, 255}));
        gradMask.ColorStops().Append(compositor.CreateColorGradientStop(1.0f, winrt::Windows::UI::Color{0, 255, 255, 255}));

        auto maskBrush = compositor.CreateMaskBrush();
        maskBrush.Source(surfBrush);
        maskBrush.Mask(gradMask);

        auto sprite = compositor.CreateSpriteVisual();
        sprite.Brush(maskBrush);
        return ReflectionBand{visSurface, sprite};
    }

    static void PlaceBand(
        ReflectionBand const& band,
        float basePtX,
        float basePtY,
        float xStart,
        float width,
        float sliceTop,
        float sliceH,
        float gapPx,
        float floorY) {
        if (width <= 1.0f || sliceH <= 2.0f) {
            band.sprite.IsVisible(false);
            return;
        }
        band.sprite.IsVisible(true);
        band.surface.SourceOffset(num::float2{xStart, sliceTop});
        band.surface.SourceSize(num::float2{width, sliceH});
        band.sprite.Size(num::float2{width, sliceH});
        float topOfRefl = basePtY + sliceTop + sliceH + gapPx;
        float scaleY = (floorY - topOfRefl) / sliceH;
        scaleY = std::clamp(scaleY, kReflMinScaleY, kReflMaxScaleY);
        band.sprite.Scale(num::float3{1.0f, -scaleY, 1.0f});
        band.sprite.Offset(num::float3{basePtX + xStart, topOfRefl + sliceH * scaleY, 0.0f});
    }

    static bool UpdateStripReflectionGeometry(
        xaml::FrameworkElement const& stripFe,
        xaml::UIElement const& stripEl,
        xaml::UIElement const& frameEl,
        ReflectionBand const& leftBand,
        ReflectionBand const& clockBand,
        ReflectionBand const& rightBand,
        bool isSystemTray) {
        try {
            float rw = static_cast<float>(stripFe.ActualWidth());
            float rh = static_cast<float>(stripFe.ActualHeight());
            if (rw < 8.0f || rh < 24.0f) return false;

            auto pt = stripFe.TransformToVisual(frameEl).TransformPoint(winrt::Windows::Foundation::Point{0.0f, 0.0f});
            // Glass floor ends at the bottom of the taskbar frame (screen bottom on a bottom taskbar).
            float floorY = 1e6f;
            if (auto frameFe = frameEl.try_as<xaml::FrameworkElement>()) {
                floorY = static_cast<float>(frameFe.ActualHeight());
            }

            if (!isSystemTray) {
                float iconTop = (rh - 24.0f) * 0.5f;
                if (iconTop < 0.0f) iconTop = 0.0f;
                PlaceBand(leftBand, pt.X, pt.Y, 0.0f, rw, iconTop, 24.0f, kReflIconGap, floorY);
                return true;
            }

            // Tray icons are 16px; a taller slice mirrors blank rows and pushes the reflection away.
            float iconSliceH = 16.0f;
            float iconTop = (rh - iconSliceH) * 0.5f;
            if (iconTop < 0.0f) iconTop = 0.0f;

            // Trust the named clock TextBlocks first. The "below the midline" heuristic can grab tray
            // glyphs (e.g. the ^ chevron) while a restyle is mid-layout, swallowing the icon band.
            ClockSpanInfo clk;
            ScanSystemTrayClock(stripFe, stripEl, iconTop + iconSliceH + 1.0f, clk, true);
            if (!clk.found) {
                clk = ClockSpanInfo{};
                ScanSystemTrayClock(stripFe, stripEl, iconTop + iconSliceH + 1.0f, clk, false);
            }
            if (clk.found && clk.right > clk.left && clk.dateHeight > 4.0f) {
                float cL = (clk.left < 0.0f) ? 0.0f : clk.left;
                float cR = (clk.right > rw) ? rw : clk.right;
                PlaceBand(leftBand, pt.X, pt.Y, 0.0f, cL, iconTop, iconSliceH, kReflIconGap, floorY);

                // Reflect only the bottom date line (DateInnerTextBlock) right beneath its baseline with clean air gap.
                PlaceBand(clockBand, pt.X, pt.Y, cL, cR - cL, clk.dateTop, clk.dateHeight, kReflDateGap, floorY);

                PlaceBand(rightBand, pt.X, pt.Y, cR, rw - cR, iconTop, iconSliceH, kReflIconGap, floorY);
                return true;
            }

            PlaceBand(leftBand, pt.X, pt.Y, 0.0f, rw, iconTop, iconSliceH, kReflIconGap, floorY);
            if (clockBand.sprite) clockBand.sprite.IsVisible(false);
            if (rightBand.sprite) rightBand.sprite.IsVisible(false);
            return false;
        } catch (...) {
            return false;
        }
    }

    void AttachStripReflection(xaml::DependencyObject const& stripObj, bool isSystemTray = false) {
        if (GetTaskbarEdge() != TaskbarEdge::Bottom) return;
        try {
            auto stripFe = stripObj.try_as<xaml::FrameworkElement>();
            auto stripEl = stripObj.try_as<xaml::UIElement>();
            if (!stripFe || !stripEl) return;

            auto frameObj = media::VisualTreeHelper::GetParent(stripObj);
            if (!frameObj) return;
            auto frameEl = frameObj.try_as<xaml::UIElement>();
            if (!frameEl) return;

            auto sourceVisual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(stripEl);
            auto hostVisual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(frameEl);
            if (!sourceVisual || !hostVisual) return;
            auto compositor = hostVisual.Compositor();
            if (!compositor) return;

            if (!isSystemTray) {
                auto band = CreateReflectionBand(compositor, sourceVisual);
                ReflectionBand empty{};
                UpdateStripReflectionGeometry(stripFe, stripEl, frameEl, band, empty, empty, false);

                auto weakStrip = winrt::make_weak(stripFe);
                auto weakStripEl = winrt::make_weak(stripEl);
                auto weakFrame = winrt::make_weak(frameEl);
                auto token = stripFe.SizeChanged(
                    [weakStrip, weakStripEl, weakFrame, band](
                        winrt::Windows::Foundation::IInspectable const&,
                        xaml::SizeChangedEventArgs const&) {
                        if (auto st = weakStrip.get()) {
                            if (auto se = weakStripEl.get()) {
                                if (auto fr = weakFrame.get()) {
                                    ReflectionBand empty{};
                                    UpdateStripReflectionGeometry(st, se, fr, band, empty, empty, false);
                                }
                            }
                        }
                    });

                xaml::Hosting::ElementCompositionPreview::SetElementChildVisual(frameEl, band.sprite);

                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_sizeCallbacks.push_back({weakStrip, token});
                m_childVisualTargets.push_back(weakFrame);
                return;
            }

            // System Tray: 3 independent baseline bands inside a single ContainerVisual
            auto container = compositor.CreateContainerVisual();
            auto leftBand = CreateReflectionBand(compositor, sourceVisual);
            auto clockBand = CreateReflectionBand(compositor, sourceVisual);
            auto rightBand = CreateReflectionBand(compositor, sourceVisual);
            container.Children().InsertAtTop(leftBand.sprite);
            container.Children().InsertAtTop(clockBand.sprite);
            container.Children().InsertAtTop(rightBand.sprite);

            bool readyNow = UpdateStripReflectionGeometry(stripFe, stripEl, frameEl, leftBand, clockBand, rightBand, true);

            auto weakStrip = winrt::make_weak(stripFe);
            auto weakStripEl = winrt::make_weak(stripEl);
            auto weakFrame = winrt::make_weak(frameEl);
            auto token = stripFe.SizeChanged(
                [weakStrip, weakStripEl, weakFrame, leftBand, clockBand, rightBand](
                    winrt::Windows::Foundation::IInspectable const&,
                    xaml::SizeChangedEventArgs const&) {
                    if (auto st = weakStrip.get()) {
                        if (auto se = weakStripEl.get()) {
                            if (auto fr = weakFrame.get()) {
                                UpdateStripReflectionGeometry(st, se, fr, leftBand, clockBand, rightBand, true);
                            }
                        }
                    }
                });

            // Re-measure on the next layout passes: DateInnerTextBlock (~17 levels deep) may not be laid
            // out yet, and on a style reload the tray margins change without resizing it (no SizeChanged).
            // Capped at 90 ticks, and stops early once the clock was found on a few passes in a row, so
            // idle CPU stays strictly 0%.
            {
                auto tries = std::make_shared<int>(0);
                auto stable = std::make_shared<int>(readyNow ? 1 : 0);
                auto layoutTok = std::make_shared<winrt::event_token>();
                *layoutTok = stripFe.LayoutUpdated(
                    [weakStrip, weakStripEl, weakFrame, leftBand, clockBand, rightBand, tries, stable, layoutTok](
                        winrt::Windows::Foundation::IInspectable const&,
                        winrt::Windows::Foundation::IInspectable const&) {
                        auto st = weakStrip.get();
                        auto se = weakStripEl.get();
                        auto fr = weakFrame.get();
                        if (!st || !se || !fr) return;
                        bool done = UpdateStripReflectionGeometry(st, se, fr, leftBand, clockBand, rightBand, true);                        *stable = done ? *stable + 1 : 0;
                        if (*stable >= 4 || ++(*tries) >= 90) {
                            try { st.LayoutUpdated(*layoutTok); } catch (...) {}
                        }
                    });
            }

            xaml::Hosting::ElementCompositionPreview::SetElementChildVisual(frameEl, container);

            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            m_sizeCallbacks.push_back({weakStrip, token});
            m_childVisualTargets.push_back(weakFrame);
        } catch (...) {
        }
    }

    void ApplyStyleIfMatch(
        xaml::DependencyObject const& obj,
        std::wstring const& type,
        std::wstring const& name,
        std::wstring const& parentType) {
        // 1. Floating dock frame: follows Bottom/Top/Left/Right edge + Left/Center alignment
        if (type == L"Taskbar.TaskbarFrame") {
            ApplyLayoutProps(obj, type, name, parentType, true);
            if (auto el = obj.try_as<xaml::UIElement>()) {
                EnableSmoothShellSlide(el);
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_implicitTargets.push_back(winrt::make_weak(el));
            }
            if (auto fe = obj.try_as<xaml::FrameworkElement>()) {
                try { fe.UpdateLayout(); } catch (...) {}
            }
            return;
        }

        // 2. Floating dock pill on RootGrid inside TaskbarFrame (respects selected TaskbarStyle)
        if (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame") {
            int style = ReadGlassConfig().style;
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakeTaskbarPillBackground(obj, style));
            SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakeTaskbarPillBorder(style));
            SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(TaskbarPillBorderThickness(style)));
            float shadowZ = TaskbarPillShadowZ(style);
            if (shadowZ > 0.0f) {
                if (auto el = obj.try_as<xaml::UIElement>()) {
                    try {
                        SaveAndSet(obj, xaml::UIElement::ShadowProperty(), media::ThemeShadow{});
                        el.Translation(num::float3{0.0f, 0.0f, shadowZ});
                        std::lock_guard<std::recursive_mutex> lock(m_mutex);
                        m_implicitTargets.push_back(winrt::make_weak(el));
                    } catch (...) {}
                }
            }
            ApplyLayoutProps(obj, type, name, parentType, true);
            try {
                auto token = obj.RegisterPropertyChangedCallback(
                    controls::Grid::PaddingProperty(),
                    [](xaml::DependencyObject const& sender, xaml::DependencyProperty const& dp) {
                        try {
                            TaskbarEdge edge = GetTaskbarEdge();
                            if (edge == TaskbarEdge::Bottom || edge == TaskbarEdge::Top) {
                                double wantLeft = IsTaskbarAlignedStart() ? 0.0 : 3.0;
                                auto cur = winrt::unbox_value_or<xaml::Thickness>(
                                    sender.GetValue(dp), xaml::Thickness{wantLeft, 0.0, 3.0, 0.0});
                                if (cur.Left != wantLeft || cur.Right != 3.0) {
                                    sender.SetValue(dp, winrt::box_value(xaml::Thickness{wantLeft, cur.Top, 3.0, cur.Bottom}));
                                }
                            }
                        } catch (...) {
                        }
                    });
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_propCallbacks.push_back({winrt::make_weak(obj), controls::Grid::PaddingProperty(), token});
            } catch (...) {
            }
            if (auto fe = obj.try_as<xaml::FrameworkElement>()) {
                try { fe.UpdateLayout(); } catch (...) {}
            }
            if (style == STYLE_ICON_REFLECT) {
                AttachStripReflection(obj);
            }
            return;
        }

        // 3. Hide ONLY the main taskbar's full-width background & stroke (never collapse HoverFlyoutBackgroundControl!)
        if ((type == L"Taskbar.TaskbarBackground" && name == L"BackgroundControl")
            || ((name == L"BackgroundFill" || name == L"BackgroundStroke")
                && parentType == L"Taskbar.BackgroundControl")) {
            SaveAndSet(obj, xaml::UIElement::VisibilityProperty(), winrt::box_value(xaml::Visibility::Collapsed));
            return;
        }

        // 3b. Trim left offset on Start button (LaunchListButton) when left-aligned so both pill ends match
        if (type == L"Taskbar.ExperienceToggleButton" && name == L"LaunchListButton") {
            ApplyLayoutProps(obj, type, name, parentType, true);
            try {
                auto token = obj.RegisterPropertyChangedCallback(
                    xaml::FrameworkElement::MarginProperty(),
                    [](xaml::DependencyObject const& sender, xaml::DependencyProperty const& dp) {
                        try {
                            xaml::Thickness want = LaunchButtonMargin();
                            auto cur = winrt::unbox_value_or<xaml::Thickness>(sender.GetValue(dp), want);
                            if (cur.Left != want.Left || cur.Top != want.Top || cur.Right != want.Right
                                || cur.Bottom != want.Bottom) {
                                sender.SetValue(dp, winrt::box_value(want));
                            }
                        } catch (...) {
                        }
                    });
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_propCallbacks.push_back({winrt::make_weak(obj), xaml::FrameworkElement::MarginProperty(), token});
            } catch (...) {
            }
            return;
        }

        // 3c. Replace thumbnail hover preview background with PuwBar frosted glass (when enabled)
        if (name == L"HoverFlyoutBackgroundControl") {
            if ((ReadGlassConfig().surfaces & SURF_THUMBNAIL) != 0) {
                SaveAndSet(obj, xaml::UIElement::VisibilityProperty(), winrt::box_value(xaml::Visibility::Collapsed));
            }
            return;
        }

        if (name == L"HoverFlyoutBackground") {
            if ((ReadGlassConfig().surfaces & SURF_THUMBNAIL) != 0) {
                SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
                SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            }
            return;
        }

        // 4. Separate floating pill for System Tray (respects selected TaskbarStyle)
        if (name == L"SystemTrayFrameGrid") {
            int style = ReadGlassConfig().style;
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakeTaskbarPillBackground(obj, style));
            if (obj.try_as<controls::StackPanel>()) {
                SaveAndSet(obj, controls::StackPanel::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::StackPanel::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::StackPanel::BorderBrushProperty(), MakeTaskbarPillBorder(style));
                SaveAndSet(obj, controls::StackPanel::BorderThicknessProperty(), winrt::box_value(TaskbarPillBorderThickness(style)));
            } else if (obj.try_as<controls::Grid>()) {
                SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakeTaskbarPillBorder(style));
                SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(TaskbarPillBorderThickness(style)));
            }
            ApplyLayoutProps(obj, type, name, parentType, true);
            if (auto el = obj.try_as<xaml::UIElement>()) {
                EnableSmoothShellSlide(el);
                float shadowZ = TaskbarPillShadowZ(style);
                if (shadowZ > 0.0f) {
                    try {
                        SaveAndSet(obj, xaml::UIElement::ShadowProperty(), media::ThemeShadow{});
                        el.Translation(num::float3{0.0f, 0.0f, shadowZ});
                    } catch (...) {}
                }
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_implicitTargets.push_back(winrt::make_weak(el));
            }
            if (auto fe = obj.try_as<xaml::FrameworkElement>()) {
                try { fe.UpdateLayout(); } catch (...) {}
            }
            if (style == STYLE_ICON_REFLECT) {
                AttachStripReflection(obj, true);
            }
            return;
        }

        // 5. Matching glass background (12px blur + 65% dark-smoke veil) for tray overflow & taskbar '...' flyouts
        if (name == L"OverflowFlyoutBackgroundBorder") {
            SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            SoftenFlyoutShadow(obj);
            return;
        }

        if (type == L"Windows.UI.Xaml.Controls.FlyoutPresenter") {
            SaveAndSet(obj, controls::Control::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Control::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Control::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Control::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            SaveAndSet(obj, controls::Control::PaddingProperty(), winrt::box_value(xaml::Thickness{2.0, 4.0, 2.0, 4.0}));
            SoftenFlyoutShadow(obj);
            return;
        }

        // 5b. Context menus (system tray right-click, Start button Win+X): keep the native AcrylicBrush and tune
        // it in place. Nothing else works here: the composition glass as the menu's Background makes the items
        // unclickable (confirmed on a freshly restarted Explorer), and placed behind the menu it finds nothing to
        // blur (tried with the plain and the host backdrop, with and without a native acrylic to enable it). So
        // this menu has the native blur and a fixed tint, without the per-pixel auto contrast.
        if (type == L"Windows.UI.Xaml.Controls.MenuFlyoutPresenter") {
            try {
                if (auto ctrl = obj.try_as<controls::Control>()) {
                    if (auto ac = ctrl.Background().try_as<media::AcrylicBrush>()) {
                        StyleNativeAcrylicInPlace(ac);
                    }
                }
            } catch (...) {
            }
            SaveAndSet(obj, controls::Control::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Control::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Control::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            return;
        }

        // 5c. Matching glass background for taskbar & tray hover tooltips (when enabled)
        if (type == L"Windows.UI.Xaml.Controls.ToolTip") {
            if ((ReadGlassConfig().surfaces & SURF_TOOLTIP) != 0) {
                SaveAndSet(obj, controls::Control::BackgroundProperty(), MakePuwGlass(obj));
                SaveAndSet(obj, controls::Control::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{8.0, 8.0, 8.0, 8.0}));
                SaveAndSet(obj, controls::Control::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Control::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            }
            return;
        }

        // 5d. Glass for Notification Center, Calendar & Quick Settings (individually toggleable; locked against VisualState resets)
        if (name == L"NotificationCenterGrid"
            || name == L"CalendarCenterGrid"
            || name == L"ControlCenterRegion"
            || name == L"MediaTransportControlsRegion") {
            uint32_t surfaces = ReadGlassConfig().surfaces;
            bool enabled = (name == L"NotificationCenterGrid") ? ((surfaces & SURF_NOTIFICATIONS) != 0)
                : (name == L"CalendarCenterGrid") ? ((surfaces & SURF_CALENDAR) != 0)
                : ((surfaces & SURF_CONTROL_CENTER) != 0);
            if (enabled) {
                SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
                LockGlassBackground(obj, controls::Panel::BackgroundProperty());
                if (obj.try_as<controls::Grid>()) {
                    SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                    SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                    SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
                    SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
                }
                SoftenFlyoutShadow(obj);
            }
            return;
        }

        // 5e. Taskbar Jump Lists: use PuwGlassBrush (never falls back to opaque fbA=255 on focus loss/slide-down)
        //     on JumpListRestyledAcrylic, and make stacked inner acrylic borders transparent so 24%/45%/62% matches exactly.
        if (name == L"JumpListRestyledAcrylic") {
            if ((ReadGlassConfig().surfaces & SURF_JUMP_LIST) != 0) {
                SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
                LockGlassBackground(obj, controls::Border::BackgroundProperty());
                SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
                SoftenFlyoutShadow(obj);
            }
            return;
        }

        if (name == L"SystemItemsAcrylic" || name == L"JumpListAcrylic") {
            if ((ReadGlassConfig().surfaces & SURF_JUMP_LIST) != 0) {
                SaveAndSet(obj, controls::Border::BackgroundProperty(), media::SolidColorBrush(winrt::Windows::UI::Color{0, 0, 0, 0}));
            }
            return;
        }

        // 5f. Start Menu: frosted glass on AcrylicBorder (keeps native two-tone AcrylicOverlay intact)
        if (name == L"AcrylicBorder" && GetModuleHandleW(L"StartMenuExperienceHost.exe") != nullptr) {
            if ((ReadGlassConfig().surfaces & SURF_START_MENU) != 0) {
                SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
                LockGlassBackground(obj, controls::Border::BackgroundProperty());
                SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
                SoftenFlyoutShadow(obj);
            }
            return;
        }

        if (name == L"IconPanel" && parentType == L"Taskbar.TaskListButton") {
            ApplyLayoutProps(obj, type, name, parentType, true);
            return;
        }

        if (name == L"RunningIndicator") {
            ApplyLayoutProps(obj, type, name, parentType, true);
            // Keep right-edge indicator locked on Right even when VisualStateManager updates active app state.
            try {
                auto token = obj.RegisterPropertyChangedCallback(
                    xaml::FrameworkElement::HorizontalAlignmentProperty(),
                    [](xaml::DependencyObject const& sender, xaml::DependencyProperty const& dp) {
                        try {
                            if (GetTaskbarEdge() == TaskbarEdge::Right) {
                                auto cur = winrt::unbox_value_or<xaml::HorizontalAlignment>(
                                    sender.GetValue(dp), xaml::HorizontalAlignment::Center);
                                if (cur != xaml::HorizontalAlignment::Right) {
                                    sender.SetValue(dp, winrt::box_value(xaml::HorizontalAlignment::Right));
                                }
                            }
                        } catch (...) {
                        }
                    });
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_propCallbacks.push_back({winrt::make_weak(obj), xaml::FrameworkElement::HorizontalAlignmentProperty(), token});
            } catch (...) {
            }
            return;
        }

        // 6. Matching floating pill for the Widgets button (respects selected TaskbarStyle)
        if (name == L"ExperienceToggleButtonRootPanel"
            && parentType == L"Taskbar.AugmentedEntryPointButton") {
            int style = ReadGlassConfig().style;
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakeTaskbarPillBackground(obj, style));
            if (obj.try_as<controls::Grid>()) {
                SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakeTaskbarPillBorder(style));
                SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(TaskbarPillBorderThickness(style)));
            }
            ApplyLayoutProps(obj, type, name, parentType, true);
            if (auto el = obj.try_as<xaml::UIElement>()) {
                EnableSmoothShellSlide(el);
                float shadowZ = TaskbarPillShadowZ(style);
                if (shadowZ > 0.0f) {
                    try {
                        SaveAndSet(obj, xaml::UIElement::ShadowProperty(), media::ThemeShadow{});
                        el.Translation(num::float3{0.0f, 0.0f, shadowZ});
                    } catch (...) {}
                }
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_implicitTargets.push_back(winrt::make_weak(el));
            }
            if (auto fe = obj.try_as<xaml::FrameworkElement>()) {
                try { fe.UpdateLayout(); } catch (...) {}
            }
            if (style == STYLE_ICON_REFLECT) {
                AttachStripReflection(obj, false);
            }
            return;
        }
    }

    template <typename Fn>
    static void RunOnObjectThread(xaml::DependencyObject const& obj, Fn&& fn) {
        try {
            auto disp = obj.Dispatcher();
            if (disp && !disp.HasThreadAccess()) {
                disp.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::High,
                    [f = std::forward<Fn>(fn)]() mutable {
                        try { f(); } catch (...) {}
                    });
            } else {
                fn();
            }
        } catch (...) {
        }
    }

    // Native shell flyouts cast a deep ThemeShadow (Z ~32-64) that is clipped flat by the flyout
    // host window at the taskbar edge; behind see-through glass that clip reads as a hard cut line.
    // Cap the Z of the nearest shadow-casting ancestor so the shadow stays inside the host window.
    void SoftenFlyoutShadow(xaml::DependencyObject const& obj) {
        // Some hosts (e.g. Start's #StartDropShadow) cast the shadow from a sibling, so each
        // ancestor's direct children are checked too.
        // Soft enough to read as a shadow, low enough that its blur stays above the taskbar edge.
        constexpr float kMaxFlyoutShadowZ = 16.0f;
        auto cap = [&](xaml::DependencyObject const& o) {
            auto el = o.try_as<xaml::UIElement>();
            if (!el || !el.Shadow()) return;
            auto t = el.Translation();
            if (t.z <= kMaxFlyoutShadowZ) return;
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_savedTranslations.push_back({winrt::make_weak(el), t});
            }
            el.Translation(num::float3{t.x, t.y, kMaxFlyoutShadowZ});
        };
        xaml::DependencyObject node = obj;
        for (int depth = 0; node && depth < 10; ++depth) {
            try {
                cap(node);
                int n = media::VisualTreeHelper::GetChildrenCount(node);
                for (int i = 0; i < n; ++i) {
                    cap(media::VisualTreeHelper::GetChild(node, i));
                }
                node = media::VisualTreeHelper::GetParent(node);
            } catch (...) {
                return;
            }
        }
    }

    void RestoreAllOnUiThread() {
        std::vector<SavedPropCallback> callbacks;
        std::vector<SavedSizeCallback> sizeCallbacks;
        std::vector<winrt::weak_ref<xaml::UIElement>> childVisuals;
        std::vector<winrt::weak_ref<xaml::UIElement>> implicits;
        std::vector<SavedAcrylic> acrylics;
        std::vector<SavedProp> items;
        std::vector<SavedTranslation> translations;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            acrylics.swap(m_savedAcrylics);
            translations.swap(m_savedTranslations);
            callbacks.swap(m_propCallbacks);
            sizeCallbacks.swap(m_sizeCallbacks);
            childVisuals.swap(m_childVisualTargets);
            implicits.swap(m_implicitTargets);
            items.swap(m_saved);
        }
        for (auto& sc : sizeCallbacks) {
            try {
                if (auto fe = sc.target.get()) {
                    auto token = sc.token;
                    RunOnObjectThread(fe, [fe, token]() {
                        fe.SizeChanged(token);
                    });
                }
            } catch (...) {
            }
        }
        for (auto& cv : childVisuals) {
            try {
                if (auto el = cv.get()) {
                    RunOnObjectThread(el, [el]() {
                        xaml::Hosting::ElementCompositionPreview::SetElementChildVisual(el, nullptr);
                    });
                }
            } catch (...) {
            }
        }
        for (auto& cb : callbacks) {
            try {
                if (auto obj = cb.target.get()) {
                    auto prop = cb.prop;
                    auto token = cb.token;
                    RunOnObjectThread(obj, [obj, prop, token]() {
                        obj.UnregisterPropertyChangedCallback(prop, token);
                    });
                }
            } catch (...) {
            }
        }
        for (auto& imp : implicits) {
            try {
                if (auto el = imp.get()) {
                    RunOnObjectThread(el, [el]() {
                        try { el.Translation(num::float3{0.0f, 0.0f, 0.0f}); } catch (...) {}
                        if (auto visual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(el)) {
                            visual.ImplicitAnimations(nullptr);
                        }
                    });
                }
            } catch (...) {
            }
        }
        for (auto it = acrylics.rbegin(); it != acrylics.rend(); ++it) {
            try {
                if (auto ac = it->brush.get()) {
                    auto tintColor = it->tintColor;
                    auto tintOpacity = it->tintOpacity;
                    auto tintLum = it->tintLum;
                    RunOnObjectThread(ac, [ac, tintColor, tintOpacity, tintLum]() {
                        ac.TintColor(tintColor);
                        ac.TintOpacity(tintOpacity);
                        ac.TintLuminosityOpacity(tintLum);
                    });
                }
            } catch (...) {
            }
        }
        for (auto& t : translations) {
            try {
                if (auto el = t.el.get()) {
                    auto original = t.original;
                    RunOnObjectThread(el, [el, original]() {
                        try { el.Translation(original); } catch (...) {}
                    });
                }
            } catch (...) {
            }
        }
        for (auto it = items.rbegin(); it != items.rend(); ++it) {
            try {
                if (auto obj = it->target.get()) {
                    auto prop = it->prop;
                    auto original = it->original;
                    bool wasUnset = it->wasUnset;
                    RunOnObjectThread(obj, [obj, prop, original, wasUnset]() {
                        if (wasUnset) {
                            obj.ClearValue(prop);
                        } else if (original) {
                            obj.SetValue(prop, original);
                        }
                    });
                }
            } catch (...) {
            }
        }
    }

    void ReloadStyleOnUiThread() {
        RestoreAllOnUiThread();
        std::vector<TrackedElement> alive;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            alive.reserve(m_tracked.size());
            for (auto& t : m_tracked) {
                if (t.target.get()) alive.push_back(t);
            }
            m_tracked = alive;
        }
        for (auto& t : alive) {
            try {
                if (auto obj = t.target.get()) {
                    auto typeCopy = t.type;
                    auto nameCopy = t.name;
                    auto parentCopy = t.parentType;
                    AddRef();
                    RunOnObjectThread(obj, [this, obj, typeCopy, nameCopy, parentCopy]() {
                        ApplyStyleIfMatch(obj, typeCopy, nameCopy, parentCopy);
                        Release();
                    });
                }
            } catch (...) {
            }
        }
    }

    void DispatchReload() {
        winrt::Windows::UI::Core::CoreDispatcher dispatcher{nullptr};
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            dispatcher = m_dispatcher;
        }
        if (dispatcher) {
            try {
                AddRef();
                dispatcher.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                    [this]() {
                        ReloadStyleOnUiThread();
                        Release();
                    });
            } catch (...) {
                Release();
            }
        }
    }

    void RequestRestoreAndUnadvise() {
        winrt::Windows::UI::Core::CoreDispatcher dispatcher{nullptr};
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            dispatcher = m_dispatcher;
            if (m_vts && m_advised) {
                m_vts->UnadviseVisualTreeChange(this);
                m_advised = false;
            }
        }
        if (dispatcher) {
            try {
                AddRef();
                dispatcher.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                    [this]() {
                        RestoreAllOnUiThread();
                        Release();
                    });
            } catch (...) {
                Release();
            }
        }
    }

    void SyncAlignmentOnUiThread() {
        std::vector<TrackedElement> targets;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            for (auto& t : m_tracked) {
                if (t.type == L"Taskbar.TaskbarFrame"
                    || (t.name == L"RootGrid" && t.parentType == L"Taskbar.TaskbarFrame")
                    || (t.type == L"Taskbar.ExperienceToggleButton" && t.name == L"LaunchListButton")
                    || t.name == L"SystemTrayFrameGrid"
                    || t.name == L"RunningIndicator"
                    || (t.name == L"IconPanel" && t.parentType == L"Taskbar.TaskListButton")
                    || (t.name == L"ExperienceToggleButtonRootPanel"
                        && t.parentType == L"Taskbar.AugmentedEntryPointButton")) {
                    targets.push_back(t);
                }
            }
        }
        for (auto& t : targets) {
            try {
                if (auto obj = t.target.get()) {
                    ApplyLayoutProps(obj, t.type, t.name, t.parentType, false);
                }
            } catch (...) {
            }
        }
    }

    void DispatchSyncAlignment() {
        winrt::Windows::UI::Core::CoreDispatcher dispatcher{nullptr};
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            dispatcher = m_dispatcher;
        }
        if (dispatcher) {
            try {
                AddRef();
                dispatcher.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                    [this]() {
                        SyncAlignmentOnUiThread();
                        Release();
                    });
            } catch (...) {
                Release();
            }
        }
    }

    static std::string Utf8(std::wstring const& w) {
        if (w.empty()) return {};
        int n = WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()), nullptr, 0, nullptr, nullptr);
        std::string out(static_cast<size_t>(n), '\0');
        WideCharToMultiByte(CP_UTF8, 0, w.data(), static_cast<int>(w.size()), out.data(), n, nullptr, nullptr);
        return out;
    }

    // One line per visual-tree node: class, name, size, position inside the taskbar window, margin, alignment, visibility.
    static void DumpNode(xaml::DependencyObject const& node, xaml::UIElement const& root, int depth, int& budget, std::string& out) {
        if (!node || depth > 40 || budget <= 0) return;
        --budget;
        try {
            std::string line(static_cast<size_t>(depth) * 2, ' ');
            line += Utf8(std::wstring(winrt::get_class_name(node)));
            if (auto fe = node.try_as<xaml::FrameworkElement>()) {
                auto nm = fe.Name();
                if (!nm.empty()) line += "#" + Utf8(std::wstring(nm));
                float x = 0.0f, y = 0.0f;
                try {
                    auto pt = fe.TransformToVisual(root).TransformPoint(winrt::Windows::Foundation::Point{0.0f, 0.0f});
                    x = pt.X;
                    y = pt.Y;
                } catch (...) {
                }
                auto m = fe.Margin();
                char buf[320];
                snprintf(buf, sizeof(buf), "  size=%.1fx%.1f at=(%.1f,%.1f) margin=(%.1f,%.1f,%.1f,%.1f) halign=%d valign=%d visibility=%d",
                    fe.ActualWidth(), fe.ActualHeight(), x, y, m.Left, m.Top, m.Right, m.Bottom,
                    static_cast<int>(fe.HorizontalAlignment()), static_cast<int>(fe.VerticalAlignment()), static_cast<int>(fe.Visibility()));
                line += buf;
            }
            out += line;
            out += "\n";
        } catch (...) {
        }
        int count = media::VisualTreeHelper::GetChildrenCount(node);
        for (int i = 0; i < count; ++i) {
            DumpNode(media::VisualTreeHelper::GetChild(node, i), root, depth + 1, budget, out);
        }
    }

    // Writes the taskbar's XAML tree to %TEMP%\puwbar_diag_tap.txt for puwbar.exe to fold into its report.
    void DumpTreeOnUiThread(std::wstring const& fileName) {
        // Every tracked element that is still alive, grouped by the root of its visual tree: after a taskbar position
        // change Windows replaces the frame, and this shows which tree PuwBar still holds and which one it never saw.
        struct Alive {
            std::wstring type;
            std::wstring name;
            xaml::DependencyObject obj{nullptr};
        };
        std::vector<Alive> alive;
        size_t total = 0;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            total = m_tracked.size();
            for (auto& t : m_tracked) {
                if (auto obj = t.target.get()) alive.push_back(Alive{t.type, t.name, obj});
            }
        }
        std::string out = "tracked elements: " + std::to_string(total) + ", still alive: " + std::to_string(alive.size()) + "\n";
        std::vector<xaml::UIElement> roots;
        std::vector<std::string> kinds;
        for (auto& e : alive) {
            xaml::UIElement root{nullptr};
            try {
                xaml::DependencyObject cur = e.obj;
                while (cur) {
                    if (auto ui = cur.try_as<xaml::UIElement>()) root = ui;
                    cur = media::VisualTreeHelper::GetParent(cur);
                }
            } catch (...) {
            }
            std::string label = Utf8(e.type) + (e.name.empty() ? "" : "#" + Utf8(e.name));
            if (!root) {
                out += "  detached: " + label + "\n";
                continue;
            }
            size_t k = 0;
            while (k < roots.size() && winrt::get_abi(roots[k]) != winrt::get_abi(root)) ++k;
            if (k == roots.size()) {
                roots.push_back(root);
                kinds.emplace_back();
            }
            kinds[k] += "    " + label + "\n";
        }
        int index = 0;
        for (auto& root : roots) {
            out += "\n--- visual tree " + std::to_string(index) + ": tracked elements under it ---\n" + kinds[static_cast<size_t>(index)];
            out += "--- tree " + std::to_string(index) + " ---\n";
            ++index;
            int budget = 4000;
            DumpNode(root, root, 0, budget, out);
        }
        wchar_t tmp[MAX_PATH]{};
        DWORD len = GetTempPathW(MAX_PATH, tmp);
        if (len == 0 || len >= MAX_PATH) return;
        std::wstring path = std::wstring(tmp, len) + fileName;
        HANDLE h = CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (h == INVALID_HANDLE_VALUE) return;
        DWORD written = 0;
        WriteFile(h, out.data(), static_cast<DWORD>(out.size()), &written, nullptr);
        CloseHandle(h);
    }

    // Two seconds after a taskbar position change, so the replaced frame has settled, writes a snapshot of the trees
    // PuwBar tracks to %TEMP%\puwbar_diag_move.txt (overwritten each time).
    void DispatchDelayedDiag() {
        AddRef();
        std::thread([this]() {
            Sleep(2000);
            DispatchDiag(L"puwbar_diag_move.txt");
            Release();
        }).detach();
    }

    void DispatchDiag(std::wstring fileName = L"puwbar_diag_tap.txt") {
        winrt::Windows::UI::Core::CoreDispatcher dispatcher{nullptr};
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            dispatcher = m_dispatcher;
        }
        if (dispatcher) {
            try {
                AddRef();
                dispatcher.RunAsync(
                    winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                    [this, fileName]() {
                        DumpTreeOnUiThread(fileName);
                        Release();
                    });
            } catch (...) {
                Release();
            }
        }
    }

    void StartWatcher() {
        StopWatcher();
        m_stopWatcher.store(false);
        // Inside auxiliary UWP hosts (Quick Settings, Notifications/Calendar, Start Menu),
        // only poll the shared dark-tint toggle file so live tray toggles apply without restarting hosts.
        if (GetModuleHandleW(L"ShellExperienceHost.exe") != nullptr
            || GetModuleHandleW(L"ShellHost.exe") != nullptr
            || GetModuleHandleW(L"StartMenuExperienceHost.exe") != nullptr) {
            AddRef();
            m_watcher = std::thread([this]() {
                uint32_t lastPacked = ReadGlassConfig().packed();
                while (!m_stopWatcher.load()) {
                    Sleep(200);
                    if (m_stopWatcher.load()) break;
                    GlassConfig cur = ReadGlassConfig();
                    if (cur.stop) {
                        RequestRestoreAndUnadvise();
                        break;
                    }
                    if (cur.packed() != lastPacked) {
                        lastPacked = cur.packed();
                        DispatchReload();
                    }
                }
                Release();
            });
            return;
        }
        AddRef();
        m_watcher = std::thread([this]() {
            HANDLE stopEv = OpenEventW(SYNCHRONIZE, FALSE, L"Local\\PuwBar.StopEvent");
            HANDLE reloadEv = OpenEventW(SYNCHRONIZE, FALSE, L"Local\\PuwBar.ReloadEvent");
            HANDLE diagEv = OpenEventW(SYNCHRONIZE, FALSE, L"Local\\PuwBar.DiagEvent");
            HANDLE alignEv = CreateEventW(nullptr, FALSE, FALSE, nullptr);
            HKEY advKey = nullptr;
            bool watchingReg = false;
            if (alignEv && RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    L"Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced",
                    0,
                    KEY_NOTIFY,
                    &advKey) == ERROR_SUCCESS) {
                watchingReg = (RegNotifyChangeKeyValue(advKey, FALSE, REG_NOTIFY_CHANGE_LAST_SET, alignEv, TRUE) == ERROR_SUCCESS);
            }

            HANDLE handles[5]{};
            int stopIdx = -1, reloadIdx = -1, alignIdx = -1, posIdx = -1, diagIdx = -1;
            DWORD count = 0;
            if (stopEv) { stopIdx = static_cast<int>(count); handles[count++] = stopEv; }
            if (reloadEv) { reloadIdx = static_cast<int>(count); handles[count++] = reloadEv; }
            if (diagEv) { diagIdx = static_cast<int>(count); handles[count++] = diagEv; }
            if (watchingReg) { alignIdx = static_cast<int>(count); handles[count++] = alignEv; }

            HANDLE posEv = CreateEventW(nullptr, FALSE, FALSE, nullptr);
            HKEY stuckKey = nullptr;
            if (posEv && RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    L"Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StuckRects3",
                    0,
                    KEY_NOTIFY,
                    &stuckKey) == ERROR_SUCCESS) {
                if (RegNotifyChangeKeyValue(stuckKey, FALSE, REG_NOTIFY_CHANGE_LAST_SET, posEv, TRUE) == ERROR_SUCCESS) {
                    posIdx = static_cast<int>(count);
                    handles[count++] = posEv;
                }
            }

            TaskbarEdge lastEdge = GetTaskbarEdge();
            if (count > 0) {
                while (!m_stopWatcher.load()) {
                    DWORD wait = WaitForMultipleObjects(count, handles, FALSE, 300);
                    if (stopIdx >= 0 && wait == WAIT_OBJECT_0 + static_cast<DWORD>(stopIdx)) {
                        RequestRestoreAndUnadvise();
                        break;
                    }
                    if (reloadIdx >= 0 && wait == WAIT_OBJECT_0 + static_cast<DWORD>(reloadIdx)) {
                        DispatchReload();
                    }
                    if (diagIdx >= 0 && wait == WAIT_OBJECT_0 + static_cast<DWORD>(diagIdx)) {
                        DispatchDiag();
                    }
                    if (alignIdx >= 0 && wait == WAIT_OBJECT_0 + static_cast<DWORD>(alignIdx)) {
                        DispatchSyncAlignment();
                        if (advKey && alignEv) {
                            RegNotifyChangeKeyValue(advKey, FALSE, REG_NOTIFY_CHANGE_LAST_SET, alignEv, TRUE);
                        }
                    }
                    if (posIdx >= 0 && wait == WAIT_OBJECT_0 + static_cast<DWORD>(posIdx)) {
                        Sleep(120);
                        lastEdge = GetTaskbarEdge();
                        DispatchSyncAlignment();
                        DispatchDelayedDiag();
                        if (stuckKey && posEv) {
                            RegNotifyChangeKeyValue(stuckKey, FALSE, REG_NOTIFY_CHANGE_LAST_SET, posEv, TRUE);
                        }
                    }
                    if (wait == WAIT_TIMEOUT) {
                        TaskbarEdge cur = GetTaskbarEdge();
                        if (cur != lastEdge) {
                            lastEdge = cur;
                            DispatchSyncAlignment();
                            DispatchDelayedDiag();
                        }
                    }
                }
            }
            if (stuckKey) RegCloseKey(stuckKey);
            if (posEv) CloseHandle(posEv);
            if (advKey) RegCloseKey(advKey);
            if (alignEv) CloseHandle(alignEv);
            if (stopEv) CloseHandle(stopEv);
            if (reloadEv) CloseHandle(reloadEv);
            if (diagEv) CloseHandle(diagEv);
            Release();
        });
    }

    void StopWatcher() {
        m_stopWatcher.store(true);
        if (m_watcher.joinable()) {
            m_watcher.detach();
        }
    }

    void DetachLocked() {
        if (m_vts) {
            if (m_advised) {
                m_vts->UnadviseVisualTreeChange(this);
                m_advised = false;
            }
            m_vts->Release();
            m_vts = nullptr;
        }
        if (m_diag) {
            m_diag->Release();
            m_diag = nullptr;
        }
        if (m_site) {
            m_site->Release();
            m_site = nullptr;
        }
        m_meta.clear();
        m_tracked.clear();
    }

    std::atomic<long> m_ref{1};
    std::recursive_mutex m_mutex;
    IUnknown* m_site = nullptr;
    IXamlDiagnostics* m_diag = nullptr;
    IVisualTreeService3* m_vts = nullptr;
    bool m_advised = false;
    winrt::Windows::UI::Core::CoreDispatcher m_dispatcher{nullptr};
    std::unordered_map<InstanceHandle, ElementMeta> m_meta;
    std::vector<TrackedElement> m_tracked;
    std::vector<SavedProp> m_saved;
    std::vector<SavedPropCallback> m_propCallbacks;
    std::vector<SavedSizeCallback> m_sizeCallbacks;
    std::vector<SavedAcrylic> m_savedAcrylics;
    std::vector<winrt::weak_ref<xaml::UIElement>> m_implicitTargets;
    std::vector<winrt::weak_ref<xaml::UIElement>> m_childVisualTargets;
    std::vector<SavedTranslation> m_savedTranslations;
    std::atomic<bool> m_stopWatcher{false};
    std::thread m_watcher;
};

class PuwBarClassFactory : public IClassFactory {
public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override {
        if (!ppv) return E_POINTER;
        *ppv = nullptr;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IClassFactory)) {
            *ppv = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(m_ref.fetch_add(1, std::memory_order_relaxed) + 1);
    }

    ULONG STDMETHODCALLTYPE Release() override {
        long count = m_ref.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (count == 0) delete this;
        return static_cast<ULONG>(count);
    }

    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* pUnkOuter, REFIID riid, void** ppv) override {
        if (pUnkOuter) return CLASS_E_NOAGGREGATION;
        auto* tap = new (std::nothrow) PuwBarTap();
        if (!tap) return E_OUTOFMEMORY;
        HRESULT hr = tap->QueryInterface(riid, ppv);
        tap->Release();
        return hr;
    }

    HRESULT STDMETHODCALLTYPE LockServer(BOOL fLock) override {
        if (fLock) g_dllRefCount.fetch_add(1, std::memory_order_relaxed);
        else g_dllRefCount.fetch_sub(1, std::memory_order_relaxed);
        return S_OK;
    }

private:
    std::atomic<long> m_ref{1};
};

extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID rclsid, REFIID riid, void** ppv) {
    if (rclsid != CLSID_PuwBarTap) return CLASS_E_CLASSNOTAVAILABLE;
    auto* factory = new (std::nothrow) PuwBarClassFactory();
    if (!factory) return E_OUTOFMEMORY;
    HRESULT hr = factory->QueryInterface(riid, ppv);
    factory->Release();
    return hr;
}

extern "C" HRESULT __stdcall DllCanUnloadNow() {
    return g_dllRefCount.load(std::memory_order_acquire) == 0 ? S_OK : S_FALSE;
}

BOOL APIENTRY DllMain(HMODULE hMod, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        g_hModule = hMod;
    }
    return TRUE;
}
