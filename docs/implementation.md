# PuwBar 0.1

## Thiết kế đã thống nhất

Rust native, dock kính khói bo mềm, có Start và cụm trạng thái riêng. Dock cao 48 DIP,
cách đáy 4 DIP sau phản hồi chạy thật, icon 28 DIP. Tỷ lệ tăng theo DPI. Gom cửa sổ theo ứng dụng, phần dư
vào menu, luôn chừa chỗ cho cụm trạng thái. Taskbar Windows phải ẩn khi PuwBar hoạt động.

## Kế hoạch thực hiện trong phiên

1. Kiểm tra môi trường và test ban đầu. Workspace trống, chưa có Cargo.toml; WinHawk đang chạy.
2. Viết test cho bố trí ở nhiều kích thước/DPI, gom nhóm và dữ liệu khôi phục. Kiểm tra thất bại trước khi triển khai.
3. Viết Win32 dock, menu cửa sổ, icon thật, nút Start, trạng thái và tiến trình giám sát.
4. Build, kiểm thử ẩn/khôi phục trên máy, cả khi kết thúc tiến trình bất thường. Giữ nguyên cấu hình WinHawk.
5. Kiểm tra graph, dọn phần không dùng, cập nhật README và note phát hành, commit cục bộ.

## Bất biến

- Chỉ ẩn taskbar sau khi giao diện và tiến trình giám sát đã sẵn sàng.
- Không tắt Explorer, không chèn mã vào Explorer, không thay cấu hình WinHawk.
- Tiến trình giám sát giữ handle tiến trình chính và khôi phục khi tiến trình chính kết thúc.
- Khôi phục trạng thái hiển thị và vùng làm việc ban đầu khi thoát.
- Dock không được đè lên cụm trạng thái; không thu icon để nhét thêm ứng dụng.
- Không chạy hai phiên PuwBar đồng thời.
- Các phần chưa có bằng chứng chạy thật phải ghi rõ trong README.

## Kiểm chứng

`rtk cargo test`, `rtk cargo clippy --all-targets -- -D warnings`,
`rtk cargo build --release`, kiểm tra thực tế Start, chọn cửa sổ,
thoát bình thường và crash recovery. Chỉ chạy với thời gian hữu hạn trong phiên kiểm thử.

## Phạm vi cần xác minh

Windows không cung cấp API công khai hoàn chỉnh để một ứng dụng ngoài Explorer nhận
toàn bộ notification icon của ứng dụng khác. Không được giả các icon tĩnh là tray thật.
Sẽ ghi rõ cách truy cập tray gốc và giới hạn trước khi bàn giao.

## Nhật ký

- Kiểm tra graph ban đầu: GitNexus không có repo puwbar; workspace không có mã nguồn hoặc caller hiện hữu.
- Rust ở E:/.rustup, Cargo cache ở E:/.cargo. context-mode cần truyền hai biến môi trường này.
- Bố trí: test đỏ trước triển khai, sau đó 6 test model qua; tổng 8 test khi thêm dữ liệu khôi phục.
- Release cuối: 450.048 byte; Preview, Normal, Crash đều qua kiểm tra desktop. Tiến trình chính khoảng 20–21 MB working set trong mẫu ngắn.
- Ruling: dùng GDI và trong suốt nhẹ trước, chưa triển khai blur. Notification tray của ứng dụng khác chưa thể coi là đã thay thế; ghi rõ trong README và release note.
- Lượt inspect từ shell bị hạn chế không tìm thấy đủ dock. Chạy lại qua môi trường desktop của context-mode có 2 panel và 17 nút; tọa độ y=1030..1078 trên màn 1920×1080, đúng khoảng cách 2 px.
- Công cụ CUA loại cửa sổ tool window khỏi danh sách nên chưa xác minh các thao tác bấm bằng CUA. Không coi kiểm tra sự tồn tại của nút là bằng chứng tương tác đã qua.
- Rà soát độc lập bằng model tiết kiệm: không có lỗi chắc chắn trong các đường restore, callback RefCell và menu đã xem; chưa bao phủ toàn bộ GDI và nhóm ứng dụng.
- Kiểm tra cuối: fmt, 8 test, Clippy và release build qua; Normal và Crash chạy lại đều qua (20,50 và 20,55 MB RAM cho tiến trình chính). PE import không chứa vcruntime, msvcp hoặc ucrtbase; clean Windows vẫn chưa thử.
- Graph: toàn bộ 15 tệp được stage. `detect-changes --scope all` không chạy được vì repo mới chưa có HEAD; dùng scope staged để phân tích toàn bộ thay đổi ban đầu. Graph báo risk high cho 145 symbol/12 flow của ứng dụng mới. Index có giới hạn coverage flow nên không coi các luồng vắng mặt là an toàn.

## Phản hồi về icon và tray, 2026-11-17

- Người dùng chỉ ra chữ cái thay icon, sai ứng dụng ghim, khoảng đáy quá sát và tray thiếu. Đã tái hiện test đỏ cho Shell nhận đường dẫn `/`, chuẩn hóa identity và thứ tự ghim; sửa xong các test qua.
- Nguyên nhân icon: filesystem Rust nhận `/`, Shell không nhận cùng đường dẫn đó. Đã chuẩn hóa sang `\`, lấy icon từ ứng dụng đích và bỏ nhánh hiển thị chữ cái. File Explorer namespace shortcut cần nhận đúng exe để không tách thành nhóm khác.
- UI Automation taskbar trên máy chỉ trả hai node. Dừng dùng hướng này và bỏ mã điều tra. Taskband Favorites chứa tên shortcut theo vị trí; chỉ đọc và giữ fallback cho shortcut không tìm thấy. Đã xác nhận thứ tự Edge, Explorer, Notepad++, VS Code và các ứng dụng còn lại.
- Đối chiếu mod Windows 11 Taskbar Styler 1.11.1 đang cài: sửa XAML của taskbar thật, dùng AcrylicBrush/WindhawkBlur và CornerRadius. Mod đang Disabled=1; không thay đổi cấu hình hoặc nguồn mod.
- DWM Acrylic được áp dụng cho cửa sổ PuwBar, lớp GDI 32-bit sửa alpha để nội dung còn đọc được. Preview có cửa sổ ứng dụng thông thường để CUA thấy; đã quan sát icon thật, sau đó bỏ mũi tên shortcut. Khoảng đáy 4 DIP. Test Normal/Crash qua, RAM chính 24,45/24,53 MB.
- Notification tray đầy đủ vẫn chưa có. Chuyển sang sửa taskbar gốc như WinHawk là thay đổi kiến trúc, mâu thuẫn yêu cầu cũ ẩn hoàn toàn taskbar gốc; cần người dùng chốt trước khi can thiệp Explorer.
