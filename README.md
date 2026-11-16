# PuwBar

Thanh ứng dụng native cho Windows, viết bằng Rust. Dock tối, bo góc mềm, cao 48 DIP
và cách đáy màn hình 4 DIP. Cụm trạng thái nằm riêng bên phải.

Đây là bản thử nghiệm 0.1. PuwBar đã ẩn và khôi phục taskbar gốc trong các lượt thử
trên máy phát triển, kể cả khi tiến trình chính bị buộc dừng. Chưa thay thế đầy đủ
mọi chức năng của taskbar Windows.

## Chạy

Mở `target/release/puwbar.exe` sau khi build. Chương trình không yêu cầu quyền quản trị.
Để xem dock mà giữ taskbar Windows nguyên trạng:

```powershell
.\target\release\puwbar.exe --preview
```

Nhấn **Ctrl+Alt+F12** hoặc bấm chuột phải lên dock và chọn **Thoát PuwBar** để thoát.
Taskbar gốc cùng vùng làm việc được trả lại. Không bật tự khởi động trong bản này.

Nếu đang dùng WinHawk, nên tạm tắt các mod thay đổi taskbar trước khi dùng hằng ngày.
PuwBar không sửa cấu hình, tắt dịch vụ hoặc gỡ WinHawk. Các lượt thử ngắn trong phiên
phát triển được chạy khi WinHawk vẫn hoạt động.

## Cách dùng

- Start mở menu Start của Windows.
- Ứng dụng ghim được đọc từ shortcut taskbar Windows khi khởi động. Các thay đổi
  ghim trên Windows được nhận ở lần mở PuwBar tiếp theo. Thứ tự lấy từ dữ liệu
  `Taskband\Favorites` của Explorer; nếu định dạng thay đổi, các shortcut không tìm
  thấy vẫn được giữ ở cuối danh sách. PuwBar chỉ đọc dữ liệu này.
- Bấm ứng dụng chưa chạy để mở. Bấm ứng dụng có một cửa sổ để chuyển tới hoặc thu nhỏ.
  Nếu có nhiều cửa sổ, menu cho phép chọn từng cửa sổ.
- Khi hết chiều ngang, các ứng dụng dư vào nút `…`. Dock chừa ít nhất 16 DIP trước
  cụm trạng thái. Kích thước icon không bị thu nhỏ để nhét thêm ứng dụng.
- Nút mạng, âm thanh, pin và đồng hồ mở trang cài đặt tương ứng. Phần trăm pin và giờ
  lấy từ hệ thống. Trên máy không có pin, hiển thị `AC`.

**Ctrl+Alt+Space** đưa focus vào Start. Dùng Tab và Shift+Tab để đi qua các nút,
Space để bấm. Nút có tên truy cập và tooltip qua control Win32; khả năng đọc bằng
screen reader chưa được kiểm tra thực tế.

## Taskbar gốc và khôi phục

PuwBar lưu trạng thái taskbar và vùng làm việc trước khi thay đổi. Tiến trình giám sát
khởi động và phản hồi sẵn sàng trước khi thanh gốc bị ẩn. Explorer vẫn chạy.
Mỗi màn hình được tạo một dock và cụm trạng thái; AppBar dành vùng dưới cho chúng.

Khi tiến trình chính kết thúc bất thường, tiến trình giám sát dùng handle tiến trình
để phát hiện và trả lại trạng thái ban đầu. Khi thoát bình thường, PuwBar chờ bộ
khôi phục kết thúc trước khi nhả khóa phiên. Không được kết thúc cả cây tiến trình:
làm vậy sẽ tắt cả bộ khôi phục.

PuwBar theo dõi thông báo tạo lại taskbar và sự kiện hiển thị, kèm kiểm tra mỗi giây.
Ẩn cửa sổ taskbar Explorer là cách tích hợp cần kiểm thử theo bản Windows; chưa có
bằng chứng rằng thanh gốc không lóe lên trong mọi tình huống khởi động lại Explorer.

## Những phần chưa có

Notification tray của ứng dụng khác như UniKey, Discord hoặc ứng dụng chạy nền chưa
được đưa vào PuwBar. Cụm trạng thái hiện tại gồm các nút hệ thống, không phải bản sao
đầy đủ của tray gốc. Khi cần các icon đó, thoát PuwBar để dùng taskbar Windows.

Chưa có xem trước cửa sổ, kéo thả, chỉnh ghim trong PuwBar, quick settings riêng,
Start menu riêng hoặc tự khởi động. Đổi cấu hình màn hình/DPI khi đang chạy sẽ cho
PuwBar thoát và khôi phục taskbar; mở lại để nhận bố cục mới.

Nền dùng Acrylic của DWM trên Windows 11 có hỗ trợ system backdrop, với lớp vẽ GDI
32-bit giữ alpha cho nền kính và icon. Khi DWM không hỗ trợ, dùng nền tối đặc.
Màu Acrylic phụ thuộc thiết lập và nền màn hình; ảnh concept không phải ảnh chụp bản chạy.

## Build và kiểm tra

Cần Rust stable và MSVC Build Tools trên máy build. Các thư viện Rust được khai báo
trong `Cargo.toml` và khóa ở `Cargo.lock`; không có runtime Node.js hoặc Python.
Build MSVC x64 dùng static CRT. Chưa thử trên Windows sạch để xác nhận tính portable.

```powershell
rtk cargo test --offline
rtk cargo clippy --offline --all-targets -- -D warnings
rtk cargo build --offline --release
```

Kiểm thử desktop sẽ tạm ẩn taskbar rồi trả lại. Chạy từng chế độ khi PuwBar đang tắt:

```powershell
rtk proxy powershell -NoProfile -File scripts/Test-Desktop.ps1 -Mode Preview
rtk proxy powershell -NoProfile -File scripts/Test-Desktop.ps1 -Mode Normal
rtk proxy powershell -NoProfile -File scripts/Test-Desktop.ps1 -Mode Crash
```

Sau khi sửa lỗi icon: 11 unit test qua. Test đọc taskbar thật được đánh dấu ignored
để không phụ thuộc cấu hình máy; đã chạy riêng và xác nhận 11 ứng dụng ghim có icon.
Các lượt Normal và Crash chạy lại qua trên màn hình 1920×1080, scale 100%.
Tiến trình chính dùng khoảng 24,5 MB working set trong các mẫu ngắn, chưa cộng guard.
Chưa có benchmark tải dài hoặc kiểm thử nhiều màn hình thật.

Preview là cửa sổ ứng dụng thông thường để công cụ desktop có thể xem hình và các nút;
chế độ thật vẫn dùng tool window. Đã quan sát icon thật trong preview, chưa xác minh
đầy đủ thao tác Start/menu. Preview luôn hiện để có thể kiểm tra ngay cả khi một cửa
sổ khác đang phủ toàn màn hình.

`--diagnose` ghi thông tin màn hình và snapshot vào tệp `.diagnostics.txt` cạnh exe.
`--test-seconds N` tự thoát sau N giây để phục vụ kiểm thử có thời hạn.
