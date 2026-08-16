//! `find_files` (`M3-5`): glob search within the workspace root, capped — stops the model
//! guessing paths one `read_file` at a time.

use std::path::{Component, Path};

use mate_tool_api::{FileOp, ToolActivity, ToolCtx, ToolFailure};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

/// Matches beyond this count are dropped with a truncation notice — a pattern like `**/*`
/// on a large tree shouldn't hand the model thousands of paths.
const RESULT_CAP: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindFilesArgs {
    /// Glob pattern to match against files under the workspace root, e.g. "**/*.rs" or
    /// "src/**/*.toml". Must stay inside the root: no ".." segments, no absolute paths.
    pub pattern: String,
}

/// Finds files under the workspace root matching a glob, jailed by construction (every
/// match comes from walking [`ToolCtx::root`] itself, never from an escaped pattern).
pub struct FindFiles {
    ctx: ToolCtx,
}

impl FindFiles {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for FindFiles {
    const NAME: &'static str = "find_files";
    type Args = FindFilesArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Find files under the workspace root matching a glob pattern, e.g. \"**/*.rs\". \
         Respects .gitignore."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(FindFilesArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: FindFilesArgs) -> Result<String, ToolFailure> {
        reject_escaping_pattern(&args.pattern)?;

        let root = self.ctx.root.clone();
        let pattern = args.pattern.clone();
        let (matches, truncated) =
            tokio::task::spawn_blocking(move || find_matches(&root, &pattern))
                .await
                .map_err(|error| ToolFailure::Other(error.into()))??;

        let match_count = matches.len();
        let mut body = matches.join("\n");
        if truncated {
            body.push_str(&format!(
                "\n... [truncated: showing first {RESULT_CAP} matches]"
            ));
        }

        // `find_files` has no single touched file — `path` carries the glob pattern itself
        // (not a real, resolvable path) so the documents log (§9.8) still gets one row per
        // call, `lines` the match count, `bytes` the rendered listing size.
        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::FileTouched {
                path: std::path::PathBuf::from(&args.pattern),
                op: FileOp::Read,
                lines: match_count,
                bytes: body.len(),
            },
        ));

        Ok(body)
    }
}

/// Rejects a pattern that could walk outside the root — `..` segments or an absolute
/// path — rather than trying to clamp it into something safe.
fn reject_escaping_pattern(pattern: &str) -> Result<(), ToolFailure> {
    let path = Path::new(pattern);
    if path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(ToolFailure::Denied(pattern.to_string()));
    }
    Ok(())
}

/// Blocking glob walk (`ignore::WalkBuilder` has no async API): every file under `root`
/// matching `pattern`, `.gitignore`-aware regardless of whether `root` is a git repository.
fn find_matches(root: &Path, pattern: &str) -> Result<(Vec<String>, bool), ToolFailure> {
    let overrides = ignore::overrides::OverrideBuilder::new(root)
        .add(pattern)
        .and_then(|builder| builder.build())
        .map_err(|error| ToolFailure::InvalidArgs(format!("invalid glob pattern: {error}")))?;

    let walker = ignore::WalkBuilder::new(root)
        .require_git(false)
        .overrides(overrides)
        .build();

    let mut matches: Vec<String> = Vec::new();
    let mut truncated = false;

    for result in walker {
        let entry = result.map_err(|error| ToolFailure::Other(anyhow::anyhow!(error)))?;
        if entry.file_type().is_none_or(|t| t.is_dir()) {
            continue;
        }
        if matches.len() == RESULT_CAP {
            truncated = true;
            break;
        }
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        matches.push(relative.to_string_lossy().into_owned());
    }

    matches.sort();
    Ok((matches, truncated))
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
            },
            rx,
        )
    }

    #[tokio::test]
    async fn emits_a_file_touched_record_naming_the_pattern_and_match_count() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.rs"), "").unwrap();
        std::fs::write(root.join("b.rs"), "").unwrap();
        let (ctx, mut rx) = ctx_with_activity(root.clone());

        let tool = FindFiles::new(ctx);
        tool.call(FindFilesArgs {
            pattern: "*.rs".to_string(),
        })
        .await
        .unwrap();

        let (agent, activity) = rx.try_recv().expect("a FileTouched record must be emitted");
        assert_eq!(agent, mate_tool_api::AgentId::ROOT);
        match activity {
            ToolActivity::FileTouched {
                path,
                op,
                lines,
                bytes,
            } => {
                assert_eq!(
                    path,
                    PathBuf::from("*.rs"),
                    "path must carry the glob pattern"
                );
                assert_eq!(op, FileOp::Read);
                assert_eq!(lines, 2, "lines must count the matches");
                assert_eq!(bytes, "a.rs\nb.rs".len());
            }
            other => panic!("expected FileTouched, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finds_files_matching_a_recursive_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "").unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();

        let tool = FindFiles::new(ctx(root));
        let out = tool
            .call(FindFilesArgs {
                pattern: "**/*.rs".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(out, "src/lib.rs\nsrc/main.rs");
    }

    #[tokio::test]
    async fn rejects_a_pattern_escaping_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let tool = FindFiles::new(ctx(root));
        let err = tool
            .call(FindFilesArgs {
                pattern: "../*.rs".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn result_cap_emits_a_truncation_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        for i in 0..RESULT_CAP + 5 {
            std::fs::write(root.join(format!("f{i}.rs")), "").unwrap();
        }

        let tool = FindFiles::new(ctx(root));
        let out = tool
            .call(FindFilesArgs {
                pattern: "*.rs".to_string(),
            })
            .await
            .unwrap();
        assert!(out.contains("truncated"));
    }
}
