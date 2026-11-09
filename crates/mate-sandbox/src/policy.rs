//! Sandbox policy and its pure translation to a Docker host config.

use std::path::{Path, PathBuf};

use bollard::models::HostConfig;

use crate::error::SandboxError;

/// Where the host workspace appears inside the container.
pub const WORKSPACE_TARGET: &str = "/workspace";

/// Paths that, as a mount source, hand the container the Docker daemon.
const DOCKER_SOCKETS: [&str; 2] = ["/var/run/docker.sock", "/run/docker.sock"];

/// Network attachment, fixed at creation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    None,
    Bridge,
    Custom(String),
}

impl NetworkMode {
    fn as_docker(&self) -> String {
        match self {
            NetworkMode::None => "none".to_owned(),
            NetworkMode::Bridge => "bridge".to_owned(),
            NetworkMode::Custom(name) => name.clone(),
        }
    }
}

/// An extra bind mount. Read-only vs read-write is always explicit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

/// Everything about a sandbox's isolation, chosen at creation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicy {
    pub network: NetworkMode,
    pub workspace: PathBuf,
    pub extra_mounts: Vec<Mount>,
    /// Bytes.
    pub memory_limit: i64,
    pub pids_limit: i64,
    /// Fractional CPUs, converted to nano-CPUs for Docker.
    pub cpu_limit: f64,
    pub read_only_rootfs: bool,
}

impl SandboxPolicy {
    pub const DEFAULT_MEMORY: i64 = 4 * 1024 * 1024 * 1024;
    pub const DEFAULT_PIDS: i64 = 512;
    pub const DEFAULT_CPUS: f64 = 2.0;

    /// Defaults from the design: bridge network, 4 GiB, 512 PIDs, 2 CPUs,
    /// writable root filesystem.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            network: NetworkMode::Bridge,
            workspace: workspace.into(),
            extra_mounts: Vec::new(),
            memory_limit: Self::DEFAULT_MEMORY,
            pids_limit: Self::DEFAULT_PIDS,
            cpu_limit: Self::DEFAULT_CPUS,
            read_only_rootfs: false,
        }
    }

    fn nano_cpus(&self) -> i64 {
        (self.cpu_limit * 1_000_000_000.0).round() as i64
    }
}

fn socket_from_docker_host(docker_host: Option<&str>) -> Option<&str> {
    docker_host?.strip_prefix("unix://")
}

fn is_docker_socket(path: &Path, docker_host: Option<&str>) -> bool {
    DOCKER_SOCKETS
        .iter()
        .map(Path::new)
        .chain(socket_from_docker_host(docker_host).map(Path::new))
        .any(|sock| path == sock)
}

/// Refuse policies that must never reach Docker. Pure apart from the
/// workspace directory check; `docker_host` is the `DOCKER_HOST` value.
pub fn validate_policy(
    policy: &SandboxPolicy,
    docker_host: Option<&str>,
) -> Result<(), SandboxError> {
    if !policy.workspace.is_dir() {
        return Err(SandboxError::InvalidPolicy(format!(
            "workspace {} is not an existing directory",
            policy.workspace.display()
        )));
    }
    let sources = std::iter::once(&policy.workspace).chain(policy.extra_mounts.iter().map(|m| &m.source));
    for source in sources {
        if is_docker_socket(source, docker_host) {
            return Err(SandboxError::InvalidPolicy(format!(
                "mounting the Docker daemon socket ({}) is not allowed",
                source.display()
            )));
        }
    }
    Ok(())
}

fn bind(source: &str, target: &str, read_only: bool) -> String {
    format!("{source}:{target}:{}", if read_only { "ro" } else { "rw" })
}

/// Translate a policy into Docker's host config. `state_volume` is mounted
/// read-write at `state_target` (Claude Code's configuration directory).
pub fn policy_to_host_config(
    policy: &SandboxPolicy,
    state_volume: &str,
    state_target: &str,
) -> HostConfig {
    let mut binds = vec![bind(
        &policy.workspace.to_string_lossy(),
        WORKSPACE_TARGET,
        false,
    )];
    binds.extend(
        policy
            .extra_mounts
            .iter()
            .map(|m| bind(&m.source.to_string_lossy(), &m.target, m.read_only)),
    );
    binds.push(bind(state_volume, state_target, false));

    HostConfig {
        binds: Some(binds),
        network_mode: Some(policy.network.as_docker()),
        cap_drop: Some(vec!["ALL".to_owned()]),
        security_opt: Some(vec!["no-new-privileges:true".to_owned()]),
        privileged: Some(false),
        memory: Some(policy.memory_limit),
        pids_limit: Some(policy.pids_limit),
        nano_cpus: Some(policy.nano_cpus()),
        readonly_rootfs: Some(policy.read_only_rootfs),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_in(dir: &Path) -> SandboxPolicy {
        SandboxPolicy::new(dir)
    }

    #[test]
    fn missing_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy_in(&dir.path().join("nope"));
        let err = validate_policy(&p, None).unwrap_err();
        assert!(
            matches!(err, SandboxError::InvalidPolicy(_)),
            "a nonexistent workspace must be rejected as InvalidPolicy, got {err:?}"
        );
    }

    #[test]
    fn file_as_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(
            validate_policy(&policy_in(&file), None).is_err(),
            "a regular file is not a valid workspace directory"
        );
    }

    #[test]
    fn valid_workspace_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            validate_policy(&policy_in(dir.path()), None).is_ok(),
            "an existing directory with no extra mounts is a valid policy"
        );
    }

    fn with_mount(dir: &Path, source: &str) -> SandboxPolicy {
        let mut p = policy_in(dir);
        p.extra_mounts.push(Mount {
            source: source.into(),
            target: "/x".into(),
            read_only: true,
        });
        p
    }

    #[test]
    fn docker_sockets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for sock in ["/var/run/docker.sock", "/run/docker.sock"] {
            assert!(
                validate_policy(&with_mount(dir.path(), sock), None).is_err(),
                "{sock} must be refused as a mount source"
            );
        }
    }

    #[test]
    fn docker_host_unix_socket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = with_mount(dir.path(), "/custom/d.sock");
        assert!(
            validate_policy(&p, Some("unix:///custom/d.sock")).is_err(),
            "the socket named by a unix:// DOCKER_HOST must be refused"
        );
        assert!(
            validate_policy(&p, None).is_ok(),
            "the same path is fine when DOCKER_HOST does not name it"
        );
    }

    #[test]
    fn host_config_is_hardened_and_limited() {
        let dir = tempfile::tempdir().unwrap();
        let hc = policy_to_host_config(&policy_in(dir.path()), "vol", "/home/u/.claude");
        assert_eq!(hc.cap_drop, Some(vec!["ALL".into()]), "all capabilities dropped");
        assert_eq!(
            hc.security_opt,
            Some(vec!["no-new-privileges:true".into()]),
            "no-new-privileges set"
        );
        assert_eq!(hc.privileged, Some(false), "never privileged");
        assert_eq!(hc.memory, Some(4 * 1024 * 1024 * 1024), "default 4 GiB memory");
        assert_eq!(hc.pids_limit, Some(512), "default PID limit");
        assert_eq!(hc.nano_cpus, Some(2_000_000_000), "2 CPUs as nano-CPUs");
        assert_eq!(hc.readonly_rootfs, Some(false), "rootfs writable by default");
    }

    #[test]
    fn host_config_network_modes() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = policy_in(dir.path());
        for (mode, want) in [
            (NetworkMode::None, "none"),
            (NetworkMode::Bridge, "bridge"),
            (NetworkMode::Custom("mynet".into()), "mynet"),
        ] {
            p.network = mode;
            assert_eq!(
                policy_to_host_config(&p, "v", "/t").network_mode.as_deref(),
                Some(want),
                "network mode maps to Docker's network_mode string"
            );
        }
    }

    #[test]
    fn host_config_binds() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_string_lossy().into_owned();
        let mut p = policy_in(dir.path());
        p.read_only_rootfs = true;
        p.extra_mounts.push(Mount { source: "/a".into(), target: "/ro".into(), read_only: true });
        p.extra_mounts.push(Mount { source: "/b".into(), target: "/rw".into(), read_only: false });
        let hc = policy_to_host_config(&p, "dkc-claude-1", "/home/u/.claude");
        assert_eq!(
            hc.binds,
            Some(vec![
                format!("{ws}:/workspace:rw"),
                "/a:/ro:ro".into(),
                "/b:/rw:rw".into(),
                "dkc-claude-1:/home/u/.claude:rw".into(),
            ]),
            "workspace rw, extra mounts with explicit modes, then the state volume"
        );
        assert_eq!(hc.readonly_rootfs, Some(true), "read_only_rootfs is passed through");
        assert!(
            hc.binds.unwrap().iter().all(|b| !b.contains("docker.sock")),
            "no Docker socket in any bind"
        );
    }
}
