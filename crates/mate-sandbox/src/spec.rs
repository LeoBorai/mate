//! Pure construction of the container create request.

use std::collections::HashMap;

use bollard::models::{ContainerCreateBody, ImageInspect};
use uuid::Uuid;

use crate::policy::{SandboxPolicy, WORKSPACE_TARGET, policy_to_host_config};
use crate::session::{
    LABEL_MANAGED_BY, LABEL_MANAGED_VALUE, LABEL_SESSION_ID, SandboxSession,
};

/// Name of the env var that relocates all Claude Code state (credentials,
/// transcripts, and the account file) under one directory.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Who the image runs as and where its home is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageIdentity {
    /// `Config.User`, when set and not root.
    pub user: Option<String>,
    pub home: String,
    /// True when the image user is empty or root; the TUI warns.
    pub runs_as_root: bool,
}

/// Read `Config.User` and `HOME` from the image. Falls back to
/// `/home/<user>` when `HOME` is unset.
pub fn image_identity(image: &ImageInspect) -> ImageIdentity {
    let config = image.config.as_ref();
    let user = config
        .and_then(|c| c.user.clone())
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty());
    let home_env = config
        .and_then(|c| c.env.as_ref())
        .and_then(|env| env.iter().find_map(|e| e.strip_prefix("HOME=")))
        .map(str::to_owned);
    // `Config.User` may be `name`, `uid`, or `name:group`; the name part
    // decides the fallback home.
    let name = user.as_deref().map(|u| u.split(':').next().unwrap_or(u));
    let runs_as_root = matches!(name, None | Some("root") | Some("0"));
    let home = match (home_env, name) {
        (Some(h), _) => h,
        (None, Some(n)) if !runs_as_root => format!("/home/{n}"),
        _ => "/root".to_owned(),
    };
    ImageIdentity {
        user: if runs_as_root { None } else { user },
        home,
        runs_as_root,
    }
}

/// A fully specified container, ready for `create_container`.
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub volume: String,
    /// `CLAUDE_CONFIG_DIR` inside the container (and the volume mount point).
    pub config_dir: String,
    pub body: ContainerCreateBody,
}

/// Build the container request for a new session (design D5, D6).
///
/// The only env var is `CLAUDE_CONFIG_DIR`, a path. No credential is ever set.
pub fn build_container_spec(
    id: Uuid,
    image: &str,
    identity: &ImageIdentity,
    policy: &SandboxPolicy,
) -> ContainerSpec {
    let name = SandboxSession::container_name(&id);
    let volume = SandboxSession::volume_name(&id);
    let config_dir = format!("{}/.claude", identity.home.trim_end_matches('/'));
    let labels = HashMap::from([
        (LABEL_MANAGED_BY.to_owned(), LABEL_MANAGED_VALUE.to_owned()),
        (LABEL_SESSION_ID.to_owned(), id.to_string()),
    ]);
    let body = ContainerCreateBody {
        image: Some(image.to_owned()),
        cmd: Some(vec!["sleep".to_owned(), "infinity".to_owned()]),
        tty: Some(true),
        open_stdin: Some(true),
        working_dir: Some(WORKSPACE_TARGET.to_owned()),
        user: identity.user.clone(),
        env: Some(vec![format!("{CLAUDE_CONFIG_DIR_ENV}={config_dir}")]),
        labels: Some(labels),
        host_config: Some(policy_to_host_config(policy, &volume, &config_dir)),
        ..Default::default()
    };
    ContainerSpec {
        name,
        volume,
        config_dir,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ImageConfig;

    fn image(user: Option<&str>, env: &[&str]) -> ImageInspect {
        ImageInspect {
            config: Some(ImageConfig {
                user: user.map(str::to_owned),
                env: Some(env.iter().map(|s| (*s).to_owned()).collect()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn identity_uses_home_env_and_user() {
        let id = image_identity(&image(Some("dev"), &["PATH=/bin", "HOME=/home/dev"]));
        assert_eq!(
            id,
            ImageIdentity { user: Some("dev".into()), home: "/home/dev".into(), runs_as_root: false },
            "user and HOME come straight from the image config"
        );
    }

    #[test]
    fn identity_falls_back_to_home_dir_of_user() {
        let id = image_identity(&image(Some("dev:staff"), &[]));
        assert_eq!(id.home, "/home/dev", "HOME unset falls back to /home/<user>");
    }

    #[test]
    fn identity_flags_root() {
        for u in [None, Some(""), Some("root"), Some("0")] {
            let id = image_identity(&image(u, &[]));
            assert!(id.runs_as_root, "user {u:?} counts as root");
            assert_eq!(id.user, None, "root images run with the image default user");
        }
    }

    #[test]
    fn spec_has_exact_labels_names_and_env() {
        let uid = Uuid::parse_str("aabbccdd-1111-4222-8333-444444444444").unwrap();
        let identity = ImageIdentity { user: Some("dev".into()), home: "/home/dev".into(), runs_as_root: false };
        let spec = build_container_spec(uid, "img:1", &identity, &SandboxPolicy::new("/ws"));
        assert_eq!(spec.name, "dkc-aabbccdd", "name format is dkc-<8 hex>");
        assert_eq!(
            spec.volume,
            "dkc-claude-aabbccdd-1111-4222-8333-444444444444",
            "volume is dkc-claude-<uuid>"
        );
        let body = spec.body;
        let labels = body.labels.unwrap();
        assert_eq!(labels.len(), 2, "exactly two labels");
        assert_eq!(labels["managed-by"], "mate", "ownership label");
        assert_eq!(labels["session-id"], uid.to_string(), "session label");
        assert_eq!(
            body.env.unwrap(),
            vec!["CLAUDE_CONFIG_DIR=/home/dev/.claude".to_owned()],
            "CLAUDE_CONFIG_DIR is the only env var, and no credential-like key exists"
        );
        assert_eq!(body.cmd.unwrap(), ["sleep", "infinity"], "keep-alive command");
        assert_eq!(body.tty, Some(true), "tty on");
        assert_eq!(body.open_stdin, Some(true), "stdin open");
        assert_eq!(body.working_dir.as_deref(), Some("/workspace"), "cwd is the workspace");
        assert_eq!(body.user.as_deref(), Some("dev"), "runs as the image user");
        let binds = body.host_config.unwrap().binds.unwrap();
        assert!(
            binds.contains(&format!("{}:/home/dev/.claude:rw", spec.volume)),
            "the state volume is mounted at CLAUDE_CONFIG_DIR"
        );
    }
}
