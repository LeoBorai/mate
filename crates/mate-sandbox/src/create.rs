//! Creating a sandbox: image, state volume, container, start, then a probe of the image
//! contract. Every Docker call goes through [`DockerApi`], so the order is unit-tested against
//! the in-memory fake.

use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::docker::{DockerApi, ensure_image, exec_collect};
use crate::error::SandboxError;
use crate::policy::{SandboxPolicy, validate_policy};
use crate::session::{SandboxSession, SandboxStatus};
use crate::spec::{build_container_spec, image_identity};

/// A created and started sandbox, plus anything the image contract probe found missing. The
/// container is left in place when the contract is not met, so the user can inspect it.
#[derive(Debug)]
pub struct Created {
    pub session: SandboxSession,
    /// Human-readable notes: an image that runs as root, a missing `tmux`, an unwritable
    /// Claude config directory. Empty when the image meets the contract.
    pub contract_issues: Vec<String>,
}

/// Command the contract probe runs for `tmux`.
pub const PROBE_TMUX: [&str; 2] = ["tmux", "-V"];
/// Command the contract probe runs for the Claude config directory.
pub const PROBE_CONFIG_DIR: [&str; 3] = ["sh", "-c", "test -w \"$CLAUDE_CONFIG_DIR\""];

/// Validates the policy, makes `image` available (building the embedded one, pulling any
/// other), then creates the volume and container, starts it, and probes the contract. The
/// policy and image are checked before anything is created, so a refusal leaves no container
/// or volume.
pub async fn create_session(
    docker: &dyn DockerApi,
    id: Uuid,
    image: &str,
    policy: SandboxPolicy,
    progress: &mut (dyn FnMut(String) + Send),
) -> Result<Created, SandboxError> {
    validate_policy(&policy, std::env::var("DOCKER_HOST").ok().as_deref())?;
    ensure_image(docker, image, progress).await?;
    let identity = image_identity(&docker.inspect_image(image).await?);
    let mut contract_issues = Vec::new();
    if identity.runs_as_root {
        contract_issues.push("the image runs as root; a non-root user is expected".to_owned());
    }

    let spec = build_container_spec(id, image, &identity, &policy);
    docker.create_volume(&spec.volume).await?;
    let container_id = docker.create_container(&spec.name, spec.body.clone()).await?;
    docker.start_container(&container_id).await?;

    let probe = |cmd: &[&str]| cmd.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ok = |outcome: Result<crate::docker::ExecOutcome, SandboxError>| {
        matches!(outcome, Ok(o) if o.exit_code == Some(0))
    };
    if !ok(exec_collect(docker, &container_id, &id, probe(&PROBE_TMUX)).await) {
        contract_issues.push("tmux is not installed in the image".to_owned());
    }
    if !ok(exec_collect(docker, &container_id, &id, probe(&PROBE_CONFIG_DIR)).await) {
        contract_issues.push(
            "the Claude config directory is not writable by the image user \
             (its ~/.claude must exist and be owned by that user)"
                .to_owned(),
        );
    }

    let session = SandboxSession {
        id,
        name: spec.name,
        container_id,
        image: image.to_owned(),
        workspace: policy.workspace.clone(),
        policy,
        claude_session_id: None,
        status: SandboxStatus::Running,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    Ok(Created {
        session,
        contract_issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDocker;

    fn fake_with_contract_met(image: &str) -> FakeDocker {
        let fake = FakeDocker::default();
        fake.images.lock().unwrap().insert(image.to_owned());
        fake.script(&PROBE_TMUX.join(" "), "tmux 3.3", "", 0);
        fake.script(&PROBE_CONFIG_DIR.join(" "), "", "", 0);
        fake
    }

    #[tokio::test]
    async fn creating_builds_the_volume_and_container_then_starts_and_probes() {
        let ws = tempfile::tempdir().unwrap();
        let fake = fake_with_contract_met("img:1");
        let id = Uuid::from_u128(0xaabbccdd_0000_4000_8000_000000000001);
        let created = create_session(&fake, id, "img:1", SandboxPolicy::new(ws.path()), &mut |_| {})
            .await
            .unwrap();
        let calls = fake.calls();
        let pos = |needle: &str| {
            calls
                .iter()
                .position(|c| c.starts_with(needle))
                .unwrap_or_else(|| panic!("no `{needle}` call in {calls:?}"))
        };
        assert!(
            pos("create_volume") < pos("create_container") && pos("create_container") < pos("start"),
            "volume, then container, then start, got {calls:?}"
        );
        assert_eq!(created.session.name, "dkc-aabbccdd", "container name is dkc-<8 hex>");
        assert_eq!(created.session.status, SandboxStatus::Running, "a started sandbox is running");
        assert_eq!(created.session.claude_session_id, None, "no Claude session yet");
        assert!(
            created.contract_issues.iter().all(|i| i.contains("root")),
            "a met contract leaves no tmux or config-dir issue, got {:?}",
            created.contract_issues
        );
    }

    #[tokio::test]
    async fn a_missing_tmux_or_unwritable_config_dir_is_reported_and_the_container_stays() {
        let ws = tempfile::tempdir().unwrap();
        let fake = FakeDocker::default();
        fake.images.lock().unwrap().insert("img:1".to_owned());
        let created = create_session(
            &fake,
            Uuid::from_u128(1),
            "img:1",
            SandboxPolicy::new(ws.path()),
            &mut |_| {},
        )
        .await
        .unwrap();
        let issues = created.contract_issues.join("\n");
        assert!(issues.contains("tmux"), "the missing tmux is named, got {issues}");
        assert!(issues.contains("not writable"), "the config dir problem is named, got {issues}");
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("remove")),
            "the container is left in place for inspection"
        );
    }

    #[tokio::test]
    async fn a_missing_workspace_is_refused_before_anything_is_created() {
        let fake = FakeDocker::default();
        let err = create_session(
            &fake,
            Uuid::from_u128(1),
            "img:1",
            SandboxPolicy::new("/does/not/exist"),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidPolicy(_)), "got {err:?}");
        assert!(fake.calls().is_empty(), "no Docker call is made for an invalid policy");
    }

    #[tokio::test]
    async fn a_build_failure_creates_no_volume_or_container() {
        let ws = tempfile::tempdir().unwrap();
        let fake = FakeDocker::default();
        *fake.build_error.lock().unwrap() = Some("boom".into());
        let tag = crate::image::embedded_tag();
        let err = create_session(&fake, Uuid::from_u128(1), &tag, SandboxPolicy::new(ws.path()), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::Docker { op: "build_image", .. }), "got {err:?}");
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("create_")),
            "nothing is created after a failed build, got {:?}",
            fake.calls()
        );
    }
}
