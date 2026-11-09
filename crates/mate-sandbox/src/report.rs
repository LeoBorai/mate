//! Inspection report: configured values (from `docker inspect`) kept apart
//! from observed values (stats, changes, top).
//!
//! Environment values never enter the view model: names only.

use bollard::models::{
    ChangeType, ContainerCpuStats, ContainerInspectResponse, ContainerStatsResponse,
    ContainerTopResponse, FilesystemChange,
};
use uuid::Uuid;

use crate::docker::{DockerApi, verify_ownership};
use crate::error::SandboxError;

/// Shown with the filesystem changes.
pub const CHANGES_CAVEAT: &str = "Filesystem changes cover only the container's own layer, \
not the workspace bind mount or volumes.";

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkInfo {
    pub name: String,
    pub ip: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MountInfo {
    pub source: String,
    pub destination: String,
    pub read_write: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Configured {
    pub state: String,
    pub network_mode: String,
    pub networks: Vec<NetworkInfo>,
    pub ports: Vec<String>,
    pub mounts: Vec<MountInfo>,
    pub memory_limit: Option<i64>,
    pub pids_limit: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub cap_drop: Vec<String>,
    pub no_new_privileges: bool,
    pub privileged: bool,
    pub readonly_rootfs: bool,
    /// Variable names only.
    pub env_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangeInfo {
    pub path: String,
    /// `modified`, `added`, or `deleted`.
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunningObserved {
    pub cpu_percent: Option<f64>,
    pub memory_usage: u64,
    pub memory_limit: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub block_read_bytes: u64,
    pub block_write_bytes: u64,
    pub changes: Vec<ChangeInfo>,
    pub process_titles: Vec<String>,
    pub processes: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Observed {
    /// The container is not running, so nothing can be observed.
    NotRunning,
    Running(RunningObserved),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionReport {
    pub configured: Configured,
    pub observed: Observed,
}

/// CPU % from one stats sample: `(cpu delta / system delta) * cpus * 100`.
fn cpu_percent(cpu: &ContainerCpuStats, pre: &ContainerCpuStats) -> Option<f64> {
    let total = |s: &ContainerCpuStats| s.cpu_usage.as_ref().and_then(|u| u.total_usage);
    let cpu_delta = total(cpu)?.saturating_sub(total(pre).unwrap_or(0)) as f64;
    let sys_delta = cpu
        .system_cpu_usage?
        .saturating_sub(pre.system_cpu_usage.unwrap_or(0)) as f64;
    let cpus = cpu
        .online_cpus
        .map(f64::from)
        .or_else(|| {
            cpu.cpu_usage
                .as_ref()
                .and_then(|u| u.percpu_usage.as_ref())
                .map(|p| p.len() as f64)
        })
        .unwrap_or(1.0);
    if sys_delta > 0.0 && cpu_delta > 0.0 {
        Some(cpu_delta / sys_delta * cpus * 100.0)
    } else {
        Some(0.0)
    }
}

fn configured(inspect: &ContainerInspectResponse) -> Configured {
    let host = inspect.host_config.as_ref();
    let net = inspect.network_settings.as_ref();
    let mut ports: Vec<String> = net
        .and_then(|n| n.ports.as_ref())
        .map(|map| {
            map.iter()
                .flat_map(|(container_port, bindings)| match bindings {
                    Some(b) if !b.is_empty() => b
                        .iter()
                        .map(|b| {
                            format!(
                                "{}:{} -> {container_port}",
                                b.host_ip.as_deref().unwrap_or("0.0.0.0"),
                                b.host_port.as_deref().unwrap_or("?")
                            )
                        })
                        .collect::<Vec<_>>(),
                    _ => vec![format!("{container_port} (not published)")],
                })
                .collect()
        })
        .unwrap_or_default();
    ports.sort();
    let mut networks: Vec<NetworkInfo> = net
        .and_then(|n| n.networks.as_ref())
        .map(|m| {
            m.iter()
                .map(|(name, ep)| NetworkInfo {
                    name: name.clone(),
                    ip: ep.ip_address.clone().unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    networks.sort_by(|a, b| a.name.cmp(&b.name));
    // Split at the first `=` and drop the value here, so it never reaches
    // the view model.
    let env_names = inspect
        .config
        .as_ref()
        .and_then(|c| c.env.as_ref())
        .map(|env| {
            env.iter()
                .map(|e| e.split('=').next().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    Configured {
        state: inspect
            .state
            .as_ref()
            .and_then(|s| s.status.as_ref())
            .map(ToString::to_string)
            .unwrap_or_default(),
        network_mode: host.and_then(|h| h.network_mode.clone()).unwrap_or_default(),
        networks,
        ports,
        mounts: inspect
            .mounts
            .iter()
            .flatten()
            .map(|m| MountInfo {
                source: m.source.clone().or_else(|| m.name.clone()).unwrap_or_default(),
                destination: m.destination.clone().unwrap_or_default(),
                read_write: m.rw.unwrap_or(false),
            })
            .collect(),
        memory_limit: host.and_then(|h| h.memory),
        pids_limit: host.and_then(|h| h.pids_limit),
        nano_cpus: host.and_then(|h| h.nano_cpus),
        cap_drop: host.and_then(|h| h.cap_drop.clone()).unwrap_or_default(),
        no_new_privileges: host
            .and_then(|h| h.security_opt.as_ref())
            .is_some_and(|o| o.iter().any(|s| s.starts_with("no-new-privileges"))),
        privileged: host.and_then(|h| h.privileged).unwrap_or(false),
        readonly_rootfs: host.and_then(|h| h.readonly_rootfs).unwrap_or(false),
        env_names,
    }
}

fn observed(
    stats: &ContainerStatsResponse,
    changes: &[FilesystemChange],
    top: &ContainerTopResponse,
) -> RunningObserved {
    let cpu_percent = match (&stats.cpu_stats, &stats.precpu_stats) {
        (Some(cpu), Some(pre)) => cpu_percent(cpu, pre),
        _ => None,
    };
    let mem = stats.memory_stats.as_ref();
    let raw_usage = mem.and_then(|m| m.usage).unwrap_or(0);
    // Same adjustment as `docker stats`: drop reclaimable page cache
    // (`inactive_file` on cgroup v2, `cache` on v1).
    let cache = mem
        .and_then(|m| m.stats.as_ref())
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("cache")))
        .copied()
        .unwrap_or(0);
    let (rx, tx) = stats
        .networks
        .iter()
        .flat_map(|m| m.values())
        .fold((0u64, 0u64), |(rx, tx), n| {
            (rx + n.rx_bytes.unwrap_or(0), tx + n.tx_bytes.unwrap_or(0))
        });
    let (mut read, mut write) = (0u64, 0u64);
    for e in stats
        .blkio_stats
        .iter()
        .flat_map(|b| b.io_service_bytes_recursive.iter().flatten())
    {
        match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("read") => read += e.value.unwrap_or(0),
            Some("write") => write += e.value.unwrap_or(0),
            _ => {}
        }
    }
    RunningObserved {
        cpu_percent,
        memory_usage: raw_usage.saturating_sub(cache),
        memory_limit: mem.and_then(|m| m.limit).unwrap_or(0),
        net_rx_bytes: rx,
        net_tx_bytes: tx,
        block_read_bytes: read,
        block_write_bytes: write,
        changes: changes
            .iter()
            .map(|c| ChangeInfo {
                path: c.path.clone(),
                kind: match c.kind {
                    ChangeType::_0 => "modified",
                    ChangeType::_1 => "added",
                    ChangeType::_2 => "deleted",
                },
            })
            .collect(),
        process_titles: top.titles.clone().unwrap_or_default(),
        processes: top.processes.clone().unwrap_or_default(),
    }
}

/// Build the report. `observed` is `None` when the container is not running.
pub fn build(
    inspect: &ContainerInspectResponse,
    observed_inputs: Option<(&ContainerStatsResponse, &[FilesystemChange], &ContainerTopResponse)>,
) -> SessionReport {
    SessionReport {
        configured: configured(inspect),
        observed: match observed_inputs {
            Some((stats, changes, top)) => Observed::Running(observed(stats, changes, top)),
            None => Observed::NotRunning,
        },
    }
}

/// Fetch inspect (ownership-checked), and stats, changes, and top only when
/// the container is running, then build the report.
pub async fn inspect_session(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
) -> Result<SessionReport, SandboxError> {
    let inspect = verify_ownership(docker, container, session_id).await?;
    let running = inspect
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);
    if !running {
        return Ok(build(&inspect, None));
    }
    let stats = docker.stats_once(container).await?;
    let changes = docker.changes(container).await?;
    let top = docker.top(container).await?;
    Ok(build(&inspect, Some((&stats, &changes, &top))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDocker, labelled_inspect};
    use bollard::models::{
        ContainerBlkioStatEntry, ContainerBlkioStats, ContainerCpuUsage, ContainerMemoryStats,
        ContainerNetworkStats, HostConfig, MountPoint,
    };
    use std::collections::HashMap;

    fn inspect() -> ContainerInspectResponse {
        let mut i = labelled_inspect(&[("managed-by", "mate")], "running");
        i.config.as_mut().unwrap().env = Some(vec![
            "CLAUDE_CONFIG_DIR=/home/u/.claude".into(),
            "TOPSECRET=hunter2-value".into(),
        ]);
        i.host_config = Some(HostConfig {
            network_mode: Some("bridge".into()),
            cap_drop: Some(vec!["ALL".into()]),
            security_opt: Some(vec!["no-new-privileges:true".into()]),
            privileged: Some(false),
            memory: Some(4096),
            pids_limit: Some(512),
            nano_cpus: Some(2_000_000_000),
            ..Default::default()
        });
        i.mounts = Some(vec![
            MountPoint {
                source: Some("/ws".into()),
                destination: Some("/workspace".into()),
                rw: Some(true),
                ..Default::default()
            },
            MountPoint {
                source: Some("/ro".into()),
                destination: Some("/ro".into()),
                rw: Some(false),
                ..Default::default()
            },
        ]);
        i
    }

    fn cpu(total: u64, system: u64) -> ContainerCpuStats {
        ContainerCpuStats {
            cpu_usage: Some(ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(2),
            ..Default::default()
        }
    }

    fn stats() -> ContainerStatsResponse {
        ContainerStatsResponse {
            cpu_stats: Some(cpu(300, 2000)),
            precpu_stats: Some(cpu(100, 1000)),
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(1000),
                limit: Some(4096),
                stats: Some(HashMap::from([("inactive_file".to_owned(), 200)])),
                ..Default::default()
            }),
            networks: Some(HashMap::from([
                ("eth0".to_owned(), ContainerNetworkStats { rx_bytes: Some(10), tx_bytes: Some(20), ..Default::default() }),
                ("eth1".to_owned(), ContainerNetworkStats { rx_bytes: Some(1), tx_bytes: Some(2), ..Default::default() }),
            ])),
            blkio_stats: Some(ContainerBlkioStats {
                io_service_bytes_recursive: Some(vec![
                    ContainerBlkioStatEntry { op: Some("Read".into()), value: Some(5), ..Default::default() },
                    ContainerBlkioStatEntry { op: Some("write".into()), value: Some(7), ..Default::default() },
                    ContainerBlkioStatEntry { op: Some("Total".into()), value: Some(12), ..Default::default() },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn top() -> ContainerTopResponse {
        ContainerTopResponse {
            titles: Some(vec!["PID".into(), "CMD".into()]),
            processes: Some(vec![vec!["1".into(), "sleep infinity".into()]]),
        }
    }

    #[test]
    fn observed_values_are_computed() {
        let changes = [FilesystemChange { path: "/tmp/x".into(), kind: ChangeType::_1 }];
        let r = build(&inspect(), Some((&stats(), &changes, &top())));
        let Observed::Running(o) = r.observed else { panic!("expected Running") };
        // (300-100)/(2000-1000) * 2 cpus * 100 = 40
        assert_eq!(o.cpu_percent, Some(40.0), "CPU % uses the delta formula with precpu_stats");
        assert_eq!(o.memory_usage, 800, "memory drops inactive_file cache (1000 - 200)");
        assert_eq!(o.memory_limit, 4096, "memory limit from stats");
        assert_eq!((o.net_rx_bytes, o.net_tx_bytes), (11, 22), "network totals sum interfaces");
        assert_eq!((o.block_read_bytes, o.block_write_bytes), (5, 7), "block totals count read/write ops only, case-insensitively");
        assert_eq!(o.changes, vec![ChangeInfo { path: "/tmp/x".into(), kind: "added" }], "change kind decoded");
        assert_eq!(o.process_titles, ["PID", "CMD"], "top titles kept");
    }

    #[test]
    fn configured_values_and_mount_flags() {
        let c = build(&inspect(), None).configured;
        assert_eq!(c.network_mode, "bridge", "network mode");
        assert_eq!(c.cap_drop, ["ALL"], "capabilities dropped");
        assert!(c.no_new_privileges, "no-new-privileges detected");
        assert!(!c.privileged, "not privileged");
        assert_eq!((c.memory_limit, c.pids_limit, c.nano_cpus), (Some(4096), Some(512), Some(2_000_000_000)), "limits");
        assert_eq!(
            c.mounts.iter().map(|m| (m.destination.as_str(), m.read_write)).collect::<Vec<_>>(),
            [("/workspace", true), ("/ro", false)],
            "mount read-write flags preserved"
        );
        assert_eq!(c.env_names, ["CLAUDE_CONFIG_DIR", "TOPSECRET"], "env shows names only");
    }

    #[test]
    fn no_env_value_appears_in_debug_output() {
        let changes: [FilesystemChange; 0] = [];
        let r = build(&inspect(), Some((&stats(), &changes, &top())));
        let dump = format!("{r:?}");
        assert!(!dump.contains("hunter2-value"), "env values must never reach the report");
        assert!(!dump.contains("/home/u/.claude"), "env values (even paths) are dropped");
    }

    #[test]
    fn zero_deltas_give_zero_cpu() {
        assert_eq!(
            cpu_percent(&cpu(100, 1000), &cpu(100, 1000)),
            Some(0.0),
            "no CPU or system delta is 0%, not NaN"
        );
    }

    #[tokio::test]
    async fn stopped_container_reports_not_running() {
        let fake = FakeDocker::default();
        let id = Uuid::from_u128(5);
        let mut i = labelled_inspect(
            &[("managed-by", "mate"), ("session-id", &id.to_string())],
            "exited",
        );
        i.host_config = Some(HostConfig { network_mode: Some("none".into()), ..Default::default() });
        fake.add("c", i);
        let r = inspect_session(&fake, "c", &id).await.unwrap();
        assert_eq!(r.observed, Observed::NotRunning, "stopped containers have no observed section");
        assert_eq!(r.configured.network_mode, "none", "configured section still shows");
        assert_eq!(fake.calls(), vec!["inspect c".to_owned()], "stats/changes/top are not fetched when not running");
    }
}
