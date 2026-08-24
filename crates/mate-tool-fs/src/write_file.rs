//! `write_file`: create or overwrite a file inside the workspace with full content, jailed
//! through [`ToolCtx::resolve_for_write`]. The first `mate-tool-fs` tool that mutates the
//! filesystem, so every call is gated through [`ToolCtx::approvals`] (§7.4) before anything
//! touches disk — there is no unattended write path.

use mate_tool_api::{ApprovalRequest, FileOp, ToolActivity, ToolCtx, ToolFailure, enforce_max_size};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteFileArgs {
    /// Path to the file, relative to the workspace root. The containing directory must
    /// already exist; the file itself is created if missing, overwritten in full if not.
    pub path: String,
    /// Full contents to write. Replaces whatever the file previously held — this is a
    /// whole-file write, not a patch or append.
    pub content: String,
}

/// Writes a file inside the workspace, jailed through [`ToolCtx::resolve_for_write`]. Always
/// asks [`ToolCtx::approvals`] before writing; refuses outright if no approval channel is
/// wired up (§7.4: a write with nobody able to answer is denied, not silently allowed).
pub struct WriteFile {
    ctx: ToolCtx,
}

impl WriteFile {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for WriteFile {
    const NAME: &'static str = "write_file";
    type Args = WriteFileArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Create or overwrite a file inside the workspace with the given full contents. The \
         containing directory must already exist. Every write requires human approval before \
         it happens."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(WriteFileArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: WriteFileArgs) -> Result<String, ToolFailure> {
        enforce_max_size(args.content.len() as u64, self.ctx.max_output_bytes)?;
        let path = self.ctx.resolve_for_write(&args.path)?;

        let existed = match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_dir() => {
                return Err(ToolFailure::InvalidArgs(format!(
                    "path is a directory: {}",
                    args.path
                )));
            }
            Ok(_) => true,
            Err(_) => false,
        };

        let approvals = self.ctx.approvals.as_ref().ok_or_else(|| {
            ToolFailure::Denied(
                "no approval channel available in this frontend; writes require interactive \
                 confirmation"
                    .to_string(),
            )
        })?;
        let granted = approvals
            .request(ApprovalRequest {
                agent: self.ctx.agent,
                name: Self::NAME.to_string(),
                detail: format!(
                    "{} {}",
                    if existed { "overwrite" } else { "create" },
                    args.path
                ),
                path: Some(path.clone()),
            })
            .await;
        if !granted {
            return Err(ToolFailure::Denied(format!(
                "write to {} was not approved",
                args.path
            )));
        }

        tokio::fs::write(&path, args.content.as_bytes())
            .await
            .map_err(|error| ToolFailure::Other(anyhow::anyhow!(error)))?;

        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::FileTouched {
                path: path.clone(),
                op: if existed { FileOp::Write } else { FileOp::Create },
                lines: args.content.lines().count(),
                bytes: args.content.len(),
            },
        ));

        Ok(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            args.path
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct StubApprovals(AtomicBool);

    #[async_trait]
    impl mate_tool_api::Approvals for StubApprovals {
        async fn request(&self, _request: ApprovalRequest) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn ctx(
        root: std::path::PathBuf,
        approvals: Option<Arc<dyn mate_tool_api::Approvals>>,
    ) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals,
            skills: std::sync::Arc::from([]),
            agents_md: None,
        }
    }

    fn ctx_with_activity(
        root: std::path::PathBuf,
        approvals: Option<Arc<dyn mate_tool_api::Approvals>>,
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
                approvals,
                skills: std::sync::Arc::from([]),
                agents_md: None,
            },
            rx,
        )
    }

    fn granting() -> Arc<dyn mate_tool_api::Approvals> {
        Arc::new(StubApprovals(AtomicBool::new(true)))
    }

    fn denying() -> Arc<dyn mate_tool_api::Approvals> {
        Arc::new(StubApprovals(AtomicBool::new(false)))
    }

    #[tokio::test]
    async fn creates_a_new_file_once_approved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let tool = WriteFile::new(ctx(root.clone(), Some(granting())));
        tool.call(WriteFileArgs {
            path: "new.txt".to_string(),
            content: "hello".to_string(),
        })
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(root.join("new.txt")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn overwrites_an_existing_file_once_approved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "old").unwrap();

        let tool = WriteFile::new(ctx(root.clone(), Some(granting())));
        tool.call(WriteFileArgs {
            path: "a.txt".to_string(),
            content: "new".to_string(),
        })
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "new");
    }

    #[tokio::test]
    async fn a_denied_approval_leaves_the_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let tool = WriteFile::new(ctx(root.clone(), Some(denying())));
        let err = tool
            .call(WriteFileArgs {
                path: "new.txt".to_string(),
                content: "hello".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::Denied(_)));
        assert!(!root.join("new.txt").exists());
    }

    #[tokio::test]
    async fn refuses_to_write_with_no_approval_channel_wired_up() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let tool = WriteFile::new(ctx(root.clone(), None));
        let err = tool
            .call(WriteFileArgs {
                path: "new.txt".to_string(),
                content: "hello".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::Denied(_)));
        assert!(!root.join("new.txt").exists());
    }

    #[tokio::test]
    async fn refuses_a_path_whose_parent_directory_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let tool = WriteFile::new(ctx(root, Some(granting())));
        let err = tool
            .call(WriteFileArgs {
                path: "missing/new.txt".to_string(),
                content: "hello".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::NotFound(_)));
    }

    #[tokio::test]
    async fn refuses_a_directory_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let tool = WriteFile::new(ctx(root, Some(granting())));
        let err = tool
            .call(WriteFileArgs {
                path: "sub".to_string(),
                content: "hello".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn refuses_content_over_the_output_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();

        let mut c = ctx(root, Some(granting()));
        c.max_output_bytes = 4;
        let tool = WriteFile::new(c);
        let err = tool
            .call(WriteFileArgs {
                path: "new.txt".to_string(),
                content: "way too long".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::TooLarge { limit: 4 }));
    }

    #[tokio::test]
    async fn emits_a_create_record_for_a_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let (ctx, mut rx) = ctx_with_activity(root.clone(), Some(granting()));

        let tool = WriteFile::new(ctx);
        tool.call(WriteFileArgs {
            path: "new.txt".to_string(),
            content: "one\ntwo".to_string(),
        })
        .await
        .unwrap();

        let (agent, activity) = rx.try_recv().expect("a FileTouched record must be emitted");
        assert_eq!(agent, mate_tool_api::AgentId::ROOT);
        assert_eq!(
            activity,
            ToolActivity::FileTouched {
                path: root.join("new.txt"),
                op: FileOp::Create,
                lines: 2,
                bytes: 7,
            },
            "a not-yet-existing target must report FileOp::Create"
        );
    }

    #[tokio::test]
    async fn emits_a_write_record_for_an_overwritten_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "old").unwrap();
        let (ctx, mut rx) = ctx_with_activity(root.clone(), Some(granting()));

        let tool = WriteFile::new(ctx);
        tool.call(WriteFileArgs {
            path: "a.txt".to_string(),
            content: "new".to_string(),
        })
        .await
        .unwrap();

        let (_, activity) = rx.try_recv().expect("a FileTouched record must be emitted");
        match activity {
            ToolActivity::FileTouched { op, .. } => {
                assert_eq!(
                    op,
                    FileOp::Write,
                    "an already-existing target must report FileOp::Write"
                )
            }
            other => panic!("expected FileTouched, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_a_path_escaping_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = dunce::canonicalize(tmp.path()).unwrap();
        let workspace = tmp_root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(tmp_root.join("outside")).unwrap();

        let tool = WriteFile::new(ctx(workspace, Some(granting())));
        let err = tool
            .call(WriteFileArgs {
                path: "../outside/new.txt".to_string(),
                content: "hello".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::Denied(_)));
    }
}
