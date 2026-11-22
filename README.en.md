<p align="center">
  <img src="assets/puwbar.png" width="112" alt="PuwBar icon">
</p>

<h1 align="center">PuwBar</h1>

<p align="center"><b>Floating frosted glass taskbar for Windows 11, with every native feature intact.</b></p>

<p align="center">
  Splits the native taskbar into separate rounded glass pills for apps, the System Tray, and Widgets.<br>
  Written in Rust + C++/WinRT, single ~870 KB exe, no Windhawk, no Visual C++ Redistributable to install.
</p>

<p align="center"><a href="README.md">Tiếng Việt</a> · <b>English</b> · <a href="https://github.com/erikpuw/puwbar/releases">Latest: v1.0.5</a></p>

> **Download only from the author's source.** PuwBar is published by its author (erikpuw) only at [github.com/erikpuw/puwbar](https://github.com/erikpuw/puwbar). A copy from anywhere else (file-sharing sites, other repositories, repackaged builds, files sent over the internet) is not controlled by the author and comes with no guarantee: it may have been modified or had code added. Use only the build in this repository's [Releases](https://github.com/erikpuw/puwbar/releases).

## Features

| Feature | What it does |
|---|---|
| **Real frosted glass** | Real-time backdrop blur via `Windows.UI.Composition` (`CreateBackdropBrush` + `D2D1GaussianBlur`), 15% white hairline border, 12px rounded corners. |
| **Native behavior intact** | Never hides or replaces Explorer. Keeps live tray animations (RunCat 365, UniKey), native right-click menus, the overflow icon grid, Start menu, and window previews. |
| **All 4 edges & Left/Center** | Adapts live when switching taskbar position (**Bottom / Top / Left / Right**) or alignment (**Left / Center**) in Windows Settings. |
| **Right-edge indicator fix** | Keeps the running-app indicator locked to the right edge on vertical right-docked taskbars. |
| **Matching glass everywhere** | Styles the System Tray pill, Start Menu, tray right-click menus, taskbar Jump Lists, hover tooltips, window thumbnail previews, Quick Settings, Notification Center, Calendar, overflow flyouts (`^`, `...`), and Widgets button. |
| **Smooth horizontal slide** | 300ms ease-in-out slide timed with Windows 11's app launch animation, VSync-locked to the monitor refresh rate. |
| **Light & single file** | About 1.2 MB of RAM and 0% CPU at idle. The TAP DLL and icon are embedded inside `puwbar.exe`; exiting restores every XAML property cleanly. |
| **Tray menu** | Start with Windows, Glass contrast (24% / 45% / 62%), Glass surfaces (toggle each panel back to native Windows), Reapply taskbar glass, Language (Vietnamese / English), About, Exit (`Ctrl+Alt+F12`). |

**Start with Windows** asks for administrator permission once to create an immediate-logon Task Scheduler task (no startup delay, retries up to 30 seconds while Explorer XAML initializes). If UAC is declined, it falls back to `HKCU\...\Run`. Untick it to turn it off.

## Usage

1. Download `puwbar.exe` from [Releases](https://github.com/erikpuw/puwbar/releases), put it in a permanent folder, and run it.
2. Click the PuwBar icon in the System Tray to turn on **Start with Windows** or switch language.
3. Press `Ctrl+Alt+F12` or pick **Exit PuwBar** in the tray menu to restore the default taskbar without restarting Explorer.

If a taskbar styling mod is active in Windhawk (such as `Windows 11 Taskbar Styler`), disable it before running PuwBar so both do not overwrite the same XAML elements.

## Building from source

Requires Windows 11 (x64), Rust stable, MSVC Build Tools, and Windows SDK (`10.0.26100.0` or newer with `xamlom.h` and C++/WinRT).

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The standalone binary is written to `target\release\puwbar.exe`. `build.rs` compiles `tap/puwbar_tap.cpp` and embeds `assets/puwbar.ico` automatically.

## License

MIT — see [LICENSE](LICENSE).
