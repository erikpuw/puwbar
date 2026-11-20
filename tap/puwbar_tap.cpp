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
#include <winrt/Windows.Graphics.Effects.h>
#include <winrt/Windows.UI.h>
#include <winrt/Windows.UI.Composition.h>
#include <winrt/Windows.UI.Core.h>
#include <winrt/Windows.UI.Xaml.h>
#include <winrt/Windows.UI.Xaml.Controls.h>
#include <winrt/Windows.UI.Xaml.Hosting.h>
#include <winrt/Windows.UI.Xaml.Media.h>

#include <atomic>
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

// {7F4B2C91-3E8A-4D15-9C62-8A1F5E3B9D42}
static constexpr GUID CLSID_PuwBarTap = {
    0x7f4b2c91, 0x3e8a, 0x4d15, {0x9c, 0x62, 0x8a, 0x1f, 0x5e, 0x3b, 0x9d, 0x42}
};

static std::atomic<long> g_dllRefCount{0};

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
    // Premultiplied RGBA for airy 24% translucent dark-smoke frosted glass
    float m_color[4] = {0.02f, 0.025f, 0.04f, 0.24f};

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

struct PuwGlassBrush : media::XamlCompositionBrushBaseT<PuwGlassBrush> {
    comp::Compositor m_compositor{nullptr};

    explicit PuwGlassBrush(comp::Compositor compositor) : m_compositor(std::move(compositor)) {
        FallbackColor(winrt::Windows::UI::Color{90, 18, 22, 32});
    }

    void OnConnected() {
        try {
            if (!CompositionBrush() && m_compositor) {
                auto backdrop = m_compositor.CreateBackdropBrush();

                auto blur = winrt::make_self<GaussianBlurEffect>();
                blur->m_source = comp::CompositionEffectSourceParameter(L"Backdrop");
                auto tint = winrt::make_self<ColorTintEffect>();
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
    try {
        if (auto el = obj.try_as<xaml::UIElement>()) {
            auto visual = xaml::Hosting::ElementCompositionPreview::GetElementVisual(el);
            if (visual) {
                if (auto compositor = visual.Compositor()) {
                    return winrt::make<PuwGlassBrush>(compositor);
                }
            }
        }
    } catch (...) {
    }
    return media::SolidColorBrush(winrt::Windows::UI::Color{90, 18, 22, 32});
}

static media::SolidColorBrush MakePuwBorder() {
    // Delicate 15% white hairline glass edge
    return media::SolidColorBrush(winrt::Windows::UI::Color{38, 255, 255, 255});
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
            IXamlDiagnostics* diag = nullptr;
            {
                std::lock_guard<std::recursive_mutex> lock(m_mutex);
                m_meta[element.Handle] = meta;
                auto it = m_meta.find(relation.Parent);
                if (it != m_meta.end()) {
                    parentType = it->second.type;
                }
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
                if (IsInteresting(meta.type, meta.name, parentType)) {
                    {
                        std::lock_guard<std::recursive_mutex> lock(m_mutex);
                        m_tracked.push_back(TrackedElement{
                            winrt::make_weak(depObj),
                            meta.type,
                            meta.name,
                            parentType,
                        });
                    }
                    auto dispatcher = depObj.Dispatcher();
                    auto typeCopy = meta.type;
                    auto nameCopy = meta.name;
                    auto parentTypeCopy = parentType;
                    AddRef();
                    if (dispatcher && !dispatcher.HasThreadAccess()) {
                        dispatcher.RunAsync(
                            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
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

    static bool IsInteresting(
        std::wstring const& type,
        std::wstring const& name,
        std::wstring const& parentType) {
        return type == L"Taskbar.TaskbarFrame"
            || (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame")
            || type == L"Taskbar.TaskbarBackground"
            || name == L"BackgroundFill"
            || name == L"BackgroundStroke"
            || name == L"SystemTrayFrameGrid"
            || name == L"OverflowFlyoutBackgroundBorder";
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

    void ApplyStyleIfMatch(
        xaml::DependencyObject const& obj,
        std::wstring const& type,
        std::wstring const& name,
        std::wstring const& parentType) {
        // 1. Center floating dock frame
        if (type == L"Taskbar.TaskbarFrame") {
            SaveAndSet(obj, xaml::FrameworkElement::WidthProperty(), winrt::box_value(std::numeric_limits<double>::quiet_NaN()));
            SaveAndSet(obj, xaml::FrameworkElement::HorizontalAlignmentProperty(), winrt::box_value(xaml::HorizontalAlignment::Center));
            SaveAndSet(obj, xaml::FrameworkElement::MarginProperty(), winrt::box_value(xaml::Thickness{250.0, 0.0, 250.0, 0.0}));
            return;
        }

        // 2. Floating dark-smoke glass dock pill on RootGrid inside TaskbarFrame
        if (name == L"RootGrid" && parentType == L"Taskbar.TaskbarFrame") {
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            SaveAndSet(obj, controls::Grid::PaddingProperty(), winrt::box_value(xaml::Thickness{3.0, 0.0, 3.0, 0.0}));
            SaveAndSet(obj, xaml::FrameworkElement::MarginProperty(), winrt::box_value(xaml::Thickness{0.0, 2.0, 0.0, 4.0}));
            return;
        }

        // 3. Hide native full-width taskbar background & stroke
        if (type == L"Taskbar.TaskbarBackground" || name == L"BackgroundFill" || name == L"BackgroundStroke") {
            SaveAndSet(obj, xaml::UIElement::VisibilityProperty(), winrt::box_value(xaml::Visibility::Collapsed));
            return;
        }

        // 4. Separate floating glass pill for the real Windows 11 System Tray
        if (name == L"SystemTrayFrameGrid") {
            SaveAndSet(obj, controls::Panel::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, xaml::FrameworkElement::MarginProperty(), winrt::box_value(xaml::Thickness{0.0, 2.0, 8.0, 4.0}));
            if (obj.try_as<controls::StackPanel>()) {
                SaveAndSet(obj, controls::StackPanel::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::StackPanel::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::StackPanel::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::StackPanel::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
                SaveAndSet(obj, controls::StackPanel::PaddingProperty(), winrt::box_value(xaml::Thickness{6.0, 0.0, 6.0, 0.0}));
            } else if (obj.try_as<controls::Grid>()) {
                SaveAndSet(obj, controls::Grid::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
                SaveAndSet(obj, controls::Grid::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
                SaveAndSet(obj, controls::Grid::BorderBrushProperty(), MakePuwBorder());
                SaveAndSet(obj, controls::Grid::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
                SaveAndSet(obj, controls::Grid::PaddingProperty(), winrt::box_value(xaml::Thickness{6.0, 0.0, 6.0, 0.0}));
            }
            return;
        }

        // 5. Matching glass background for the native tray overflow icon grid
        if (name == L"OverflowFlyoutBackgroundBorder") {
            SaveAndSet(obj, controls::Border::BackgroundProperty(), MakePuwGlass(obj));
            SaveAndSet(obj, controls::Border::BackgroundSizingProperty(), winrt::box_value(controls::BackgroundSizing::InnerBorderEdge));
            SaveAndSet(obj, controls::Border::CornerRadiusProperty(), winrt::box_value(xaml::CornerRadius{12.0, 12.0, 12.0, 12.0}));
            SaveAndSet(obj, controls::Border::BorderBrushProperty(), MakePuwBorder());
            SaveAndSet(obj, controls::Border::BorderThicknessProperty(), winrt::box_value(xaml::Thickness{1.0, 1.0, 1.0, 1.0}));
            return;
        }
    }

    void RestoreAllOnUiThread() {
        std::vector<SavedProp> items;
        {
            std::lock_guard<std::recursive_mutex> lock(m_mutex);
            items.swap(m_saved);
        }
        for (auto it = items.rbegin(); it != items.rend(); ++it) {
            try {
                if (auto obj = it->target.get()) {
                    if (it->wasUnset) {
                        obj.ClearValue(it->prop);
                    } else if (it->original) {
                        obj.SetValue(it->prop, it->original);
                    }
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

    void StartWatcher() {
        StopWatcher();
        m_stopWatcher.store(false);
        AddRef();
        m_watcher = std::thread([this]() {
            HANDLE stopEv = OpenEventW(SYNCHRONIZE, FALSE, L"Local\\PuwBar.StopEvent");
            HANDLE reloadEv = OpenEventW(SYNCHRONIZE, FALSE, L"Local\\PuwBar.ReloadEvent");
            HANDLE handles[2]{};
            DWORD count = 0;
            if (stopEv) handles[count++] = stopEv;
            if (reloadEv) handles[count++] = reloadEv;

            if (count > 0) {
                while (!m_stopWatcher.load()) {
                    DWORD wait = WaitForMultipleObjects(count, handles, FALSE, 250);
                    if (stopEv && wait == WAIT_OBJECT_0) {
                        RequestRestoreAndUnadvise();
                        break;
                    }
                    if (reloadEv && ((stopEv && wait == WAIT_OBJECT_0 + 1) || (!stopEv && wait == WAIT_OBJECT_0))) {
                        DispatchReload();
                    }
                }
            }
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

BOOL APIENTRY DllMain(HMODULE, DWORD, LPVOID) {
    return TRUE;
}
