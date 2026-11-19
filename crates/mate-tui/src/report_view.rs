//! The `/inspect` report as plain lines. Pure: `mate_sandbox::report::SessionReport` in, strings
//! out, so the layout is tested without a terminal (the `ui.rs` convention). Two titled blocks
//! keep what the container was *configured* with apart from what was *observed* running.

use mate_sandbox::report::{
    CHANGES_CAVEAT, Configured, Observed, RunningObserved, SessionReport,
};

pub(crate) const CONFIGURED_TITLE: &str = "Configured (docker inspect)";
pub(crate) const OBSERVED_TITLE: &str = "Observed (stats / changes / top)";

/// Rows shown per list (changes, processes) before "… N more".
const MAX_ROWS: usize = 8;

pub(crate) fn report_lines(report: &SessionReport) -> Vec<String> {
    let mut lines = configured_lines(&report.configured);
    lines.push(String::new());
    lines.push(OBSERVED_TITLE.to_string());
    match &report.observed {
        Observed::NotRunning => lines.push("  not running: nothing to observe".to_string()),
        Observed::Running(observed) => lines.extend(observed_lines(observed)),
    }
    lines
}

fn configured_lines(c: &Configured) -> Vec<String> {
    let mut lines = vec![CONFIGURED_TITLE.to_string()];
    lines.push(format!("  state: {}", c.state));
    lines.push(format!("  network mode: {}", c.network_mode));
    for net in &c.networks {
        lines.push(format!("  network {}: {}", net.name, display_or_dash(&net.ip)));
    }
    if !c.ports.is_empty() {
        lines.push(format!("  ports: {}", c.ports.join(", ")));
    }
    lines.push("  mounts:".to_string());
    if c.mounts.is_empty() {
        lines.push("    none".to_string());
    }
    for mount in &c.mounts {
        let mode = if mount.read_write { "rw" } else { "ro" };
        lines.push(format!("    {} -> {} ({mode})", mount.source, mount.destination));
    }
    lines.push(format!(
        "  limits: memory {}, pids {}, cpus {}",
        c.memory_limit
            .filter(|m| *m > 0)
            .map_or("unlimited".to_string(), |m| bytes(m as u64)),
        c.pids_limit
            .filter(|p| *p > 0)
            .map_or("unlimited".to_string(), |p| p.to_string()),
        c.nano_cpus
            .filter(|n| *n > 0)
            .map_or("unlimited".to_string(), |n| format!("{:.2}", n as f64 / 1e9)),
    ));
    lines.push(format!(
        "  hardening: cap_drop [{}], no-new-privileges {}, privileged {}, read-only rootfs {}",
        c.cap_drop.join(", "),
        yes_no(c.no_new_privileges),
        yes_no(c.privileged),
        yes_no(c.readonly_rootfs),
    ));
    lines.push(format!("  env (names only): {}", c.env_names.join(", ")));
    lines
}

fn observed_lines(o: &RunningObserved) -> Vec<String> {
    let mut lines = Vec::new();
    let cpu = o
        .cpu_percent
        .map_or("n/a".to_string(), |p| format!("{p:.1}%"));
    lines.push(format!(
        "  cpu {cpu}, memory {} / {}",
        bytes(o.memory_usage),
        bytes(o.memory_limit)
    ));
    lines.push(format!(
        "  network rx {}, tx {}",
        bytes(o.net_rx_bytes),
        bytes(o.net_tx_bytes)
    ));
    lines.push(format!(
        "  block read {}, write {}",
        bytes(o.block_read_bytes),
        bytes(o.block_write_bytes)
    ));
    lines.push(format!("  filesystem changes: {}", o.changes.len()));
    for change in o.changes.iter().take(MAX_ROWS) {
        lines.push(format!("    {} {}", change.kind, change.path));
    }
    if o.changes.len() > MAX_ROWS {
        lines.push(format!("    … {} more", o.changes.len() - MAX_ROWS));
    }
    lines.push(format!("  note: {CHANGES_CAVEAT}"));
    lines.push(format!("  processes: {}", o.processes.len()));
    for process in o.processes.iter().take(MAX_ROWS) {
        lines.push(format!("    {}", process.join(" ")));
    }
    if o.processes.len() > MAX_ROWS {
        lines.push(format!("    … {} more", o.processes.len() - MAX_ROWS));
    }
    lines
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn display_or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mate_sandbox::report::{ChangeInfo, MountInfo, NetworkInfo};

    fn configured() -> Configured {
        Configured {
            state: "running".into(),
            network_mode: "bridge".into(),
            networks: vec![NetworkInfo {
                name: "bridge".into(),
                ip: "172.17.0.2".into(),
            }],
            ports: vec![],
            mounts: vec![MountInfo {
                source: "/home/u/ws".into(),
                destination: "/workspace".into(),
                read_write: true,
            }],
            memory_limit: Some(4 * 1024 * 1024 * 1024),
            pids_limit: Some(512),
            nano_cpus: Some(2_000_000_000),
            cap_drop: vec!["ALL".into()],
            no_new_privileges: true,
            privileged: false,
            readonly_rootfs: false,
            env_names: vec!["CLAUDE_CONFIG_DIR".into()],
        }
    }

    fn running() -> SessionReport {
        SessionReport {
            configured: configured(),
            observed: Observed::Running(RunningObserved {
                cpu_percent: Some(12.5),
                memory_usage: 10 * 1024 * 1024,
                memory_limit: 4 * 1024 * 1024 * 1024,
                net_rx_bytes: 2048,
                net_tx_bytes: 512,
                block_read_bytes: 0,
                block_write_bytes: 1024,
                changes: vec![ChangeInfo {
                    path: "/tmp/x".into(),
                    kind: "added",
                }],
                process_titles: vec!["PID".into(), "CMD".into()],
                processes: vec![vec!["1".into(), "sleep infinity".into()]],
            }),
        }
    }

    #[test]
    fn a_running_report_shows_both_titled_blocks_and_the_layer_caveat() {
        let text = report_lines(&running()).join("\n");
        assert!(text.contains(CONFIGURED_TITLE), "the configured block is titled");
        assert!(text.contains(OBSERVED_TITLE), "the observed block is titled");
        assert!(
            text.contains(CHANGES_CAVEAT),
            "filesystem changes carry the container-layer caveat"
        );
    }

    #[test]
    fn the_configured_block_lists_mounts_limits_and_hardening() {
        let text = report_lines(&running()).join("\n");
        assert!(
            text.contains("/home/u/ws -> /workspace (rw)"),
            "mounts show their read-write flag, got {text}"
        );
        assert!(
            text.contains("memory 4.0 GiB, pids 512, cpus 2.00"),
            "limits are shown in readable units, got {text}"
        );
        assert!(
            text.contains("cap_drop [ALL], no-new-privileges yes, privileged no"),
            "hardening is shown, got {text}"
        );
        assert!(
            text.contains("env (names only): CLAUDE_CONFIG_DIR"),
            "env shows names only, got {text}"
        );
    }

    #[test]
    fn a_stopped_sandbox_still_shows_configured_and_says_nothing_is_observed() {
        let report = SessionReport {
            configured: configured(),
            observed: Observed::NotRunning,
        };
        let text = report_lines(&report).join("\n");
        assert!(text.contains(CONFIGURED_TITLE), "configured values are still shown");
        assert!(
            text.contains("not running"),
            "the observed block says the container is not running, got {text}"
        );
        assert!(
            !text.contains(CHANGES_CAVEAT),
            "no filesystem changes are shown, so no caveat is needed"
        );
    }

    #[test]
    fn long_change_lists_are_capped() {
        let mut report = running();
        if let Observed::Running(o) = &mut report.observed {
            o.changes = (0..20)
                .map(|i| ChangeInfo {
                    path: format!("/f{i}"),
                    kind: "added",
                })
                .collect();
        }
        let text = report_lines(&report).join("\n");
        assert!(text.contains("… 12 more"), "rows past the cap are summarised, got {text}");
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(bytes(512), "512 B", "small values stay in bytes");
        assert_eq!(bytes(1536), "1.5 KiB", "KiB is 1024 bytes");
    }
}
