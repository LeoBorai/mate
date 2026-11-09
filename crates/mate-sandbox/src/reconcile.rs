//! Startup reconciliation of stored sessions against Docker.

use bollard::models::ContainerStateStatusEnum;

use crate::docker::{DockerApi, verify_ownership};
use crate::error::SandboxError;
use crate::session::{SandboxSession, SandboxStatus};

/// Map Docker's container state onto [`SandboxStatus`].
fn map_state(status: Option<ContainerStateStatusEnum>) -> SandboxStatus {
    use ContainerStateStatusEnum as S;
    match status {
        Some(S::RUNNING | S::PAUSED | S::RESTARTING) => SandboxStatus::Running,
        Some(S::CREATED) => SandboxStatus::Created,
        _ => SandboxStatus::Exited,
    }
}

/// Reconcile one session. A container that is gone (404) or not ours (label
/// mismatch) is [`SandboxStatus::Missing`]; an unreachable daemon is an error
/// so the tab shows it instead of a guessed status.
pub async fn reconcile_one(
    docker: &dyn DockerApi,
    session: &SandboxSession,
) -> Result<SandboxStatus, SandboxError> {
    match verify_ownership(docker, &session.container_id, &session.id).await {
        Ok(inspect) => Ok(map_state(inspect.state.and_then(|s| s.status))),
        Err(SandboxError::NotFound(_) | SandboxError::NotOurs(_)) => Ok(SandboxStatus::Missing),
        Err(e) => Err(e),
    }
}

/// Reconcile every session. The returned session carries the new status when
/// reconciliation succeeded, and is unchanged otherwise.
pub async fn reconcile(
    docker: &dyn DockerApi,
    sessions: Vec<SandboxSession>,
) -> Vec<(SandboxSession, Result<SandboxStatus, SandboxError>)> {
    let mut out = Vec::with_capacity(sessions.len());
    for mut session in sessions {
        let result = reconcile_one(docker, &session).await;
        if let Ok(status) = &result {
            session.status = *status;
        }
        out.push((session, result));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDocker, labelled_inspect};
    use crate::session::tests::sample;

    fn owned(state: &str) -> bollard::models::ContainerInspectResponse {
        let s = sample();
        labelled_inspect(
            &[("managed-by", "mate"), ("session-id", &s.id.to_string())],
            state,
        )
    }

    async fn status_for(fake: &FakeDocker) -> Result<SandboxStatus, SandboxError> {
        reconcile_one(fake, &sample()).await
    }

    #[tokio::test]
    async fn maps_docker_states() {
        for (state, want) in [
            ("running", SandboxStatus::Running),
            ("exited", SandboxStatus::Exited),
            ("created", SandboxStatus::Created),
        ] {
            let fake = FakeDocker::default();
            fake.add(&sample().container_id, owned(state));
            assert_eq!(
                status_for(&fake).await.unwrap(),
                want,
                "docker state {state} maps to {want:?}"
            );
        }
    }

    #[tokio::test]
    async fn missing_container_is_missing() {
        let fake = FakeDocker::default();
        assert_eq!(
            status_for(&fake).await.unwrap(),
            SandboxStatus::Missing,
            "a 404 on inspect means the container was removed outside mate"
        );
    }

    #[tokio::test]
    async fn label_mismatch_is_missing() {
        let fake = FakeDocker::default();
        fake.add(&sample().container_id, labelled_inspect(&[], "running"));
        assert_eq!(
            status_for(&fake).await.unwrap(),
            SandboxStatus::Missing,
            "a container without our labels is treated as missing, never touched"
        );
        assert_eq!(
            fake.calls(),
            vec![format!("inspect {}", sample().container_id)],
            "only an inspect was issued"
        );
    }

    #[tokio::test]
    async fn daemon_unavailable_is_kept_per_session() {
        let fake = FakeDocker::default();
        *fake.daemon_down.lock().unwrap() = true;
        let results = reconcile(&fake, vec![sample()]).await;
        assert_eq!(results.len(), 1, "one result per stored session");
        assert!(
            matches!(results[0].1, Err(SandboxError::DaemonUnavailable)),
            "the error is reported instead of a guessed status"
        );
        assert_eq!(
            results[0].0.status,
            sample().status,
            "the stored status is left unchanged when reconcile fails"
        );
    }

    #[tokio::test]
    async fn reconcile_updates_session_status() {
        let fake = FakeDocker::default();
        fake.add(&sample().container_id, owned("exited"));
        let results = reconcile(&fake, vec![sample()]).await;
        assert_eq!(
            results[0].0.status,
            SandboxStatus::Exited,
            "the returned session carries the reconciled status"
        );
    }
}
