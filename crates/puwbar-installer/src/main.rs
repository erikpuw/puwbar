#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod install;
mod shortcut;

use autostart::{wide, AutostartOutcome};
use eframe::egui;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke, Vec2};
use install::{
    default_install_dir, launch_installed_app, perform_install, perform_uninstall, InstallOptions,
    APP_NAME, APP_VERSION,
};
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::Arc;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ready,
    Done,
    Failed,
}

struct InstallerApp {
    mode: Mode,
    is_vi: bool,
    dark: bool,
    install_dir_text: String,
    create_start_menu: bool,
    create_desktop: bool,
    enable_startup_task: bool,
    launch_after_install: bool,
    status: Status,
    message: String,
    installed_exe: Option<PathBuf>,
}

fn detect_windows_dark_theme() -> bool {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut light = 0u32;
    let mut bytes = size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut light).cast(),
            &mut bytes,
        ) == 0
    };
    if ok {
        light == 0
    } else {
        true
    }
}

impl InstallerApp {
    fn new(cc: &eframe::CreationContext<'_>, mode: Mode) -> Self {
        configure_fonts(&cc.egui_ctx);
        let dark = detect_windows_dark_theme();
        apply_visuals(&cc.egui_ctx, dark);
        Self {
            mode,
            is_vi: true,
            dark,
            install_dir_text: default_install_dir().display().to_string(),
            create_start_menu: true,
            create_desktop: false,
            enable_startup_task: true,
            launch_after_install: true,
            status: Status::Ready,
            message: String::new(),
            installed_exe: None,
        }
    }
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let accent = Color32::from_rgb(0x3B, 0x82, 0xF6);
        let ok_color = Color32::from_rgb(0x22, 0xC5, 0x5E);
        let err_color = Color32::from_rgb(0xEF, 0x44, 0x44);
        let warn_bg = if self.dark {
            Color32::from_rgb(0x1E, 0x29, 0x3B)
        } else {
            Color32::from_rgb(0xEF, 0xF6, 0xFF)
        };
        let warn_stroke = if self.dark {
            Color32::from_rgb(0x3B, 0x82, 0xF6)
        } else {
            Color32::from_rgb(0x93, 0xC5, 0xFD)
        };
        let muted = if self.dark {
            Color32::from_gray(165)
        } else {
            Color32::from_gray(95)
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(20.0))
            .show(ctx, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let title = match (self.mode, self.is_vi) {
                            (Mode::Install, true) => format!("Cài đặt {APP_NAME} v{APP_VERSION}"),
                            (Mode::Install, false) => format!("Install {APP_NAME} v{APP_VERSION}"),
                            (Mode::Uninstall, true) => {
                                format!("Gỡ cài đặt {APP_NAME} v{APP_VERSION}")
                            }
                            (Mode::Uninstall, false) => {
                                format!("Uninstall {APP_NAME} v{APP_VERSION}")
                            }
                        };
                        ui.label(RichText::new(title).size(19.0).strong());
                        let subtitle = if self.is_vi {
                            "Thanh taskbar kính mờ nổi cho Windows 11 — cài đặt cố định vào máy"
                        } else {
                            "Floating frosted glass taskbar for Windows 11 — per-user installation"
                        };
                        ui.label(RichText::new(subtitle).size(12.0).color(muted));
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        let lang_label = if self.is_vi { "EN" } else { "VI" };
                        if ui.button(lang_label).clicked() {
                            self.is_vi = !self.is_vi;
                        }
                    });
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                match self.mode {
                    Mode::Install => {
                        let dir_label = if self.is_vi {
                            "Thư mục cài đặt:"
                        } else {
                            "Install location:"
                        };
                        ui.label(RichText::new(dir_label).strong());
                        ui.add_space(4.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.install_dir_text)
                                .desired_width(f32::INFINITY)
                                .interactive(self.status != Status::Done),
                        );
                        ui.add_space(10.0);

                        let sm_txt = if self.is_vi {
                            "Tạo shortcut trong Start Menu"
                        } else {
                            "Create Start Menu shortcut"
                        };
                        let dt_txt = if self.is_vi {
                            "Tạo shortcut ngoài Desktop"
                        } else {
                            "Create Desktop shortcut"
                        };
                        let st_txt = if self.is_vi {
                            "Chạy cùng Windows khi đăng nhập (xin quyền Quản trị viên UAC)"
                        } else {
                            "Start with Windows at logon (requests Administrator UAC)"
                        };
                        let launch_txt = if self.is_vi {
                            "Mở PuwBar ngay sau khi cài xong"
                        } else {
                            "Launch PuwBar after installation"
                        };

                        ui.add_enabled_ui(self.status != Status::Done, |ui| {
                            ui.checkbox(&mut self.create_start_menu, sm_txt);
                            ui.checkbox(&mut self.create_desktop, dt_txt);
                            ui.checkbox(&mut self.enable_startup_task, st_txt);

                            if self.enable_startup_task {
                                ui.add_space(4.0);
                                egui::Frame::new()
                                    .fill(warn_bg)
                                    .stroke(Stroke::new(1.0_f32, warn_stroke))
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        let uac_note = if self.is_vi {
                                            "Windows sẽ hiện hộp thoại UAC xin quyền Quản trị viên để đăng ký tác vụ khởi động tức thì (Task Scheduler, độ trễ 0 giây). Nếu từ chối UAC, PuwBar sẽ tự chuyển sang khởi động thường (Registry Run)."
                                        } else {
                                            "Windows will show a UAC prompt to register a zero-delay Task Scheduler logon task (HighestAvailable). If declined, PuwBar falls back to standard Registry Run."
                                        };
                                        ui.label(
                                            RichText::new(uac_note)
                                                .size(11.5)
                                                .color(if self.dark {
                                                    Color32::from_rgb(0xBF, 0xDB, 0xFE)
                                                } else {
                                                    Color32::from_rgb(0x1E, 0x3A, 0x8A)
                                                }),
                                        );
                                    });
                                ui.add_space(4.0);
                            }

                            ui.checkbox(&mut self.launch_after_install, launch_txt);
                        });
                    }
                    Mode::Uninstall => {
                        let desc = if self.is_vi {
                            format!(
                                "Gỡ cài đặt sẽ dừng PuwBar (khôi phục taskbar gốc Windows 11), xóa tác vụ khởi động cùng Windows (có thể hỏi UAC), xóa shortcut và thư mục:\n{}",
                                self.install_dir_text
                            )
                        } else {
                            format!(
                                "Uninstalling will stop PuwBar (restoring the native Windows 11 taskbar), remove startup tasks (may prompt UAC), shortcuts, and:\n{}",
                                self.install_dir_text
                            )
                        };
                        ui.label(desc);
                    }
                }

                ui.add_space(10.0);

                if !self.message.is_empty() {
                    let color = match self.status {
                        Status::Done => ok_color,
                        Status::Failed => err_color,
                        Status::Ready => accent,
                    };
                    ui.label(RichText::new(&self.message).color(color).strong());
                }

                // Bottom action bar
                ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                    ui.horizontal(|ui| {
                        if self.status == Status::Done {
                            let finish_txt = if self.is_vi { "Hoàn tất" } else { "Finish" };
                            if ui
                                .add_sized([110.0, 32.0], egui::Button::new(finish_txt))
                                .clicked()
                            {
                                if self.mode == Mode::Install && self.launch_after_install {
                                    if let Some(ref exe) = self.installed_exe {
                                        launch_installed_app(exe);
                                    }
                                }
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        } else {
                            let primary_txt = match (self.mode, self.is_vi) {
                                (Mode::Install, true) => "Cài đặt",
                                (Mode::Install, false) => "Install",
                                (Mode::Uninstall, true) => "Gỡ cài đặt",
                                (Mode::Uninstall, false) => "Uninstall",
                            };
                            if ui
                                .add_sized([120.0, 32.0], egui::Button::new(primary_txt))
                                .clicked()
                            {
                                match self.mode {
                                    Mode::Install => {
                                        let opts = InstallOptions {
                                            install_dir: PathBuf::from(
                                                self.install_dir_text.trim(),
                                            ),
                                            create_start_menu: self.create_start_menu,
                                            create_desktop: self.create_desktop,
                                            enable_startup_task: self.enable_startup_task,
                                        };
                                        match perform_install(&opts) {
                                            Ok(res) => {
                                                self.installed_exe = Some(res.exe_path);
                                                self.status = Status::Done;
                                                self.message = match (res.autostart, self.is_vi) {
                                                    (AutostartOutcome::TaskSchedulerElevated, true) => format!(
                                                        "Đã cài đặt {APP_NAME} v{APP_VERSION}! Đã bật khởi động tức thì (Task Scheduler + UAC)."
                                                    ),
                                                    (AutostartOutcome::TaskSchedulerElevated, false) => format!(
                                                        "{APP_NAME} v{APP_VERSION} installed! Zero-delay Task Scheduler startup enabled."
                                                    ),
                                                    (AutostartOutcome::RegistryRunFallback, true) => format!(
                                                        "Đã cài đặt {APP_NAME} v{APP_VERSION}! (Từ chối UAC — đã dùng khởi động thường Registry Run.)"
                                                    ),
                                                    (AutostartOutcome::RegistryRunFallback, false) => format!(
                                                        "{APP_NAME} v{APP_VERSION} installed! (UAC declined — fell back to Registry Run.)"
                                                    ),
                                                    (AutostartOutcome::Disabled, true) => format!(
                                                        "Đã cài đặt thành công {APP_NAME} v{APP_VERSION}!"
                                                    ),
                                                    (AutostartOutcome::Disabled, false) => format!(
                                                        "{APP_NAME} v{APP_VERSION} installed successfully!"
                                                    ),
                                                };
                                            }
                                            Err(err) => {
                                                self.status = Status::Failed;
                                                self.message = err;
                                            }
                                        }
                                    }
                                    Mode::Uninstall => {
                                        let dir = PathBuf::from(self.install_dir_text.trim());
                                        match perform_uninstall(&dir) {
                                            Ok(()) => {
                                                self.status = Status::Done;
                                                self.message = if self.is_vi {
                                                    "Đã gỡ cài đặt PuwBar và khôi phục taskbar gốc."
                                                        .into()
                                                } else {
                                                    "PuwBar has been uninstalled and the native taskbar restored.".into()
                                                };
                                            }
                                            Err(err) => {
                                                self.status = Status::Failed;
                                                self.message = err;
                                            }
                                        }
                                    }
                                }
                            }

                            let cancel_txt = if self.is_vi { "Đóng" } else { "Cancel" };
                            if ui
                                .add_sized([90.0, 32.0], egui::Button::new(cancel_txt))
                                .clicked()
                            {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    });
                });
            });
    }
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoeui".to_owned(), Arc::new(FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "segoeui".to_owned());
        ctx.set_fonts(fonts);
    }
}

fn apply_visuals(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    ctx.set_visuals(visuals);
}

fn load_window_icon() -> egui::IconData {
    const ICON_RGBA: &[u8] = include_bytes!("../../../assets/puwbar-icon.rgba");
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: 64,
        height: 64,
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let self_is_uninstaller = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()))
        .map(|n| n.contains("uninstall"))
        .unwrap_or(false);

    if args.iter().any(|a| a == "--silent-install" || a == "/S") {
        let opts = InstallOptions {
            install_dir: default_install_dir(),
            create_start_menu: true,
            create_desktop: false,
            enable_startup_task: true,
        };
        let _ = perform_install(&opts);
        return Ok(());
    }
    if args.iter().any(|a| a == "--silent-uninstall") {
        let dir = default_install_dir();
        let _ = perform_uninstall(&dir);
        return Ok(());
    }

    let mode = if self_is_uninstaller || args.iter().any(|a| a == "--uninstall") {
        Mode::Uninstall
    } else {
        Mode::Install
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{APP_NAME} v{APP_VERSION} Setup"))
            .with_icon(Arc::new(load_window_icon()))
            .with_inner_size(Vec2::new(540.0, 425.0))
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "PuwBar Setup",
        options,
        Box::new(move |cc| Ok(Box::new(InstallerApp::new(cc, mode)))),
    )
}
