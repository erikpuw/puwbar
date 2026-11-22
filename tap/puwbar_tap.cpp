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

struct GlassConfig {
    int level = 2; // 0 = Light (24%), 1 = Balanced (45%), 2 = High contrast (62%)
    uint32_t surfaces = SURF_ALL;
    bool stop = false;

    uint32_t packed() const {
        return (static_cast<uint32_t>(level) << 8) | (surfaces & SURF_ALL);
    }
};

// Reads "<level>:<hex-mask>" or "stop" from puwbar_dark_tint.cfg next to puwbar_tap_*.dll.
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
    char buf[16]{};
    DWORD read = 0;
    bool ok = ReadFile(h, buf, sizeof(buf) - 1, &read, nullptr) != 0 && read >= 1;
    CloseHandle(h);
    if (!ok) return cfg;
    if (buf[0] == 's') {
        cfg.stop = true;
        return cfg;
    }
    if (buf[0] == '0') cfg.level = 0;
    else if (buf[0] == '1') cfg.level = 1;
    else cfg.level = 2;
    if (read >= 3 && buf[1] == ':') {
        unsigned long mask = strtoul(buf + 2, nullptr, 16);
        cfg.surfaces = static_cast<uint32_t>(mask) & SURF_ALL;
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

struct SavedAcrylic {
    winrt::weak_ref<media::AcrylicBrush> brush;
    winrt::Windows::UI::Color tintColor{};
    double tintOpacity = 1.0;
    winrt::Windows::Foundation::IReference<double> tintLum{nullptr};
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

struct ColorTintEffect : winrt::implements<ColorTintEffect, ge::IGraphicsEffect, ge::IGraphicsEffectSource, ge_interop::IGraphicsEffectD2D1Interop> {
    winrt::hstring m_name{L"Tint"};
    // Premultiplied RGBA: 0 = 24% light, 1 = 45% balanced, 2 = 62% high contrast
    float m_color[4] = {0.028f, 0.035f, 0.052f, 0.62f};

    explicit ColorTintEffect(int level) {
        if (level == 0) {
            m_color[0] = 0.020f;
            m_color[1] = 0.025f;
            m_color[2] = 0.040f;
            m_color[3] = 0.24f;
        } else if (level == 1) {
            m_color[0] = 0.024f;
            m_color[1] = 0.030f;
            m_color[2] = 0.046f;
            m_color[3] = 0.45f;
        }
    }

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

static winrt::Windows::UI::Color FallbackColorForLevel(int level) {
    if (level == 0) return winrt::Windows::UI::Color{90, 18, 22, 32};
    if (level == 1) return winrt::Windows::UI::Color{128, 16, 20, 30};
    return winrt::Windows::UI::Color{160, 14, 18, 26};
}

struct PuwGlassBrush : media::XamlCompositionBrushBaseT<PuwGlassBrush> {
    comp::Compositor m_compositor{nullptr};
    int m_level = 2;

    explicit PuwGlassBrush(comp::Compositor compositor, int level)
        : m_compositor(std::move(compositor)), m_level(level) {
        FallbackColor(FallbackColorForLevel(level));
    }

    void OnConnected() {
        try {
            if (!CompositionBrush() && m_compositor) {
                auto backdrop = m_compositor.CreateBackdropBrush();

                auto blur = winrt::make_self<GaussianBlurEffect>();
                blur->m_source = comp::CompositionEffectSourceParameter(L"Backdrop");
                auto tint = winrt::make_self<ColorTintEffect>(m_level);
                auto composite = winrt::make_self<CompositeEffect>();
                composite->m_src0 = blur.as<ge::IGraphicsEffectSource>();
                composite->m_src1 = tint.as<ge::IGraphicsEffectSource>();

                auto factory = m_compositor.CreateEffectFactory(composite.as<ge::IGraphicsEffect>());
                auto effectBrush = factory.CreateBrush();
                effectBrush.SetSourceParameter(L"Backdrop", backdrop);
                CompositionBrush(effectBrush);
            }
        } catch (...) {
        }
    }

    void OnDisconnected() {
        try {
            if (auto b = CompositionBrush()) {
                b.Close();
                CompositionBrush(nullptr);
            }
        } catch (...) {
        }
    }
};

static media::Brush MakePuwGlass(xaml::DependencyObject const& obj) {
    int level = ReadGlassConfig().level;
    try {
        if (auto el = obj.try_as<xaml::UIElement>()) {
            auto visual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(el);
            if (visual) {
                if (auto compositor = visual.Compositor()) {
                    return winrt::make<PuwGlassBrush>(compositor, level);
                }
            }
        }
    } catch (...) {
    }
    return media::SolidColorBrush(FallbackColorForLevel(level));
}

static media::SolidColorBrush MakePuwBorder() {
    // Delicate 15% white hairline glass edge
    return media::SolidColorBrush(winrt::Windows::UI::Color{38, 255, 255, 255});
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
                    winrt::box_value(xaml::Thickness{0.0, 4.0, 0.0, 4.0}));
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
                setProp(
                    xaml::FrameworkElement::MarginProperty(),
                    winrt::box_value(edge == TaskbarEdge::Top ? xaml::Thickness{0.0, 4.0, 0.0, 2.0} : xaml::Thickness{0.0, 2.0, 0.0, 4.0}));
            }
            return;
        }

        if (type == L"Taskbar.ExperienceToggleButton" && name == L"LaunchListButton") {
            if (!vertical) {
                setProp(
                    xaml::FrameworkElement::MarginProperty(),
                    winrt::box_value(startAligned ? xaml::Thickness{-7.0, 0.0, 0.0, 0.0} : xaml::Thickness{0.0, 0.0, 0.0, 0.0}));
            } else {
                setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(xaml::Thickness{0.0, 0.0, 0.0, 0.0}));
            }
            return;
        }

        if (name == L"SystemTrayFrameGrid") {
            xaml::Thickness margin = vertical
                ? xaml::Thickness{2.0, 0.0, 2.0, 8.0}
                : (edge == TaskbarEdge::Top ? xaml::Thickness{0.0, 4.0, 8.0, 2.0} : xaml::Thickness{0.0, 2.0, 8.0, 4.0});
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
            xaml::Thickness margin = vertical
                ? (edge == TaskbarEdge::Right ? xaml::Thickness{2.0, 8.0, 4.0, 0.0} : xaml::Thickness{4.0, 8.0, 2.0, 0.0})
                : (edge == TaskbarEdge::Top ? xaml::Thickness{8.0, 4.0, 0.0, 2.0} : xaml::Thickness{8.0, 2.0, 0.0, 4.0});
            xaml::Thickness pad = vertical
                ? xaml::Thickness{0.0, 4.0, 0.0, 4.0}
                : xaml::Thickness{6.0, 0.0, 6.0, 0.0};
            setProp(xaml::FrameworkElement::MarginProperty(), winrt::box_value(margin));
            if (obj.try_as<controls::Grid>()) {
                setProp(controls::Grid::PaddingProperty(), winrt::box_value(pad));
            }
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

        // 2. Floating dark-smoke glass dock pill on RootGrid inside TaskbarFrame
        if (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame") {
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
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
                            TaskbarEdge edge = GetTaskbarEdge();
                            if (edge == TaskbarEdge::Bottom || edge == TaskbarEdge::Top) {
                                double wantLeft = IsTaskbarAlignedStart() ? -7.0 : 0.0;
                                auto cur = winrt::unbox_value_or<xaml::Thickness>(
                                    sender.GetValue(dp), xaml::Thickness{wantLeft, 0.0, 0.0, 0.0});
                                if (cur.Left != wantLeft) {
                                    sender.SetValue(dp, winrt::box_value(xaml::Thickness{wantLeft, cur.Top, cur.Right, cur.Bottom}));
                                }
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

        // 4. Separate floating glass pill for the real Windows 11 System Tray (65% dark-smoke veil for crisp white text on pure white backgrounds)
        if (name == L"SystemTrayFrameGrid") {
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
            if (obj.try_as<controls::StackPanel>()) {
                SaveAndSet(obj, controls::StackPanel::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::StackPanel::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::StackPanel::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::StackPanel::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            } else if (obj.try_as<controls::Grid>()) {
                SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            }
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

        // 5. Matching glass background (12px blur + 65% dark-smoke veil) for tray overflow & taskbar '...' flyouts
        if (name == L"OverflowFlyoutBackgroundBorder") {
            SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            return;
        }

        if (type == L"Windows.UI.Xaml.Controls.FlyoutPresenter") {
            SaveAndSet(obj, controls::Control::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Control::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Control::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Control::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            SaveAndSet(obj, controls::Control::PaddingProperty(), winrt::box_value(xaml::Thickness{2.0, 4.0, 2.0, 4.0}));
            return;
        }

        // 5b. Adjust System Tray right-click MenuFlyoutPresenter's existing AcrylicBrush in-place (keeps native click hit-testing intact)
        if (type == L"Windows.UI.Xaml.Controls.MenuFlyoutPresenter") {
            int level = ReadGlassConfig().level;
            try {
                if (auto ctrl = obj.try_as<controls::Control>()) {
                    if (auto ac = ctrl.Background().try_as<media::AcrylicBrush>()) {
                        {
                            std::lock_guard<std::recursive_mutex> lock(m_mutex);
                            m_savedAcrylics.push_back(SavedAcrylic{
                                winrt::make_weak(ac),
                                ac.TintColor(),
                                ac.TintOpacity(),
                                ac.TintLuminosityOpacity(),
                            });
                        }
                        if (level == 0) {
                            ac.TintColor(winrt::Windows::UI::Color{255, 24, 30, 44});
                            ac.TintOpacity(0.18);
                            ac.TintLuminosityOpacity(0.22);
                        } else if (level == 1) {
                            ac.TintColor(winrt::Windows::UI::Color{255, 18, 24, 36});
                            ac.TintOpacity(0.40);
                            ac.TintLuminosityOpacity(0.42);
                        } else {
                            ac.TintColor(winrt::Windows::UI::Color{255, 14, 18, 26});
                            ac.TintOpacity(0.62);
                            ac.TintLuminosityOpacity(0.65);
                        }
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
            }
            return;
        }

        // 5e. Glass for Taskbar Jump Lists & Start Menu (individually toggleable; locked against VisualState resets)
        if (name == L"JumpListRestyledAcrylic"
            || name == L"SystemItemsAcrylic"
            || name == L"JumpListAcrylic"
            || (name == L"AcrylicBorder" && GetModuleHandleW(L"StartMenuExperienceHost.exe") != nullptr)) {
            uint32_t surfaces = ReadGlassConfig().surfaces;
            bool enabled = (name == L"AcrylicBorder")
                ? ((surfaces & SURF_START_MENU) != 0)
                : ((surfaces & SURF_JUMP_LIST) != 0);
            if (enabled) {
                SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
                LockGlassBackground(obj, controls::Border::BackgroundProperty());
                SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            }
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

        // 6. Matching floating glass pill for the Widgets button (visible only when Widgets is enabled in Windows)
        if (name == L"ExperienceToggleButtonRootPanel"
            && parentType == L"Taskbar.AugmentedEntryPointButton") {
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
            if (obj.try_as<controls::Grid>()) {
                SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            }
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

    void RestoreAllOnUiThread() {
        std::vector<SavedPropCallback> callbacks;
        std::vector<winrt::weak_ref<xaml::UIElement>> implicits;
        std::vector<SavedAcrylic> acrylics;
        std::vector<SavedProp> items;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            callbacks.swap(m_propCallbacks);
            implicits.swap(m_implicitTargets);
            acrylics.swap(m_savedAcrylics);
            items.swap(m_saved);
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
                    ApplyStyleIfMatch(obj, t.type, t.name, t.parentType);
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

            HANDLE handles[4]{};
            int stopIdx = -1, reloadIdx = -1, alignIdx = -1, posIdx = -1;
            DWORD count = 0;
            if (stopEv) { stopIdx = static_cast<int>(count); handles[count++] = stopEv; }
            if (reloadEv) { reloadIdx = static_cast<int>(count); handles[count++] = reloadEv; }
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
                        if (stuckKey && posEv) {
                            RegNotifyChangeKeyValue(stuckKey, FALSE, REG_NOTIFY_CHANGE_LAST_SET, posEv, TRUE);
                        }
                    }
                    if (wait == WAIT_TIMEOUT) {
                        TaskbarEdge cur = GetTaskbarEdge();
                        if (cur != lastEdge) {
                            lastEdge = cur;
                            DispatchSyncAlignment();
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
    std::vector<SavedAcrylic> m_savedAcrylics;
    std::vector<SavedPropCallback> m_propCallbacks;
    std::vector<winrt::weak_ref<xaml::UIElement>> m_implicitTargets;
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
