#![windows_subsystem = "windows"]

mod desktop;
mod shell;
mod tray;
mod ui;

fn main() {
    if let Err(error) = ui::run() {
        desktop::show_error(&error);
        std::process::exit(1);
    }
}
