//! The command line: options that print something and exit, run against the
//! built program. Options that open the window, read a file aloud or build a
//! package are left out, since they need a screen, speakers or a release
//! build.

use std::process::{Command, Output};

const USAGE_START: &str = "Speech Output Engine\n\nUsage:\n";

fn speechout(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_speechout")).args(args).output().expect("speechout runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn prints_its_version() {
    for option in ["--version", "-V"] {
        let output = speechout(&[option]);
        assert!(output.status.success(), "{option}");
        assert_eq!(stdout(&output), format!("speechout {}\n", env!("CARGO_PKG_VERSION")), "{option}");
    }
}

#[test]
fn prints_its_usage() {
    for option in ["--help", "-h"] {
        let output = speechout(&[option]);
        assert!(output.status.success(), "{option}");
        let usage = stdout(&output);
        assert!(usage.starts_with(USAGE_START), "{option}: {usage}");
        for listed in ["--read FILE", "--version", "--help"] {
            assert!(usage.contains(listed), "{option} doesn't list {listed}");
        }
    }
}

#[test]
fn read_needs_a_file() {
    let output = speechout(&["--read"]);
    assert_eq!(output.status.code(), Some(2));
    let error = stderr(&output);
    assert!(error.starts_with("--read needs a file\n\n") && error.contains(USAGE_START), "{error}");
}

#[test]
fn unknown_options_are_refused() {
    for option in ["--bogus", "-x", "read"] {
        let output = speechout(&[option]);
        assert_eq!(output.status.code(), Some(2), "{option}");
        let error = stderr(&output);
        assert!(error.starts_with(&format!("Unknown option: {option}\n\n")) && error.contains(USAGE_START), "{error}");
    }
}

/// Each packaging option only exists on the system it builds a package for.
#[test]
fn packaging_options_are_only_on_their_own_system() {
    let foreign: &[&str] = if cfg!(target_os = "macos") {
        &["--package-deb"]
    } else if cfg!(target_os = "windows") {
        &["--package-macos", "--package-deb"]
    } else {
        &["--package-macos"]
    };
    for option in foreign {
        let output = speechout(&[option, "dist"]);
        assert_eq!(output.status.code(), Some(2), "{option}");
        assert!(stderr(&output).starts_with(&format!("Unknown option: {option}\n")), "{option}");
    }
}
