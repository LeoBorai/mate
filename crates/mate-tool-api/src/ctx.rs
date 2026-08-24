//! [`ToolCtx`] (§6.1) and the path jail every filesystem-touching tool resolves user
//! input through (§8.1, `M3-2`). Rig executes tool calls automatically — there is no
//! interception point between model and `call()` — so the tool itself is the security
//! boundary, and `resolve` is where that boundary lives.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{
    ActivitySink, AgentId, AgentsMdSource, Approvals, SkillMetadata, SubagentSpawner, ToolFailure,
};

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
    /// Skills discovered under `.claude/skills`/`.opencode/skills`/`.copilot/skills`/
    /// `.agents/skills` at session-build time — never re-walked per call. Empty unless the
    /// workspace root actually has one of those directories. `mate_core::toolset::build_toolset`
    /// attaches the `skill` tool only when this is non-empty, the same conditional-attachment
    /// pattern `spawner`/http already use.
    pub skills: Arc<[SkillMetadata]>,
    /// The workspace root's project-instructions file (`AGENTS.md`, `CLAUDE.md`, ...),
    /// discovered once at session-build time by `mate_core::agents_md::discover_agents_md` —
    /// never re-read per call. `None` when no recognized filename is present, or the config
    /// disables the feature. No tool reads this at call time; it rides `ToolCtx` only because
    /// `mate_core::session::SessionManager::spawn` already threads `ctx.skills` the same way to
    /// build each session's `SubagentRunner`, and this reuses that exact channel rather than
    /// adding a parallel one.
    pub agents_md: Option<Arc<AgentsMdSource>>,
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

    /// Resolves a model-supplied path for writing: like [`Self::resolve`], but the target
    /// itself need not exist yet — only its containing directory does, since [`Self::resolve`]
    /// canonicalizing the full candidate would reject every not-yet-created file as not
    /// found. Jails the same way: canonicalize the containing directory (so `../` traversal or
    /// a symlinked ancestor resolves to its real location before the boundary check), require
    /// it under `root`, then build the final path from there. If *anything* already sits at
    /// that name — including a symlink whose own destination doesn't exist yet, checked via
    /// [`std::fs::symlink_metadata`] rather than [`std::fs::metadata`] specifically so a
    /// dangling symlink still counts as "something's there" — canonicalize *that* too and
    /// re-check the jail: writing through a symlink follows it, so a symlink that can't be
    /// verified safe (denylisted, outside root, or dangling) is denied rather than silently
    /// treated as a fresh file at the symlink's own path.
    pub fn resolve_for_write(&self, user_path: &str) -> Result<PathBuf, ToolFailure> {
        let candidate = self.root.join(user_path);
        let file_name = candidate
            .file_name()
            .ok_or_else(|| ToolFailure::InvalidArgs(format!("not a file path: {user_path}")))?
            .to_owned();
        let parent = candidate
            .parent()
            .ok_or_else(|| ToolFailure::InvalidArgs(format!("not a file path: {user_path}")))?;

        let canon_parent = dunce::canonicalize(parent)
            .map_err(|_| ToolFailure::NotFound(user_path.to_string()))?;
        if !canon_parent.starts_with(&self.root) || is_denylisted(&canon_parent) {
            return Err(ToolFailure::Denied(user_path.to_string()));
        }

        let target = canon_parent.join(&file_name);
        let final_path = match std::fs::symlink_metadata(&target) {
            Ok(_) => {
                let canon_target = dunce::canonicalize(&target)
                    .map_err(|_| ToolFailure::Denied(user_path.to_string()))?;
                if !canon_target.starts_with(&self.root) {
                    return Err(ToolFailure::Denied(user_path.to_string()));
                }
                canon_target
            }
            Err(_) => target,
        };

        if is_denylisted(&final_path) {
            return Err(ToolFailure::Denied(user_path.to_string()));
        }

        Ok(final_path)
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
            skills: Arc::from([]),
            agents_md: None,
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

    #[test]
    fn resolve_for_write_allows_a_not_yet_existing_file_in_an_existing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let resolved = ctx(root.clone()).resolve_for_write("new.txt").unwrap();
        assert_eq!(resolved, root.join("new.txt"));
    }

    #[test]
    fn resolve_for_write_allows_overwriting_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        fs::write(root.join("a.txt"), "old").unwrap();

        let resolved = ctx(root.clone()).resolve_for_write("a.txt").unwrap();
        assert_eq!(resolved, root.join("a.txt"));
    }

    #[test]
    fn resolve_for_write_rejects_a_missing_parent_directory_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let err = ctx(root)
            .resolve_for_write("does/not/exist.txt")
            .unwrap_err();
        assert!(matches!(err, ToolFailure::NotFound(_)));
    }

    #[test]
    fn resolve_for_write_rejects_dot_dot_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(tmp_root.join("outside")).unwrap();

        let err = ctx(workspace)
            .resolve_for_write("../outside/new.txt")
            .unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn resolve_for_write_rejects_denylisted_names_even_when_not_yet_created() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let c = ctx(root);
        for path in [".env", "id_rsa", "new.pem"] {
            let err = c.resolve_for_write(path).unwrap_err();
            assert!(
                matches!(err, ToolFailure::Denied(_)),
                "{path} should be denied even though it doesn't exist yet"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_rejects_a_symlink_pointing_at_an_existing_outside_file() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        let outside = tmp_root.join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "s3cr3t").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), workspace.join("link")).unwrap();

        let err = ctx(workspace).resolve_for_write("link").unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_rejects_a_dangling_symlink_pointing_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        let outside = tmp_root.join("outside");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&outside).unwrap();
        // The symlink's destination (`outside/target.txt`) is never created — writing through
        // a dangling symlink must still be denied, not silently treated as a fresh file at the
        // symlink's own in-jail path (`std::fs::canonicalize` alone can't tell the two apart,
        // which is exactly why `resolve_for_write` checks `symlink_metadata` first).
        std::os::unix::fs::symlink(outside.join("target.txt"), workspace.join("link")).unwrap();

        let err = ctx(workspace).resolve_for_write("link").unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }
}
