# PuwBar

[English](README.en.md)

PuwBar là công cụ gọn nhẹ cho Windows 11, biến thanh taskbar gốc thành các cụm kính mờ nổi bo góc riêng biệt (dock ứng dụng, cụm System Tray và nút Widgets khi bật). Toàn bộ chức năng gốc của taskbar Windows 11 được giữ nguyên, bao gồm animation của ứng dụng tray (như RunCat 365), menu chuột phải gốc, lưới icon overflow, Start menu và xem trước cửa sổ.

## Cách hoạt động

PuwBar gồm phần điều khiển viết bằng Rust và một thư viện TAP nhỏ (`puwbar_tap.dll`, C++/WinRT) được nhúng sẵn ngay trong `puwbar.exe`. Khi chạy, chương trình tự giải nén DLL vào `%TEMP%` và dùng API `InitializeXamlDiagnosticsEx` của `Windows.UI.Xaml.dll` để gắn trực tiếp vào cây XAML của `explorer.exe`.

Vì chỉ chỉnh thuộc tính trên cây XAML hiện có của Windows 11, PuwBar tốn khoảng 1,2 MB RAM khi chạy nền, không cần tắt Explorer và không phụ thuộc Windhawk hay phần mềm ngoài. Khi thoát, mọi thuộc tính đã đổi được trả lại nguyên bản mà không cần khởi động lại Explorer.

## Tính năng chính (v1.0.1)

- Một file thực thi duy nhất `puwbar.exe`, không cần chép kèm DLL hay runtime ngoài.
- Kính mờ thời gian thực bằng `Windows.UI.Composition` (`CreateBackdropBrush` + `D2D1GaussianBlur` tối ưu tốc độ), viền sáng mảnh 15% và bo góc 12px.
- Tự nhận diện cả 4 vị trí thanh taskbar (Bottom, Top, Left, Right) cùng căn lề Left/Center trong Windows Settings theo thời gian thực.
- Khóa đúng cạnh vạch đánh dấu ứng dụng đang chạy khi đặt taskbar dọc bên phải.
- Bọc kính đồng bộ cho cụm System Tray, popup overflow (`^`, `...`) và nút Widgets (chỉ hiện khi Widgets được bật trong Windows).
- Hiệu ứng trượt ngang 300ms khớp nhịp mở/đóng ứng dụng, khóa VSync theo tần số quét màn hình.
- Menu chuột phải ở icon System Tray hỗ trợ chuyển ngôn ngữ Tiếng Việt / English, hộp thoại Giới thiệu (About), áp dụng lại giao diện và bật/tắt tự khởi động cùng Windows qua Task Scheduler (chạy sớm ngay lúc đăng nhập, không trễ).

## Cài đặt và sử dụng

1. Tải `puwbar.exe` từ mục [Releases](https://github.com/erikpuw/puwbar/releases) hoặc tự build từ mã nguồn.
2. Chép `puwbar.exe` vào thư mục cố định và chạy trực tiếp (không cần quyền quản trị).
3. Bấm vào icon PuwBar ở System Tray để bật **Khởi động cùng Windows** (sẽ hỏi UAC một lần để tạo tác vụ Task Scheduler chạy sớm; nếu từ chối sẽ tự chuyển sang khóa `HKCU\...\Run`), đổi ngôn ngữ hoặc xem thông tin phiên bản.
4. Nhấn `Ctrl+Alt+F12` hoặc chọn **Thoát PuwBar** trong menu tray để thoát và trả lại taskbar gốc.

> Nếu máy đang bật mod chỉnh giao diện taskbar trong Windhawk (như `Windows 11 Taskbar Styler`), nên tắt mod đó để tránh hai bên cùng ghi đè thuộc tính XAML.

## Build từ mã nguồn

Yêu cầu: Windows 11 (x64), Rust stable, MSVC Build Tools và Windows 10/11 SDK (`10.0.26100.0` hoặc mới hơn có `xamlom.h` và C++/WinRT).

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

File đầu ra nằm tại `target\release\puwbar.exe`. `build.rs` tự biên dịch `tap/puwbar_tap.cpp` và nhúng kèm `assets/puwbar.ico` vào file `.exe`.

## Ghi chú phát hành

Xem chi tiết các thay đổi tại mục [Releases](https://github.com/erikpuw/puwbar/releases).
