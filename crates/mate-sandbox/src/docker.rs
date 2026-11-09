//! Docker access: the [`DockerApi`] trait, its bollard implementation, and
//! the ownership guard every mutating call goes through.
//!
//! bollard API used (version 0.21.1, plus `bollard-stubs` 1.53.1-rc.29.3.1),
//! read from source under `~/.cargo/registry/src/<index>/bollard-0.21.1/`:
//!
//! - `src/docker.rs`: `Docker::connect_with_socket_defaults()` (honours the
//!   default socket path, returns `Result<Docker, Error>`).
//! - `src/container.rs`: `create_container(Option<CreateContainerOptions>,
//!   ContainerCreateBody)`, `start_container`, `stop_container`,
//!   `remove_container`, `inspect_container`, `list_containers`,
//!   `top_processes`, `container_changes` (`Option<Vec<FilesystemChange>>`),
//!   `stats` (a `Stream`), and `LogOutput::{StdOut, StdErr, StdIn, Console}`.
//! - `src/exec.rs`: `create_exec(&str, impl Into<ExecConfig>)` via
//!   `CreateExecOptions<String>`, `start_exec(&str, Option<StartExecOptions>)`
//!   returning `StartExecResults::{Attached { output, input }, Detached}`,
//!   `inspect_exec`, `resize_exec(&str, exec::ResizeExecOptions)`.
//! - `src/image.rs`: `create_image` (a `Stream` of `CreateImageInfo`) and
//!   `inspect_image`.
//! - `src/volume.rs`: `create_volume(VolumeCreateRequest)`, `remove_volume`.
//! - `src/errors.rs`: `Error::{DockerResponseServerError, IOError,
//!   HyperResponseError, HyperLegacyError, RequestTimeoutError,
//!   SocketNotFoundError, ..}`.
//! - Option builders live in `bollard-stubs/src/query_parameters.rs`
//!   (`bollard::query_parameters`).

use std::collections::HashMap;
use std::pin::Pin;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, ContainerStatsResponse, ContainerSummary,
    ContainerTopResponse, ExecInspectResponse, FilesystemChange, ImageInspect,
    VolumeCreateRequest,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder,
    RemoveContainerOptionsBuilder, StatsOptionsBuilder, StopContainerOptionsBuilder,
};
use futures::{Stream, StreamExt};
use tokio::io::AsyncWrite;
use uuid::Uuid;

use crate::error::SandboxError;
use crate::session::{LABEL_MANAGED_BY, LABEL_MANAGED_VALUE, LABEL_SESSION_ID};

/// Container output, demultiplexed by Docker's stream framing.
pub type ExecOutput = Pin<Box<dyn Stream<Item = Result<LogOutput, SandboxError>> + Send>>;
/// Container input.
pub type ExecInput = Pin<Box<dyn AsyncWrite + Send>>;

/// An attached exec: read `output`, write `input`.
pub struct ExecIo {
    pub output: ExecOutput,
    pub input: ExecInput,
}

/// What to run in an exec.
#[derive(Debug, Clone, Default)]
pub struct ExecSpec {
    pub cmd: Vec<String>,
    pub tty: bool,
    pub attach_stdin: bool,
    pub env: Vec<String>,
}

/// The Docker operations the sandbox needs, and nothing more. Raw: no
/// ownership checks here. Callers use the guarded helpers below for anything
/// that stops, removes, or execs.
#[async_trait]
pub trait DockerApi: Send + Sync {
    async fn inspect_image(&self, image: &str) -> Result<ImageInspect, SandboxError>;
    /// Pull `image`, calling `progress` with a status line as layers move.
    async fn pull_image(
        &self,
        image: &str,
        progress: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), SandboxError>;
    async fn create_volume(&self, name: &str) -> Result<(), SandboxError>;
    async fn remove_volume(&self, name: &str) -> Result<(), SandboxError>;
    /// Returns the new container id.
    async fn create_container(
        &self,
        name: &str,
        body: ContainerCreateBody,
    ) -> Result<String, SandboxError>;
    async fn start_container(&self, id: &str) -> Result<(), SandboxError>;
    async fn stop_container(&self, id: &str) -> Result<(), SandboxError>;
    async fn remove_container(&self, id: &str) -> Result<(), SandboxError>;
    async fn inspect_container(&self, id: &str)
    -> Result<ContainerInspectResponse, SandboxError>;
    /// Only containers labelled `managed-by=mate`.
    async fn list_managed_containers(&self) -> Result<Vec<ContainerSummary>, SandboxError>;
    /// Returns the exec id.
    async fn create_exec(&self, container: &str, spec: ExecSpec) -> Result<String, SandboxError>;
    async fn start_exec(&self, exec_id: &str, tty: bool) -> Result<ExecIo, SandboxError>;
    async fn resize_exec(&self, exec_id: &str, width: u16, height: u16)
    -> Result<(), SandboxError>;
    async fn inspect_exec(&self, exec_id: &str) -> Result<ExecInspectResponse, SandboxError>;
    /// One stats sample (includes `precpu_stats`, so CPU % can be computed).
    async fn stats_once(&self, id: &str) -> Result<ContainerStatsResponse, SandboxError>;
    async fn changes(&self, id: &str) -> Result<Vec<FilesystemChange>, SandboxError>;
    async fn top(&self, id: &str) -> Result<ContainerTopResponse, SandboxError>;
}

/// Map a bollard error. Connection, socket, and timeout failures mean the
/// daemon is unreachable; a 404 means the object is gone.
pub fn map_error(op: &'static str, err: bollard::errors::Error) -> SandboxError {
    use bollard::errors::Error as E;
    match err {
        E::IOError { .. }
        | E::HyperResponseError { .. }
        | E::HyperLegacyError { .. }
        | E::RequestTimeoutError
        | E::SocketNotFoundError(_) => SandboxError::DaemonUnavailable,
        E::DockerResponseServerError {
            status_code: 404,
            message,
        } => SandboxError::NotFound(format!("{op}: {message}")),
        source => SandboxError::Docker { op, source },
    }
}

/// Split `repo[:tag]` into (`repo`, `tag`), defaulting the tag to `latest`.
/// Docker pulls every tag when none is given, which is never what we want.
/// Digest references (`@sha256:..`) are returned whole with no tag.
pub fn split_image_tag(image: &str) -> (String, Option<String>) {
    if image.contains('@') {
        return (image.to_owned(), None);
    }
    let after_slash = image.rfind('/').map_or(0, |i| i + 1);
    match image[after_slash..].rfind(':') {
        Some(i) => {
            let split = after_slash + i;
            (image[..split].to_owned(), Some(image[split + 1..].to_owned()))
        }
        None => (image.to_owned(), Some("latest".to_owned())),
    }
}

/// [`DockerApi`] over the local Docker socket.
#[derive(Clone)]
pub struct BollardDocker {
    docker: Docker,
}

impl BollardDocker {
    /// Connects lazily; no request is made until the first call. Honours the
    /// default socket path resolution of `connect_with_socket_defaults`.
    pub fn connect() -> Result<Self, SandboxError> {
        let docker =
            Docker::connect_with_socket_defaults().map_err(|e| map_error("connect", e))?;
        Ok(Self { docker })
    }
}

#[async_trait]
impl DockerApi for BollardDocker {
    async fn inspect_image(&self, image: &str) -> Result<ImageInspect, SandboxError> {
        self.docker
            .inspect_image(image)
            .await
            .map_err(|e| map_error("inspect image", e))
    }

    async fn pull_image(
        &self,
        image: &str,
        progress: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), SandboxError> {
        let (repo, tag) = split_image_tag(image);
        let mut builder = CreateImageOptionsBuilder::new().from_image(&repo);
        if let Some(tag) = tag {
            builder = builder.tag(&tag);
        }
        let mut stream = Box::pin(self.docker.create_image(Some(builder.build()), None, None));
        while let Some(item) = stream.next().await {
            let info = item.map_err(|e| map_error("pull image", e))?;
            if let Some(status) = info.status {
                progress(match info.id {
                    Some(id) => format!("{id}: {status}"),
                    None => status,
                });
            }
        }
        Ok(())
    }

    async fn create_volume(&self, name: &str) -> Result<(), SandboxError> {
        let req = VolumeCreateRequest {
            name: Some(name.to_owned()),
            ..Default::default()
        };
        self.docker
            .create_volume(req)
            .await
            .map(|_| ())
            .map_err(|e| map_error("create volume", e))
    }

    async fn remove_volume(&self, name: &str) -> Result<(), SandboxError> {
        self.docker
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
            .map_err(|e| map_error("remove volume", e))
    }

    async fn create_container(
        &self,
        name: &str,
        body: ContainerCreateBody,
    ) -> Result<String, SandboxError> {
        let options = CreateContainerOptionsBuilder::new().name(name).build();
        self.docker
            .create_container(Some(options), body)
            .await
            .map(|r| r.id)
            .map_err(|e| map_error("create container", e))
    }

    async fn start_container(&self, id: &str) -> Result<(), SandboxError> {
        self.docker
            .start_container(id, None)
            .await
            .map_err(|e| map_error("start container", e))
    }

    async fn stop_container(&self, id: &str) -> Result<(), SandboxError> {
        let options = StopContainerOptionsBuilder::new().t(10).build();
        self.docker
            .stop_container(id, Some(options))
            .await
            .map_err(|e| map_error("stop container", e))
    }

    async fn remove_container(&self, id: &str) -> Result<(), SandboxError> {
        let options = RemoveContainerOptionsBuilder::new().force(false).build();
        self.docker
            .remove_container(id, Some(options))
            .await
            .map_err(|e| map_error("remove container", e))
    }

    async fn inspect_container(
        &self,
        id: &str,
    ) -> Result<ContainerInspectResponse, SandboxError> {
        self.docker
            .inspect_container(id, None)
            .await
            .map_err(|e| map_error("inspect container", e))
    }

    async fn list_managed_containers(&self) -> Result<Vec<ContainerSummary>, SandboxError> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        filters.insert(
            "label".to_owned(),
            vec![format!("{LABEL_MANAGED_BY}={LABEL_MANAGED_VALUE}")],
        );
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        self.docker
            .list_containers(Some(options))
            .await
            .map_err(|e| map_error("list containers", e))
    }

    async fn create_exec(&self, container: &str, spec: ExecSpec) -> Result<String, SandboxError> {
        let options = CreateExecOptions::<String> {
            attach_stdin: Some(spec.attach_stdin),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(spec.tty),
            env: if spec.env.is_empty() { None } else { Some(spec.env) },
            cmd: Some(spec.cmd),
            ..Default::default()
        };
        self.docker
            .create_exec(container, options)
            .await
            .map(|r| r.id)
            .map_err(|e| map_error("create exec", e))
    }

    async fn start_exec(&self, exec_id: &str, tty: bool) -> Result<ExecIo, SandboxError> {
        let options = StartExecOptions {
            detach: false,
            tty,
            output_capacity: None,
        };
        match self
            .docker
            .start_exec(exec_id, Some(options))
            .await
            .map_err(|e| map_error("start exec", e))?
        {
            StartExecResults::Attached { output, input } => Ok(ExecIo {
                output: Box::pin(output.map(|r| r.map_err(|e| map_error("exec output", e)))),
                input,
            }),
            StartExecResults::Detached => Err(SandboxError::NotFound(
                "exec started detached; expected an attached stream".to_owned(),
            )),
        }
    }

    async fn resize_exec(
        &self,
        exec_id: &str,
        width: u16,
        height: u16,
    ) -> Result<(), SandboxError> {
        self.docker
            .resize_exec(exec_id, bollard::exec::ResizeExecOptions { height, width })
            .await
            .map_err(|e| map_error("resize exec", e))
    }

    async fn inspect_exec(&self, exec_id: &str) -> Result<ExecInspectResponse, SandboxError> {
        self.docker
            .inspect_exec(exec_id)
            .await
            .map_err(|e| map_error("inspect exec", e))
    }

    async fn stats_once(&self, id: &str) -> Result<ContainerStatsResponse, SandboxError> {
        // `stream(false)` without `one_shot`: the daemon waits for a second
        // sample so `precpu_stats` is populated, which the CPU % delta needs.
        let options = StatsOptionsBuilder::new().stream(false).build();
        let mut stream = Box::pin(self.docker.stats(id, Some(options)));
        match stream.next().await {
            Some(Ok(stats)) => Ok(stats),
            Some(Err(e)) => Err(map_error("stats", e)),
            None => Err(SandboxError::NotFound(format!("stats: no sample for {id}"))),
        }
    }

    async fn changes(&self, id: &str) -> Result<Vec<FilesystemChange>, SandboxError> {
        self.docker
            .container_changes(id)
            .await
            .map(Option::unwrap_or_default)
            .map_err(|e| map_error("changes", e))
    }

    async fn top(&self, id: &str) -> Result<ContainerTopResponse, SandboxError> {
        self.docker
            .top_processes(id, None)
            .await
            .map_err(|e| map_error("top", e))
    }
}

/// Inspect `container` and verify it carries `managed-by=mate` and
/// `session-id=<session_id>`. Returns the inspect data on success, so callers
/// can reuse it. Anything else is [`SandboxError::NotOurs`].
pub async fn verify_ownership(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
) -> Result<ContainerInspectResponse, SandboxError> {
    let inspect = docker.inspect_container(container).await?;
    if labels_match(&inspect, session_id) {
        Ok(inspect)
    } else {
        Err(SandboxError::NotOurs(container.to_owned()))
    }
}

/// True when the inspected container carries both ownership labels with the
/// expected values.
pub fn labels_match(inspect: &ContainerInspectResponse, session_id: &Uuid) -> bool {
    let Some(labels) = inspect.config.as_ref().and_then(|c| c.labels.as_ref()) else {
        return false;
    };
    labels.get(LABEL_MANAGED_BY).map(String::as_str) == Some(LABEL_MANAGED_VALUE)
        && labels.get(LABEL_SESSION_ID).map(String::as_str)
            == Some(session_id.to_string().as_str())
}

/// Stop a container after verifying ownership.
pub async fn stop_owned(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
) -> Result<(), SandboxError> {
    verify_ownership(docker, container, session_id).await?;
    docker.stop_container(container).await
}

/// Remove a container after verifying ownership.
pub async fn remove_owned(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
) -> Result<(), SandboxError> {
    verify_ownership(docker, container, session_id).await?;
    docker.remove_container(container).await
}

/// Create an exec in a container after verifying ownership.
pub async fn create_exec_owned(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
    spec: ExecSpec,
) -> Result<String, SandboxError> {
    verify_ownership(docker, container, session_id).await?;
    docker.create_exec(container, spec).await
}

/// Cap on captured stdout and on captured stderr of a one-shot exec.
pub const CAPTURE_CAP: usize = 1024 * 1024;

/// Result of a one-shot, non-tty exec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
}

fn append_capped(buf: &mut Vec<u8>, chunk: &[u8]) {
    let room = CAPTURE_CAP.saturating_sub(buf.len());
    buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
}

/// Run `cmd` in an owned container with no tty and no stdin, capturing
/// demultiplexed stdout and stderr (each capped at [`CAPTURE_CAP`]) and the
/// exit code.
pub async fn exec_collect(
    docker: &dyn DockerApi,
    container: &str,
    session_id: &Uuid,
    cmd: Vec<String>,
) -> Result<ExecOutcome, SandboxError> {
    let spec = ExecSpec {
        cmd,
        ..Default::default()
    };
    let exec = create_exec_owned(docker, container, session_id, spec).await?;
    let mut io = docker.start_exec(&exec, false).await?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while let Some(chunk) = io.output.next().await {
        match chunk? {
            LogOutput::StdOut { message } | LogOutput::Console { message } => {
                append_capped(&mut out, &message)
            }
            LogOutput::StdErr { message } => append_capped(&mut err, &message),
            LogOutput::StdIn { .. } => {}
        }
    }
    let exit_code = docker.inspect_exec(&exec).await?.exit_code;
    Ok(ExecOutcome {
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        exit_code,
    })
}

/// Make sure `image` exists locally, pulling it (with progress) if not.
pub async fn ensure_image(
    docker: &dyn DockerApi,
    image: &str,
    progress: &mut (dyn FnMut(String) + Send),
) -> Result<(), SandboxError> {
    match docker.inspect_image(image).await {
        Ok(_) => Ok(()),
        Err(SandboxError::NotFound(_)) => docker.pull_image(image, progress).await,
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDocker, labelled_inspect};

    #[test]
    fn io_error_maps_to_daemon_unavailable() {
        let err = bollard::errors::Error::IOError {
            err: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        };
        assert!(
            matches!(map_error("x", err), SandboxError::DaemonUnavailable),
            "a refused socket connection means the daemon is unreachable"
        );
        assert!(
            matches!(
                map_error("x", bollard::errors::Error::RequestTimeoutError),
                SandboxError::DaemonUnavailable
            ),
            "a timeout means the daemon is unreachable"
        );
    }

    #[test]
    fn http_404_maps_to_not_found() {
        let err = bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "no such container".into(),
        };
        assert!(
            matches!(map_error("inspect", err), SandboxError::NotFound(_)),
            "a 404 means the object is gone"
        );
    }

    #[test]
    fn other_server_errors_keep_op_and_source() {
        let err = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "boom".into(),
        };
        assert!(
            matches!(map_error("start", err), SandboxError::Docker { op: "start", .. }),
            "other failures are wrapped with the operation name"
        );
    }

    #[test]
    fn image_tag_split() {
        assert_eq!(
            split_image_tag("ghcr.io/leoborai/dkc:latest"),
            ("ghcr.io/leoborai/dkc".into(), Some("latest".into())),
            "explicit tag is split off"
        );
        assert_eq!(
            split_image_tag("alpine"),
            ("alpine".into(), Some("latest".into())),
            "no tag defaults to latest, never pull-all-tags"
        );
        assert_eq!(
            split_image_tag("localhost:5000/app"),
            ("localhost:5000/app".into(), Some("latest".into())),
            "a registry port is not a tag"
        );
        assert_eq!(
            split_image_tag("localhost:5000/app:v2"),
            ("localhost:5000/app".into(), Some("v2".into())),
            "tag after a registry port still splits"
        );
        assert_eq!(
            split_image_tag("alpine@sha256:abc"),
            ("alpine@sha256:abc".into(), None),
            "digest references are left whole"
        );
    }

    #[tokio::test]
    async fn guard_refuses_missing_or_mismatched_labels() {
        let id = Uuid::from_u128(7);
        let other = Uuid::from_u128(8);
        let fake = FakeDocker::default();
        fake.add("unlabelled", labelled_inspect(&[], "running"));
        fake.add("wrong-id", labelled_inspect(&[("managed-by", "mate"), ("session-id", &other.to_string())], "running"));
        fake.add("not-mate", labelled_inspect(&[("managed-by", "other"), ("session-id", &id.to_string())], "running"));
        for c in ["unlabelled", "wrong-id", "not-mate"] {
            assert!(
                matches!(stop_owned(&fake, c, &id).await, Err(SandboxError::NotOurs(_))),
                "{c} lacks the exact ownership labels so stop must be refused"
            );
            assert!(
                matches!(remove_owned(&fake, c, &id).await, Err(SandboxError::NotOurs(_))),
                "{c}: remove must be refused"
            );
            assert!(
                matches!(
                    create_exec_owned(&fake, c, &id, ExecSpec::default()).await,
                    Err(SandboxError::NotOurs(_))
                ),
                "{c}: exec must be refused"
            );
        }
        assert!(
            fake.calls().iter().all(|c| !c.starts_with("stop") && !c.starts_with("remove") && !c.starts_with("create_exec")),
            "no mutating call reached Docker for a container that is not ours"
        );
    }

    #[tokio::test]
    async fn guard_allows_owned_container() {
        let id = Uuid::from_u128(7);
        let fake = FakeDocker::default();
        fake.add(
            "mine",
            labelled_inspect(&[("managed-by", "mate"), ("session-id", &id.to_string())], "running"),
        );
        stop_owned(&fake, "mine", &id).await.unwrap();
        assert!(
            fake.calls().contains(&"stop mine".to_owned()),
            "an owned container is stopped after the label check"
        );
    }
}
