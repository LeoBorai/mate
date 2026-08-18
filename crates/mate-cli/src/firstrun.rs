//! First-run notice (`M13-5`): before mate ever reads a file or makes a request on the user's
//! behalf, say so once. Gated on a marker file in the same state directory `logging` already
//! writes to, so it shows exactly once per machine — not once per invocation, and not once per
//! workspace.

use std::fs;
use std::path::PathBuf;

use crate::logging::state_dir;

const NOTICE: &str = "\
mate can read files inside your workspace and make outbound network requests on your behalf, \
using the read_file/list_dir/find_files and http_request tools. See `mate --help` and \
.mate.toml / ~/.config/mate/config.toml to control what it's allowed to do.\n";

fn marker_path() -> Option<PathBuf> {
    state_dir().ok().map(|dir| dir.join(".first_run_ack"))
}

/// Prints [`NOTICE`] to stderr and writes the marker, but only the first time this ever runs on
/// this machine. Every failure mode here (can't resolve the state dir, can't create it, can't
/// write the marker) just means the notice prints again next time — never a reason to fail the
/// run over.
pub fn show_once() {
    let Some(marker) = marker_path() else {
        return;
    };
    if marker.exists() {
        return;
    }
    eprint!("{NOTICE}");
    if let Some(parent) = marker.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&marker, b"");
}

#[cfg(test)]
// Same reasoning as `logging`'s own tests: `Jail::expect_with` fixes the closure's error to
// `figment::Error`, which clippy flags as large and isn't ours to box.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    #[test]
    fn first_call_writes_the_marker_and_prints_the_notice() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);

            let marker = marker_path().unwrap();
            assert!(!marker.exists());
            show_once();
            assert!(marker.exists(), "the marker file must be created on first run");
            Ok(())
        });
    }

    #[test]
    fn a_second_call_is_a_silent_no_op() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);

            show_once();
            let marker = marker_path().unwrap();
            let created_at = fs::metadata(&marker).unwrap().modified().unwrap();

            show_once();
            let still = fs::metadata(&marker).unwrap().modified().unwrap();
            assert_eq!(
                created_at, still,
                "a second show_once must not rewrite the marker"
            );
            Ok(())
        });
    }
}
