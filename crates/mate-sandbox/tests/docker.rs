//! Live Docker integration test. Ignored by default; opt in with
//! `DKC_DOCKER_TESTS=1 cargo test -p mate-sandbox -- --ignored`.

use futures::StreamExt;
use mate_sandbox::docker::{
    BollardDocker, DockerApi, ExecSpec, create_exec_owned, ensure_image, remove_owned, stop_owned,
};
use mate_sandbox::policy::{SandboxPolicy, validate_policy};
use mate_sandbox::spec::{ImageIdentity, build_container_spec};
use uuid::Uuid;

const IMAGE: &str = "alpine:3";

#[tokio::test]
#[ignore = "needs a Docker daemon; set DKC_DOCKER_TESTS=1"]
async fn container_lifecycle_and_hardening() {
    if std::env::var("DKC_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: DKC_DOCKER_TESTS=1 not set");
        return;
    }
    let Ok(docker) = BollardDocker::connect() else {
        eprintln!("skipping: could not build a Docker client");
        return;
    };
    if docker.list_managed_containers().await.is_err() {
        eprintln!("skipping: Docker daemon unreachable");
        return;
    }

    ensure_image(&docker, IMAGE, &mut |line| eprintln!("pull: {line}"))
        .await
        .unwrap();

    let workspace = tempfile::tempdir().unwrap();
    let policy = SandboxPolicy::new(workspace.path());
    validate_policy(&policy, std::env::var("DOCKER_HOST").ok().as_deref()).unwrap();

    let id = Uuid::new_v4();
    let identity = ImageIdentity {
        user: None,
        home: "/root".into(),
        runs_as_root: true,
    };
    let spec = build_container_spec(id, IMAGE, &identity, &policy);
    docker.create_volume(&spec.volume).await.unwrap();
    let container = docker
        .create_container(&spec.name, spec.body.clone())
        .await
        .unwrap();
    docker.start_container(&container).await.unwrap();

    // exec `echo hi`
    let exec = create_exec_owned(
        &docker,
        &container,
        &id,
        ExecSpec {
            cmd: vec!["echo".into(), "hi".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut io = docker.start_exec(&exec, false).await.unwrap();
    let mut out = String::new();
    while let Some(chunk) = io.output.next().await {
        out.push_str(&chunk.unwrap().to_string());
    }
    let done = docker.inspect_exec(&exec).await.unwrap();
    assert_eq!(out.trim(), "hi", "exec output is demultiplexed stdout");
    assert_eq!(done.exit_code, Some(0), "echo exits 0");

    // inspect: labels and hardening
    let inspect = docker.inspect_container(&container).await.unwrap();
    let labels = inspect.config.unwrap().labels.unwrap();
    assert_eq!(labels["managed-by"], "mate", "ownership label applied");
    assert_eq!(labels["session-id"], id.to_string(), "session label applied");
    let host = inspect.host_config.unwrap();
    assert_eq!(host.cap_drop, Some(vec!["ALL".into()]), "all caps dropped");
    assert_eq!(host.privileged, Some(false), "not privileged");
    assert!(
        host.security_opt.unwrap().iter().any(|o| o.starts_with("no-new-privileges")),
        "no-new-privileges is set"
    );

    stop_owned(&docker, &container, &id).await.unwrap();
    remove_owned(&docker, &container, &id).await.unwrap();
    docker.remove_volume(&spec.volume).await.unwrap();
}
