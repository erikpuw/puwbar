#![windows_subsystem = "windows"]

mod desktop;
mod diag;
mod gfx;
mod glass;
mod settings;
mod shell;
mod tap;
mod tray;
mod ui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--standalone" | "--preview" | "--guard" | "--diagnose"
        )
    }) {
        ui::run()
    } else {
        let seconds = args
            .windows(2)
            .find(|a| a[0] == "--test-seconds")
            .map(|a| a[1].parse::<u32>())
            .transpose()
            .map_err(|_| "Số giây không hợp lệ".to_string());
        seconds.and_then(tap::run)
    };
    if let Err(error) = result {
        desktop::show_error(&error);
        std::process::exit(1);
    }
}
