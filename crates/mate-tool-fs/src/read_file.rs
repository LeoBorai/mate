//! `read_file` (`M3-3`): line-numbered file contents, optionally sliced to a line range.

use mate_tool_api::{
    FileOp, ToolActivity, ToolCtx, ToolFailure, enforce_max_size, number_lines, refuse_binary,
};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadFileArgs {
    /// Path to the file, relative to the workspace root.
    pub path: String,
    /// First line to include, 1-based and inclusive. Clamped into range; omit to start at
    /// the beginning of the file.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// Last line to include, 1-based and inclusive. Clamped into range; omit to read to the
    /// end of the file.
    #[serde(default)]
    pub end_line: Option<usize>,
}

/// Reads a file inside the workspace, jailed through [`ToolCtx::resolve`]. Refuses files
/// over `max_output_bytes` or that look binary (`M3-6`) rather than truncating them —
/// there's no line-numbered rendering of half a PNG. Output is line-numbered so a model can
/// cite exact locations back.
pub struct ReadFile {
    ctx: ToolCtx,
}

impl ReadFile {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for ReadFile {
    const NAME: &'static str = "read_file";
    type Args = ReadFileArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Read a file inside the workspace. Output is line-numbered. Use start_line/end_line \
         to read a slice of a large file instead of the whole thing."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(ReadFileArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: ReadFileArgs) -> Result<String, ToolFailure> {
        let path = self.ctx.resolve(&args.path)?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| ToolFailure::NotFound(args.path.clone()))?;
        enforce_max_size(metadata.len(), self.ctx.max_output_bytes)?;

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|_| ToolFailure::NotFound(args.path.clone()))?;
        refuse_binary(&bytes)?;

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Ok(String::new());
        }

        let start = args.start_line.unwrap_or(1).clamp(1, total);
        let end = args.end_line.unwrap_or(total).clamp(start, total);

        // Whole-file line/byte counts, not the (possibly sliced) range actually returned —
        // the documents log (§9.8) shows what the file *is*, so repeat reads of different
        // slices still coalesce into one row instead of each slice looking like a different
        // file size.
        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::FileTouched {
                path: path.clone(),
                op: FileOp::Read,
                lines: total,
                bytes: metadata.len() as usize,
            },
        ));

        Ok(number_lines(&lines[(start - 1)..end].join("\n"), start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx(root: std::path::PathBuf) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
            skills: std::sync::Arc::from([]),
            agents_md: None,
        }
    }

    fn ctx_with_activity(
        root: std::path::PathBuf,
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
                skills: std::sync::Arc::from([]),
                agents_md: None,
            },
            rx,
        )
    }

    fn args(path: &str, start_line: Option<usize>, end_line: Option<usize>) -> ReadFileArgs {
        ReadFileArgs {
            path: path.to_string(),
            start_line,
            end_line,
        }
    }

    #[tokio::test]
    async fn reads_a_whole_file_with_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree").unwrap();

        let tool = ReadFile::new(ctx(root));
        let out = tool.call(args("a.txt", None, None)).await.unwrap();
        assert_eq!(out, "     1\tone\n     2\ttwo\n     3\tthree");
    }

    #[tokio::test]
    async fn emits_a_file_touched_record_with_the_whole_files_lines_and_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree").unwrap();
        let (ctx, mut rx) = ctx_with_activity(root.clone());

        let tool = ReadFile::new(ctx);
        // Slicing to one line must not shrink the reported lines/bytes — they describe the
        // whole file, not the slice actually returned (see the emission site's comment).
        tool.call(args("a.txt", Some(1), Some(1))).await.unwrap();

        let (agent, activity) = rx.try_recv().expect("a FileTouched record must be emitted");
        assert_eq!(agent, mate_tool_api::AgentId::ROOT);
        assert_eq!(
            activity,
            ToolActivity::FileTouched {
                path: root.join("a.txt"),
                op: FileOp::Read,
                lines: 3,
                bytes: 13,
            },
            "lines/bytes must reflect the whole file, not the requested slice"
        );
    }

    #[tokio::test]
    async fn clamps_start_line_below_one_up_to_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree").unwrap();

        let tool = ReadFile::new(ctx(root));
        // `start_line: Some(0)` is out of range on the low end; clamp, don't error.
        let out = tool.call(args("a.txt", Some(0), Some(1))).await.unwrap();
        assert_eq!(out, "     1\tone");
    }

    #[tokio::test]
    async fn clamps_end_line_past_the_file_end_down_to_the_last_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree").unwrap();

        let tool = ReadFile::new(ctx(root));
        let out = tool.call(args("a.txt", Some(2), Some(999))).await.unwrap();
        assert_eq!(out, "     2\ttwo\n     3\tthree");
    }

    #[tokio::test]
    async fn refuses_a_file_over_the_output_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("big.txt"), vec![b'a'; 10 * 1024 * 1024]).unwrap();

        let mut c = ctx(root);
        c.max_output_bytes = 1024;
        let tool = ReadFile::new(c);
        let err = tool.call(args("big.txt", None, None)).await.unwrap_err();
        assert!(matches!(err, ToolFailure::TooLarge { limit: 1024 }));
    }

    #[tokio::test]
    async fn refuses_a_binary_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("image.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();

        let tool = ReadFile::new(ctx(root));
        let err = tool.call(args("image.png", None, None)).await.unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn schema_carries_field_descriptions() {
        let tool = ReadFile::new(ctx(std::env::temp_dir()));
        let schema = tool.parameters();
        let description = schema["properties"]["path"]["description"]
            .as_str()
            .unwrap();
        assert!(description.contains("workspace root"));
    }
}
