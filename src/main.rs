//! Speech Output Engine (`speechout`): an accessible text-to-speech reader.

// Release builds on Windows are GUI programs without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod document;
mod i18n;
mod logging;
mod paths;
mod platform;
mod secrets;
mod settings;
mod speech;
mod spoken;
mod transcribe;
mod update;
mod vision;
mod wordlist;
mod worker;

use std::process::ExitCode;

const USAGE: &str = "Speech Output Engine

Usage:
  speechout                     Start the application
  speechout --version           Print the version
  speechout --help              Print this help
  speechout --package-macos DIR Build an ad hoc signed .app bundle and zip in DIR (macOS)
  speechout --package-deb DIR   Build a .deb package in DIR (Linux)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_gui(),
        Some("--version" | "-V") => {
            println!("speechout {}", env!("CARGO_PKG_VERSION"));
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
        Some(other) => {
            eprintln!("Unknown option: {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn package(dir: Option<&String>) -> ExitCode {
    let dir = std::path::PathBuf::from(dir.map(String::as_str).unwrap_or("dist"));
    match platform::package(&dir) {
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
    logging::init();
    let settings = settings::Settings::load();
    i18n::activate_saved(&paths::languages_dir(), &settings.language);
    let log_status = match logging::set_directory(&settings.log_dir()) {
        Ok(_) => String::new(),
        Err(e) => i18n::tf("status.log_off", &[("error", &e)]),
    };
    log::info!("starting speechout {}", env!("CARGO_PKG_VERSION"));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::APP_TITLE)
            .with_app_id("speechout")
            .with_inner_size([640.0, 820.0])
            .with_min_inner_size([360.0, 480.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        app::APP_TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(app::SpeechApp::new(cc, settings, log_status)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("could not start the window: {e}");
            eprintln!("Could not start the window: {e}");
            ExitCode::FAILURE
        }
    }
}
