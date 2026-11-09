//! In-memory [`DockerApi`] for unit tests.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ContainerInspectResponse, ContainerState,
    ContainerStateStatusEnum, ContainerStatsResponse, ContainerSummary, ContainerTopResponse,
    ExecInspectResponse, FilesystemChange, ImageInspect,
};

use crate::docker::{DockerApi, ExecIo, ExecSpec};
use crate::error::SandboxError;

#[derive(Default)]
pub(crate) struct FakeDocker {
    containers: Mutex<HashMap<String, ContainerInspectResponse>>,
    calls: Mutex<Vec<String>>,
    pub daemon_down: Mutex<bool>,
}

impl FakeDocker {
    pub fn add(&self, id: &str, inspect: ContainerInspectResponse) {
        self.containers.lock().unwrap().insert(id.to_owned(), inspect);
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
pub(crate) fn labelled_inspect(labels: &[(&str, &str)], state: &str) -> ContainerInspectResponse {
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
    async fn inspect_image(&self, _: &str) -> Result<ImageInspect, SandboxError> {
        unsupported()
    }
    async fn pull_image(
        &self,
        _: &str,
        _: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), SandboxError> {
        unsupported()
    }
    async fn create_volume(&self, _: &str) -> Result<(), SandboxError> {
        unsupported()
    }
    async fn remove_volume(&self, name: &str) -> Result<(), SandboxError> {
        self.record(format!("remove_volume {name}"))
    }
    async fn create_container(&self, _: &str, _: ContainerCreateBody) -> Result<String, SandboxError> {
        unsupported()
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
    async fn create_exec(&self, container: &str, _: ExecSpec) -> Result<String, SandboxError> {
        self.record(format!("create_exec {container}"))?;
        Ok("exec-1".to_owned())
    }
    async fn start_exec(&self, _: &str, _: bool) -> Result<ExecIo, SandboxError> {
        unsupported()
    }
    async fn resize_exec(&self, _: &str, _: u16, _: u16) -> Result<(), SandboxError> {
        unsupported()
    }
    async fn inspect_exec(&self, _: &str) -> Result<ExecInspectResponse, SandboxError> {
        unsupported()
    }
    async fn stats_once(&self, _: &str) -> Result<ContainerStatsResponse, SandboxError> {
        unsupported()
    }
    async fn changes(&self, _: &str) -> Result<Vec<FilesystemChange>, SandboxError> {
        unsupported()
    }
    async fn top(&self, _: &str) -> Result<ContainerTopResponse, SandboxError> {
        unsupported()
    }
}
