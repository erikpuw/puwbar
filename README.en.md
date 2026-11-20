# PuwBar

[Tiếng Việt](README.md)

PuwBar is a lightweight Windows 11 utility that styles the native taskbar into separate floating frosted glass pills for the app dock, System Tray, and Widgets button. Every native Windows 11 taskbar feature keeps working as usual, including animated tray icons (such as RunCat 365), native right-click menus, the overflow icon grid, Start menu, and window previews.

## How it works

PuwBar pairs a Rust controller with a small C++/WinRT TAP library (`puwbar_tap.dll`) embedded directly inside `puwbar.exe`. At launch, the binary extracts the DLL to `%TEMP%` and calls `InitializeXamlDiagnosticsEx` from `Windows.UI.Xaml.dll` to attach to the live XAML tree inside `explorer.exe`.

Because it modifies properties on the existing Windows 11 taskbar tree, PuwBar uses roughly 1.2 MB of RAM at idle, leaves Explorer running, and requires no third-party modding tools such as Windhawk. Exiting restores every modified XAML property immediately without restarting Explorer.

## Highlights (v1.0.1)

- Single standalone executable (`puwbar.exe`) with the TAP DLL and app icon embedded.
- Real-time frosted glass via `Windows.UI.Composition` (`CreateBackdropBrush` + speed-optimized `D2D1GaussianBlur`), 15% white hairline border, and 12px rounded corners.
- Follows all four taskbar positions (Bottom, Top, Left, Right) and Left/Center alignment from Windows Settings in real time.
- Keeps the active-app indicator aligned to the right edge on vertical right-docked taskbars.
- Applies matching glass styling to the System Tray pill, overflow flyouts (`^`, `...`), and Widgets button when enabled in Windows.
- Smooth 300ms horizontal slide synced with native app launch animations and locked to the monitor refresh rate via DWM VSync.
- System Tray menu with Vietnamese / English language switcher, About dialog, Reapply action, and immediate-logon Task Scheduler autostart (zero startup delay, falls back to `HKCU\...\Run` if UAC is declined).

## Usage

1. Download `puwbar.exe` from [Releases](https://github.com/erikpuw/puwbar/releases) or build from source.
2. Place `puwbar.exe` in a permanent folder and run it (no admin rights required for normal use).
3. Click the PuwBar icon in the System Tray to enable **Start with Windows** (prompts UAC once to register an immediate-logon Task Scheduler task), switch language, or open **About PuwBar**.
4. Press `Ctrl+Alt+F12` or pick **Exit PuwBar** from the tray menu to restore the default taskbar and quit.

> If you have a taskbar styling mod enabled in Windhawk (such as `Windows 11 Taskbar Styler`), disable it before running PuwBar so both do not overwrite the same XAML elements.

## Building from source

Requirements: Windows 11 (x64), Rust stable, MSVC Build Tools, and Windows 10/11 SDK (`10.0.26100.0` or newer with `xamlom.h` and C++/WinRT headers).

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The final single-file executable is written to `target\release\puwbar.exe`. `build.rs` automatically compiles `tap/puwbar_tap.cpp` and links `assets/puwbar.ico`.

## Release notes

See [docs/releases/v1.0.1.md](docs/releases/v1.0.1.md) for the v1.0.1 changelog.
