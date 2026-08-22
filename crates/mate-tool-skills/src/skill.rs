//! `skill` — the level-2 loader: given a name from the "Available skills" preamble section,
//! returns that skill's full `SKILL.md` body (frontmatter stripped, since the model already
//! has name/description from the preamble) plus the skill's own directory, so bundled files
//! can be read with `read_file`/`find_files` (level 3 — `mate` has no bash tool to `cat` them
//! itself).

use mate_tool_api::{ToolActivity, ToolCtx, ToolFailure, enforce_max_size, refuse_binary};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::frontmatter;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkillArgs {
    /// Exact `name` of a skill from the "Available skills" list in the system preamble.
    pub name: String,
}

/// Loads one skill's full instructions by name. Only ever attached when
/// `ToolCtx::skills` is non-empty (`mate_core::toolset::build_toolset`).
pub struct Skill {
    ctx: ToolCtx,
}

impl Skill {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for Skill {
    const NAME: &'static str = "skill";
    type Args = SkillArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Load the full instructions for a skill named in the \"Available skills\" list. \
         Returns the skill's own directory (read any file it bundles with read_file/find_files) \
         followed by its complete instructions."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(SkillArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: SkillArgs) -> Result<String, ToolFailure> {
        let metadata = self
            .ctx
            .skills
            .iter()
            .find(|skill| skill.name == args.name)
            .ok_or_else(|| ToolFailure::NotFound(args.name.clone()))?;

        let skill_md = self.ctx.root.join(&metadata.dir).join("SKILL.md");
        let file_metadata = tokio::fs::metadata(&skill_md)
            .await
            .map_err(|_| ToolFailure::NotFound(args.name.clone()))?;
        enforce_max_size(file_metadata.len(), self.ctx.max_output_bytes)?;

        let bytes = tokio::fs::read(&skill_md)
            .await
            .map_err(|_| ToolFailure::NotFound(args.name.clone()))?;
        refuse_binary(&bytes)?;

        let content = String::from_utf8_lossy(&bytes);
        let parsed = frontmatter::parse(&content)
            .map_err(|error| ToolFailure::Other(anyhow::anyhow!(error)))?;

        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::Note {
                text: format!("skill `{}` loaded", metadata.name),
            },
        ));

        Ok(format!(
            "Skill directory: {} (read bundled files from here with read_file/find_files)\n\n{}",
            metadata.dir.display(),
            parsed.body
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Arc;

    use mate_tool_api::{AgentId, SkillMetadata};
    use tokio_util::sync::CancellationToken;

    fn ctx(root: PathBuf, skills: Vec<SkillMetadata>) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
            skills: Arc::from(skills),
        }
    }

    fn ctx_with_activity(
        root: PathBuf,
        skills: Vec<SkillMetadata>,
    ) -> (
        ToolCtx,
        tokio::sync::mpsc::Receiver<(AgentId, ToolActivity)>,
    ) {
        let (activity, rx) = tokio::sync::mpsc::channel(8);
        (
            ToolCtx {
                agent: AgentId::ROOT,
                root,
                max_output_bytes: 1_000_000,
                spawner: None,
                activity,
                cancel: CancellationToken::new(),
                approvals: None,
                skills: Arc::from(skills),
            },
            rx,
        )
    }

    fn write_skill(root: &std::path::Path, dir: &str, content: &str) -> SkillMetadata {
        let skill_dir = root.join(dir);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        let parsed = frontmatter::parse(content).unwrap();
        SkillMetadata {
            name: parsed.name,
            description: parsed.description,
            dir: PathBuf::from(dir),
        }
    }

    #[tokio::test]
    async fn loads_the_body_with_frontmatter_stripped_and_a_directory_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let metadata = write_skill(
            &root,
            ".claude/skills/pdf-processing",
            "---\nname: pdf-processing\ndescription: Extract PDFs.\n---\n\n# PDF Processing\n\nDo the thing.\n",
        );

        let tool = Skill::new(ctx(root, vec![metadata]));
        let out = tool.call(SkillArgs { name: "pdf-processing".to_string() }).await.unwrap();

        assert!(
            out.starts_with(
                "Skill directory: .claude/skills/pdf-processing (read bundled files from here with read_file/find_files)\n\n"
            ),
            "output must lead with the skill's own directory: {out}"
        );
        assert!(out.ends_with("# PDF Processing\n\nDo the thing."));
        assert!(
            !out.contains("---\nname:"),
            "frontmatter must be stripped, the model already has name/description from the preamble"
        );
    }

    #[tokio::test]
    async fn an_unknown_name_is_not_found() {
        let tool = Skill::new(ctx(PathBuf::from("."), Vec::new()));
        let err = tool
            .call(SkillArgs {
                name: "does-not-exist".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ToolFailure::NotFound(_)));
    }

    #[tokio::test]
    async fn emits_a_note_activity_record_naming_the_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let metadata = write_skill(
            &root,
            ".claude/skills/a",
            "---\nname: a\ndescription: A.\n---\nbody\n",
        );
        let (ctx, mut rx) = ctx_with_activity(root, vec![metadata]);

        let tool = Skill::new(ctx);
        tool.call(SkillArgs { name: "a".to_string() }).await.unwrap();

        let (agent, activity) = rx.try_recv().expect("a Note record must be emitted");
        assert_eq!(agent, AgentId::ROOT);
        assert_eq!(
            activity,
            ToolActivity::Note {
                text: "skill `a` loaded".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_skill_md_removed_after_discovery_is_not_found_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let metadata = SkillMetadata {
            name: "ghost".to_string(),
            description: "Gone.".to_string(),
            dir: PathBuf::from(".claude/skills/ghost"),
        };

        let tool = Skill::new(ctx(root, vec![metadata]));
        let err = tool
            .call(SkillArgs {
                name: "ghost".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ToolFailure::NotFound(_)));
    }
}
