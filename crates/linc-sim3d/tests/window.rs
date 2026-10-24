//! Acceptance tests for the --hal rig's window policy and for the command-line
//! rules that drive it.
//!
//! The policy is pure: the flags, LINC_SIM3D_WINDOW and whether a display is
//! set decide whether a process that owns the HAL component also owns a window.
//! The command-line rules (which flags may be combined) are checked by running
//! the real binary, because that is where clap enforces them.
//!
//! tests/rig.rs covers the rig's configuration files; this file covers the
//! window decision on top of them.

use std::process::Command;

use linc_sim3d::viewer::{
    close_request, has_display, window_mode, CloseReason, WindowFlag, WindowMode,
};

const BIN: &str = env!("CARGO_BIN_EXE_linc-sim3d");

// -------------------------------------------------------------- the policy

#[test]
fn a_display_means_a_window_by_default() {
    assert_eq!(
        window_mode(WindowFlag::Unset, None, true),
        WindowMode::IfAvailable,
        "a desktop rig shows the window without being asked twice"
    );
    assert!(WindowMode::IfAvailable.wants_window());
}

#[test]
fn no_display_means_headless() {
    assert_eq!(
        window_mode(WindowFlag::Unset, None, false),
        WindowMode::Never,
        "a rig with no display must not try to open one"
    );
    assert!(!WindowMode::Never.wants_window());
}

#[test]
fn the_environment_turns_the_window_off() {
    for value in ["0", "false", "no", "off", " FALSE "] {
        assert_eq!(
            window_mode(WindowFlag::Unset, Some(value), true),
            WindowMode::Never,
            "LINC_SIM3D_WINDOW={value} must keep a desktop rig headless"
        );
    }
}

#[test]
fn the_environment_forces_the_window() {
    for value in ["1", "true", "yes", "on", " TRUE "] {
        assert_eq!(
            window_mode(WindowFlag::Unset, Some(value), false),
            WindowMode::Required,
            "LINC_SIM3D_WINDOW={value} must behave like --window"
        );
    }
}

#[test]
fn the_flag_forces_the_window_over_the_environment() {
    assert_eq!(
        window_mode(WindowFlag::Force, Some("0"), false),
        WindowMode::Required,
        "--window is the operator speaking, and it beats the environment"
    );
}

#[test]
fn no_window_wins_over_the_environment_and_the_display() {
    assert_eq!(
        window_mode(WindowFlag::Off, Some("1"), true),
        WindowMode::Never
    );
}

#[test]
fn an_unreadable_environment_value_falls_back_to_the_display() {
    assert_eq!(
        window_mode(WindowFlag::Unset, Some("maybe"), true),
        WindowMode::IfAvailable
    );
    assert_eq!(
        window_mode(WindowFlag::Unset, Some("maybe"), false),
        WindowMode::Never
    );
    assert_eq!(
        window_mode(WindowFlag::Unset, Some("   "), true),
        WindowMode::IfAvailable
    );
}

#[test]
fn a_display_is_either_variable() {
    assert!(has_display(Some(":1"), None));
    assert!(has_display(None, Some("wayland-0")));
    assert!(!has_display(None, None));
    assert!(!has_display(Some(""), Some("  ")), "empty means no display");
}

// ------------------------------------------------------------ the command line

fn run(arguments: &[&str]) -> (i32, String) {
    let output = Command::new(BIN)
        .args(arguments)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("LINC_SIM3D_WINDOW")
        .output()
        .expect("run linc-sim3d");
    let code = output.status.code().unwrap_or(-1);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (code, text)
}

#[test]
fn window_and_no_window_are_rejected_together() {
    let (code, text) = run(&["--window", "--no-window"]);
    assert_eq!(code, 2, "clap rejects the pair: {text}");
    assert!(
        text.contains("cannot be used with"),
        "the error must name the conflict: {text}"
    );
}

#[test]
fn no_window_without_hal_is_rejected() {
    let (code, text) = run(&["--no-window"]);
    assert_eq!(
        code, 2,
        "--no-window only means something with --hal: {text}"
    );
    assert!(
        text.contains("--hal"),
        "the error must point at --hal: {text}"
    );
}

#[test]
fn the_window_flags_are_rejected_with_selftest() {
    for flag in ["--window", "--no-window"] {
        let (code, text) = run(&["--selftest", flag]);
        assert_eq!(code, 2, "{flag} must not combine with --selftest: {text}");
    }
}

#[test]
fn help_names_both_window_flags() {
    let (code, text) = run(&["--help"]);
    assert_eq!(code, 0, "help must succeed: {text}");
    assert!(
        text.contains("--window"),
        "help is missing --window: {text}"
    );
    assert!(
        text.contains("--no-window"),
        "help is missing --no-window: {text}"
    );
    assert!(
        text.contains("HAL component"),
        "help must say --hal publishes the component: {text}"
    );
}

// ------------------------------------------------------- the close decision

#[test]
fn a_stop_signal_closes_the_window() {
    assert_eq!(
        close_request(true, false),
        Some(CloseReason::StopRequested),
        "SIGINT/SIGTERM must not wait for someone to click something"
    );
}

#[test]
fn esc_closes_the_window() {
    assert_eq!(close_request(false, true), Some(CloseReason::Escape));
}

#[test]
fn a_stop_signal_wins_over_esc() {
    assert_eq!(
        close_request(true, true),
        Some(CloseReason::StopRequested),
        "Esc means keep sampling, a signal means shut down: the signal decides"
    );
}

#[test]
fn an_untouched_window_stays_open() {
    assert_eq!(close_request(false, false), None);
}
