//! [`ToolCtx`] (§6.1) and the path jail every filesystem-touching tool resolves user
//! input through (§8.1, `M3-2`). Rig executes tool calls automatically — there is no
//! interception point between model and `call()` — so the tool itself is the security
//! boundary, and `resolve` is where that boundary lives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{ActivitySink, AgentId, Approvals, SubagentSpawner, ToolFailure};

/// Everything a tool needs at construction time, captured by value into the tool struct
/// (§8.1 note 2: `call(&self, args)` takes no context parameter, so root, caps, spawner,
/// and cancellation must already live on `self`).
///
/// `session: SessionId` isn't here yet — `SessionId` has no producer until `M6`'s session
/// manager, the same reasoning `mate-core::streaming::AgentEventEnvelope` already applied.
#[derive(Clone)]
pub struct ToolCtx {
    /// Which agent is calling — for activity/event attribution.
    pub agent: AgentId,
    /// Canonicalized workspace root every path is jailed under.
    pub root: PathBuf,
    pub max_output_bytes: usize,
    pub spawner: Option<Arc<dyn SubagentSpawner>>,
    pub activity: ActivitySink,
    /// This agent's cancellation token, child of the session's.
    pub cancel: CancellationToken,
    /// The session's one approval channel (§7.4, `M13-1`) — `None` in every frontend that
    /// doesn't wire one up yet (`mate-cli`'s plain frontend, every test helper below).
    /// `mate_core::session::SessionManager::spawn` overwrites this the same way it already
    /// overwrites `cancel` and `activity`.
    pub approvals: Option<Arc<dyn Approvals>>,
}

impl ToolCtx {
    /// Resolves a model-supplied path to a real, in-jail file: join under `root`,
    /// canonicalize (so both `../` traversal and an outward-pointing symlink resolve to
    /// their real target before the boundary check), require the result to still start
    /// with `root`, and reject denylisted names. Canonicalizing *after* joining is the
    /// whole trick — canonicalizing `user` alone would never see the traversal.
    pub fn resolve(&self, user_path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.root.join(user_path);
        let canon = dunce::canonicalize(&candidate)
            .map_err(|_| ToolFailure::NotFound(user_path.to_string()))?;

        if !canon.starts_with(&self.root) || is_denylisted(&canon) {
            return Err(ToolFailure::Denied(user_path.to_string()));
        }

        Ok(canon)
    }
}

/// Fixed policy denylist (§8.1): secrets and VCS internals are never readable, in-jail or
/// not. `.git` is checked as a path component so anything *inside* the directory is
/// covered, not just a literal `.git` file.
fn is_denylisted(path: &Path) -> bool {
    if path.components().any(|c| c.as_os_str() == ".git") {
        return true;
    }

    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    name == ".env" || name.ends_with(".pem") || name.starts_with("id_rsa")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ctx(root: PathBuf) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
        }
    }

    #[test]
    fn resolves_a_valid_in_jail_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        fs::write(root.join("a.txt"), "hi").unwrap();

        let resolved = ctx(root.clone()).resolve("a.txt").unwrap();
        assert_eq!(resolved, root.join("a.txt"));
    }

    #[test]
    fn rejects_dot_dot_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        let outside = tmp_root.join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "s3cr3t").unwrap();

        let err = ctx(workspace).resolve("../outside/secret.txt").unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn rejects_an_absolute_path_escaping_root() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        let outside = tmp_root.join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret.txt");
        fs::write(&secret, "s3cr3t").unwrap();

        let err = ctx(workspace)
            .resolve(secret.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_pointing_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        let outside = tmp_root.join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "s3cr3t").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), workspace.join("link")).unwrap();

        let err = ctx(workspace).resolve("link").unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn rejects_denylisted_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::write(root.join("id_rsa"), "key").unwrap();
        fs::write(root.join("service.pem"), "cert").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("config"), "[core]").unwrap();

        let c = ctx(root);
        for path in [".env", "id_rsa", "service.pem", ".git/config"] {
            let err = c.resolve(path).unwrap_err();
            assert!(
                matches!(err, ToolFailure::Denied(_)),
                "{path} should be denied"
            );
        }
    }

    #[test]
    fn rejects_a_non_existent_path_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let err = ctx(root).resolve("does/not/exist.txt").unwrap_err();
        assert!(matches!(err, ToolFailure::NotFound(_)));
    }
}
