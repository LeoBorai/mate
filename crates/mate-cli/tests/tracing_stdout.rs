//! M0-4 acceptance (§11, §15): a run produces zero stdout bytes — all tracing
//! output lands in `mate.log`, never the terminal.

use std::process::Command;

#[test]
fn run_produces_no_stdout_and_logs_to_file() {
    let exe = env!("CARGO_BIN_EXE_mate");
    let home = tempfile::tempdir().unwrap();

    let output = Command::new(exe)
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        // A bare invocation with no args now routes to the TUI frontend (`M7`) rather than a
        // hardcoded stub error — force it to fail fast on the missing token, before it ever
        // touches the network or the terminal, so this test stays deterministic regardless of
        // whether the ambient environment happens to export a real token.
        .env_remove("API_TOKEN")
        .env("RUST_LOG", "trace")
        .output()
        .expect("failed to run mate");

    assert!(
        output.stdout.is_empty(),
        "expected zero stdout bytes, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let log_path = home.path().join(".local/state/mate/mate.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("expected log file at {}: {e}", log_path.display()));
    assert!(
        log_contents.contains("mate started"),
        "expected log to contain the startup line, got: {log_contents}"
    );
}
