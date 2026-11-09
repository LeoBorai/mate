//! The persisted sandbox session model.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::policy::SandboxPolicy;

/// Label key marking containers owned by mate.
pub const LABEL_MANAGED_BY: &str = "managed-by";
/// Label value for [`LABEL_MANAGED_BY`].
pub const LABEL_MANAGED_VALUE: &str = "mate";
/// Label key carrying the session id.
pub const LABEL_SESSION_ID: &str = "session-id";

/// Lifecycle state of a sandbox container, as last reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Running,
    Exited,
    Created,
    /// Container is gone or not ours.
    Missing,
}

/// One sandbox: a container plus a Claude Code conversation.
///
/// Deliberately has no field that could hold a credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSession {
    pub id: Uuid,
    pub name: String,
    pub container_id: String,
    pub image: String,
    pub workspace: PathBuf,
    pub policy: SandboxPolicy,
    pub claude_session_id: Option<Uuid>,
    pub status: SandboxStatus,
    /// Unix seconds.
    pub created_at: i64,
}

impl SandboxSession {
    /// `dkc-<first 8 hex of the id>`.
    pub fn container_name(id: &Uuid) -> String {
        format!("dkc-{}", &id.simple().to_string()[..8])
    }

    /// `dkc-claude-<id>`.
    pub fn volume_name(id: &Uuid) -> String {
        format!("dkc-claude-{id}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> SandboxSession {
        let id = Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap();
        SandboxSession {
            id,
            name: SandboxSession::container_name(&id),
            container_id: "abc123".into(),
            image: "ghcr.io/leoborai/dkc:latest".into(),
            workspace: "/tmp/ws".into(),
            policy: SandboxPolicy::new("/tmp/ws"),
            claude_session_id: None,
            status: SandboxStatus::Running,
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn serde_round_trip() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        let back: SandboxSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s, "a session survives a JSON round trip unchanged");
    }

    #[test]
    fn json_keys_are_exactly_the_expected_set() {
        let v = serde_json::to_value(sample()).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "claude_session_id", "container_id", "created_at", "id", "image", "name",
                "policy", "status", "workspace"
            ],
            "the persisted shape has no extra fields (so nothing credential-like can leak in)"
        );
    }

    #[test]
    fn names_derive_from_id() {
        let id = sample().id;
        assert_eq!(SandboxSession::container_name(&id), "dkc-11111111", "dkc-<8 hex>");
        assert_eq!(
            SandboxSession::volume_name(&id),
            "dkc-claude-11111111-2222-4333-8444-555555555555",
            "volume is keyed by the full uuid"
        );
    }
}
