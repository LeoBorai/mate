//! In-memory [`DockerApi`] for tests, here and (via the `test-support` feature) in other crates.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use bollard::container::LogOutput;
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ContainerInspectResponse, ContainerState,
    ContainerStateStatusEnum, ContainerStatsResponse, ContainerSummary, ContainerTopResponse,
    ExecInspectResponse, FilesystemChange, ImageInspect,
};

use crate::docker::{DockerApi, ExecIo, ExecSpec};
use crate::error::SandboxError;

#[derive(Default)]
pub struct FakeDocker {
    containers: Mutex<HashMap<String, ContainerInspectResponse>>,
    calls: Mutex<Vec<String>>,
    pub daemon_down: Mutex<bool>,
    /// Image references that exist locally.
    pub images: Mutex<HashSet<String>>,
    /// When set, `build_image` fails with this message.
    pub build_error: Mutex<Option<String>>,
    /// Scripted exec results keyed by the space-joined command:
    /// (stdout, stderr, exit code). Unscripted commands exit 1 silently.
    scripts: Mutex<HashMap<String, (String, String, i64)>>,
}

impl FakeDocker {
    pub fn add(&self, id: &str, inspect: ContainerInspectResponse) {
        self.containers.lock().unwrap().insert(id.to_owned(), inspect);
    }

    pub fn script(&self, cmd: &str, stdout: &str, stderr: &str, code: i64) {
        self.scripts
            .lock()
            .unwrap()
            .insert(cmd.to_owned(), (stdout.to_owned(), stderr.to_owned(), code));
    }

    fn scripted(&self, exec_id: &str) -> (String, String, i64) {
        self.scripts
            .lock()
            .unwrap()
            .get(exec_id)
            .cloned()
            .unwrap_or_else(|| (String::new(), String::new(), 1))
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) -> Result<(), SandboxError> {
        self.calls.lock().unwrap().push(call);
        if *self.daemon_down.lock().unwrap() {
            Err(SandboxError::DaemonUnavailable)
        } else {
            Ok(())
        }
    }
}

/// An inspect response with the given labels and state string
/// (`running`, `exited`, `created`).
pub fn labelled_inspect(labels: &[(&str, &str)], state: &str) -> ContainerInspectResponse {
    let status = match state {
        "running" => ContainerStateStatusEnum::RUNNING,
        "exited" => ContainerStateStatusEnum::EXITED,
        _ => ContainerStateStatusEnum::CREATED,
    };
    ContainerInspectResponse {
        config: Some(ContainerConfig {
            labels: Some(
                labels
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ),
            ..Default::default()
        }),
        state: Some(ContainerState {
            status: Some(status),
            running: Some(state == "running"),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn unsupported<T>() -> Result<T, SandboxError> {
    Err(SandboxError::NotFound("not supported by FakeDocker".into()))
}

#[async_trait]
impl DockerApi for FakeDocker {
    async fn inspect_image(&self, image: &str) -> Result<ImageInspect, SandboxError> {
        self.record(format!("inspect_image {image}"))?;
        if self.images.lock().unwrap().contains(image) {
            Ok(ImageInspect::default())
        } else {
            Err(SandboxError::NotFound(image.to_owned()))
        }
    }
    async fn pull_image(
        &self,
        image: &str,
        progress: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), SandboxError> {
        self.record(format!("pull {image}"))?;
        progress("pulled".to_owned());
        self.images.lock().unwrap().insert(image.to_owned());
        Ok(())
    }
    async fn build_image(
        &self,
        tag: &str,
        _: Vec<u8>,
        progress: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), SandboxError> {
        self.record(format!("build {tag}"))?;
        progress("Step 1/1".to_owned());
        if let Some(message) = self.build_error.lock().unwrap().clone() {
            return Err(SandboxError::Docker {
                op: "build_image",
                source: bollard::errors::Error::DockerResponseServerError {
                    status_code: 500,
                    message,
                },
            });
        }
        self.images.lock().unwrap().insert(tag.to_owned());
        Ok(())
    }
    async fn create_volume(&self, name: &str) -> Result<(), SandboxError> {
        self.record(format!("create_volume {name}"))
    }
    async fn remove_volume(&self, name: &str) -> Result<(), SandboxError> {
        self.record(format!("remove_volume {name}"))
    }
    async fn create_container(
        &self,
        name: &str,
        body: ContainerCreateBody,
    ) -> Result<String, SandboxError> {
        self.record(format!("create_container {name}"))?;
        let labels: Vec<(String, String)> = body.labels.unwrap_or_default().into_iter().collect();
        let labels: Vec<(&str, &str)> =
            labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let id = format!("container-{name}");
        self.add(&id, labelled_inspect(&labels, "running"));
        Ok(id)
    }
    async fn start_container(&self, id: &str) -> Result<(), SandboxError> {
        self.record(format!("start {id}"))
    }
    async fn stop_container(&self, id: &str) -> Result<(), SandboxError> {
        self.record(format!("stop {id}"))
    }
    async fn remove_container(&self, id: &str) -> Result<(), SandboxError> {
        self.record(format!("remove {id}"))
    }
    async fn inspect_container(&self, id: &str) -> Result<ContainerInspectResponse, SandboxError> {
        self.record(format!("inspect {id}"))?;
        self.containers
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| SandboxError::NotFound(id.to_owned()))
    }
    async fn list_managed_containers(&self) -> Result<Vec<ContainerSummary>, SandboxError> {
        self.record("list".to_owned())?;
        Ok(Vec::new())
    }
    async fn create_exec(&self, container: &str, spec: ExecSpec) -> Result<String, SandboxError> {
        self.record(format!("create_exec {container}"))?;
        let cmd = spec.cmd.join(" ");
        self.calls.lock().unwrap().push(format!("exec {cmd}"));
        Ok(cmd)
    }
    async fn start_exec(&self, exec_id: &str, _: bool) -> Result<ExecIo, SandboxError> {
        let (stdout, stderr, _) = self.scripted(exec_id);
        let chunks: Vec<Result<LogOutput, SandboxError>> = vec![
            Ok(LogOutput::StdOut { message: stdout.into_bytes().into() }),
            Ok(LogOutput::StdErr { message: stderr.into_bytes().into() }),
        ];
        Ok(ExecIo {
            output: Box::pin(futures::stream::iter(chunks)),
            input: Box::pin(tokio::io::sink()),
        })
    }
    async fn resize_exec(&self, _: &str, _: u16, _: u16) -> Result<(), SandboxError> {
        unsupported()
    }
    async fn inspect_exec(&self, exec_id: &str) -> Result<ExecInspectResponse, SandboxError> {
        let (_, _, code) = self.scripted(exec_id);
        Ok(ExecInspectResponse { exit_code: Some(code), ..Default::default() })
    }
    async fn stats_once(&self, id: &str) -> Result<ContainerStatsResponse, SandboxError> {
        self.record(format!("stats {id}"))?;
        Ok(ContainerStatsResponse::default())
    }
    async fn changes(&self, id: &str) -> Result<Vec<FilesystemChange>, SandboxError> {
        self.record(format!("changes {id}"))?;
        Ok(Vec::new())
    }
    async fn top(&self, id: &str) -> Result<ContainerTopResponse, SandboxError> {
        self.record(format!("top {id}"))?;
        Ok(ContainerTopResponse::default())
    }
}
