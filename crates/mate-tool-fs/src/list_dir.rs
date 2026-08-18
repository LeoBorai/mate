//! `list_dir` (`M3-4`): one level of a directory, `.gitignore`-aware, capped.

use std::path::Path;

use mate_tool_api::{FileOp, ToolActivity, ToolCtx, ToolFailure};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

/// Entries beyond this count are dropped with a truncation notice rather than dumped in
/// full — stops a model reading a 50k-file `node_modules` into its own context.
const ENTRY_CAP: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDirArgs {
    /// Directory to list, relative to the workspace root. Omit to list the root itself.
    #[serde(default)]
    pub path: Option<String>,
}

/// Lists one level of a directory inside the workspace, jailed through
/// [`ToolCtx::resolve`]. Skips whatever `.gitignore` would skip (hidden files included) so
/// the model isn't handed build output and dependency trees it has to filter itself.
pub struct ListDir {
    ctx: ToolCtx,
}

impl ListDir {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for ListDir {
    const NAME: &'static str = "list_dir";
    type Args = ListDirArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "List one level of a directory inside the workspace. Respects .gitignore. \
         Directory entries are suffixed with '/'."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(ListDirArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: ListDirArgs) -> Result<String, ToolFailure> {
        let user_path = args.path.as_deref().unwrap_or("");
        let dir = self.ctx.resolve(user_path)?;

        let metadata = tokio::fs::metadata(&dir)
            .await
            .map_err(|_| ToolFailure::NotFound(user_path.to_string()))?;
        if !metadata.is_dir() {
            return Err(ToolFailure::InvalidArgs(format!(
                "not a directory: {user_path}"
            )));
        }

        let walk_dir = dir.clone();
        let (entries, truncated) = tokio::task::spawn_blocking(move || list_one_level(&walk_dir))
            .await
            .map_err(|error| ToolFailure::Other(error.into()))??;

        let entry_count = entries.len();
        let mut body = entries.join("\n");
        if truncated {
            body.push_str(&format!(
                "\n... [truncated: showing first {ENTRY_CAP} entries]"
            ));
        }

        // `list_dir` touches a directory, not a single file — `path` names the directory
        // listed, `lines` the entry count, `bytes` the rendered listing size, so the
        // documents log (§9.8) still gets one coherent row per call.
        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::FileTouched {
                path: dir,
                op: FileOp::Read,
                lines: entry_count,
                bytes: body.len(),
            },
        ));

        Ok(body)
    }
}

/// Blocking directory walk (`ignore::WalkBuilder` has no async API): one level deep,
/// `.gitignore`-aware regardless of whether `dir` sits inside an actual git repository.
fn list_one_level(dir: &Path) -> Result<(Vec<String>, bool), ToolFailure> {
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .require_git(false)
        .build();

    for result in walker {
        let entry = result.map_err(|error| ToolFailure::Other(anyhow::anyhow!(error)))?;
        if entry.depth() == 0 {
            continue; // the directory itself
        }
        if names.len() == ENTRY_CAP {
            truncated = true;
            break;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        names.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }

    names.sort();
    let rendered = names
        .into_iter()
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name })
        .collect();
    Ok((rendered, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    fn ctx(root: PathBuf) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
        }
    }

    fn ctx_with_activity(
        root: PathBuf,
    ) -> (
        ToolCtx,
        tokio::sync::mpsc::Receiver<(mate_tool_api::AgentId, ToolActivity)>,
    ) {
        let (activity, rx) = tokio::sync::mpsc::channel(8);
        (
            ToolCtx {
                agent: mate_tool_api::AgentId::ROOT,
                root,
                max_output_bytes: 1_000_000,
                spawner: None,
                activity,
                cancel: CancellationToken::new(),
                approvals: None,
            },
            rx,
        )
    }

    #[tokio::test]
    async fn emits_a_file_touched_record_naming_the_directory_and_entry_count() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let (ctx, mut rx) = ctx_with_activity(root.clone());

        let tool = ListDir::new(ctx);
        tool.call(ListDirArgs { path: None }).await.unwrap();

        let (agent, activity) = rx.try_recv().expect("a FileTouched record must be emitted");
        assert_eq!(agent, mate_tool_api::AgentId::ROOT);
        match activity {
            ToolActivity::FileTouched {
                path,
                op,
                lines,
                bytes,
            } => {
                assert_eq!(path, root, "path must name the directory listed");
                assert_eq!(op, FileOp::Read);
                assert_eq!(lines, 2, "lines must count the listed entries");
                assert_eq!(bytes, "a.txt\nb.txt".len());
            }
            other => panic!("expected FileTouched, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lists_files_and_directories_one_level_deep() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("nested.txt"), "").unwrap();

        let tool = ListDir::new(ctx(root));
        let out = tool.call(ListDirArgs { path: None }).await.unwrap();
        assert_eq!(out, "a.txt\nsub/");
    }

    #[tokio::test]
    async fn ignored_files_are_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "").unwrap();
        std::fs::write(root.join("kept.txt"), "").unwrap();

        let tool = ListDir::new(ctx(root));
        let out = tool.call(ListDirArgs { path: None }).await.unwrap();
        assert!(!out.contains("ignored.txt"));
        assert!(out.contains("kept.txt"));
    }

    #[tokio::test]
    async fn entry_cap_emits_a_truncation_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        for i in 0..ENTRY_CAP + 5 {
            std::fs::write(root.join(format!("f{i}.txt")), "").unwrap();
        }

        let tool = ListDir::new(ctx(root));
        let out = tool.call(ListDirArgs { path: None }).await.unwrap();
        assert!(out.contains("truncated"));
    }
}
