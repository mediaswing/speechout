// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod document;
mod logging;
mod package;
mod platform;
mod speech;
mod store;
mod table;
mod transcribe;
mod ui;
mod update;
mod vision;
mod wordlist;
mod worker;

use std::process::ExitCode;

/// The app's name, as the window, the installers and the README give it.
pub const APP_TITLE: &str = "Common Speech Output";

/// The executable's name, as installed and typed: `speechout`, from the
/// `[[bin]]` in Cargo.toml. It is also this crate's name, so log targets start with it.
pub const BIN_NAME: &str = env!("CARGO_CRATE_NAME");

const USAGE: &str = "Common Speech Output

Usage:
  speechout                     Start the application
  speechout --version           Print the version
  speechout --help              Print this help
  speechout --package-macos DIR Build an ad hoc signed installer package (.pkg) in DIR (macOS)
  speechout --package-deb DIR   Build a .deb package in DIR (Linux)
  speechout --package-msi DIR   Build an .msi installer in DIR (Windows; needs the WiX Toolset)";

/// Release builds on Windows are GUI programs with no console, so `--version`
/// and `--help` would print nothing. Attach to the console of the command prompt
/// that started us, if there is one, so their output shows up.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // SAFETY: AttachConsole takes a process id and has no other preconditions;
    // failure (no parent console, or one already attached) is harmless.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(all(windows, not(debug_assertions)))]
    if !args.is_empty() {
        attach_parent_console();
    }
    match args.first().map(String::as_str) {
        None => run_gui(),
        Some("--version" | "-V") => {
            println!("{BIN_NAME} {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "macos")]
        Some("--package-macos") => package(args.get(1)),
        #[cfg(all(unix, not(target_os = "macos")))]
        Some("--package-deb") => package(args.get(1)),
        #[cfg(windows)]
        Some("--package-msi") => package(args.get(1)),
        Some(other) => {
            eprintln!("Unknown option: {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn package(dir: Option<&String>) -> ExitCode {
    let dir = std::path::PathBuf::from(dir.map_or("dist", String::as_str));
    match package::build(&dir) {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Packaging failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_gui() -> ExitCode {
    // The settings say where the log goes, so they come first. If either can't
    // be opened, the app still runs and the status bar says what went wrong.
    let mut problems = Vec::new();
    let store = store::Store::open(&store::default_db_path()).or_else(|e| {
        problems.push(format!("Your settings could not be opened, so changes won't be saved: {e:#}."));
        store::Store::in_memory()
    });
    let store = match store {
        Ok(store) => store,
        Err(e) => {
            eprintln!("Could not start: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let settings = store.settings().unwrap_or_default();
    let default_logs = store::data_dir().join("logs");
    let log_dir = settings.log_dir.clone().unwrap_or_else(|| default_logs.clone());
    if let Err(e) = logging::init(&log_dir) {
        problems.push(format!("The debug log can't go in {}: {e:#}. Using the default folder.", log_dir.display()));
        if let Err(e) = logging::init(&default_logs) {
            eprintln!("Could not start the debug log: {e:#}");
        }
    }
    for problem in &problems {
        log::warn!("{problem}");
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            // Matches StartupWMClass in the .desktop file (src/package/linux.rs).
            .with_app_id(BIN_NAME)
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([560.0, 420.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_TITLE,
        options,
        Box::new(move |cc| {
            let dialogs = Box::new(ui::NativeDialogs);
            let mut app = ui::App::new(&cc.egui_ctx, store, dialogs, problems.join(" "));
            // Here rather than in App::new, so that tests never reach GitHub.
            app.check_for_updates_at_startup();
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => {
            log::info!("Exited normally");
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("Exited with error: {e}");
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_called_speechout() {
        assert_eq!(BIN_NAME, "speechout");
        assert!(USAGE.lines().skip(3).all(|line| line.trim_start().starts_with(BIN_NAME)), "{USAGE}");
    }
}
