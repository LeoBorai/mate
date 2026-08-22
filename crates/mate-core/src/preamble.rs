//! `render_preamble` (§4, `M1-4`): turns a workspace root, host OS, and an agent's tool list
//! into the system preamble that ends up in [`crate::config::AgentSpec::preamble`].
//! `build_agent` (`M1-2`) just forwards whatever preamble the spec carries — this is the
//! function that produces it.
//!
//! Two variants, not two functions: [`PreambleRole`] picks the intro paragraph, but the
//! workspace/OS/tool-list scaffolding is identical, since a subagent's `ToolCtx` is the same
//! shape as a root agent's, just narrower (§7.4). The subagent variant carries the §7.6
//! context-firewall reminder and the §7.5 "summarize, don't dump" instruction — the two things
//! that go wrong first when a model is delegated to instead of talked to directly.
//!
//! Tool crates land in `M4`, so [`ToolDescriptor`] is a plain data pair rather than anything
//! tied to `rig::tool::Tool` — a caller today can pass an empty slice or a hand-built list, and
//! `M4` wiring later derives the real list from each agent's attached `ToolSet`.

use std::path::Path;

/// One tool available to the agent, as shown in the preamble's tool list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
}

impl ToolDescriptor {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// One Skill available to the agent, as shown in the preamble's "Available skills" section.
/// A separate type from [`ToolDescriptor`] even though the shape is identical — a skill isn't
/// a tool, and folding it into "Available tools" would misrepresent what the entry is; the
/// `skill` tool itself (the thing that *loads* one) is the `ToolDescriptor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDescriptor {
    pub name: String,
    pub description: String,
}

impl SkillDescriptor {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// Which kind of agent the preamble is for (§4: "the same builder makes root agents and
/// subagents"). Changes the intro paragraph, not the workspace/OS/tool-list scaffolding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreambleRole {
    Root,
    Subagent,
}

impl PreambleRole {
    fn intro(self) -> &'static str {
        match self {
            PreambleRole::Root => "You are mate, a terminal coding agent.",
            PreambleRole::Subagent => {
                "You are a subordinate agent spawned by mate to complete one narrow task.\n\
                 You do not see the parent conversation and never will: your entire input is \
                 this preamble plus the task you were given. Do not ask for missing context, \
                 and do not wait for clarification; use the tools you have to complete the \
                 task as given.\n\
                 Your final answer becomes the whole report the parent agent receives. Keep \
                 it short: summarize what you found or did, and do not paste raw file \
                 contents or tool output verbatim."
            }
        }
    }
}

/// Renders the system preamble for `role`, given the session's workspace root, the host OS,
/// and the tools this particular agent has attached.
pub fn render_preamble(
    role: PreambleRole,
    workspace_root: &Path,
    os: &str,
    tools: &[ToolDescriptor],
    skills: &[SkillDescriptor],
) -> String {
    let scaffold = format!(
        "Workspace root: {}\nOperating system: {os}",
        workspace_root.display()
    );

    let tool_list = if tools.is_empty() {
        "Available tools:\n(none)".to_string()
    } else {
        let lines: Vec<String> = tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect();
        format!("Available tools:\n{}", lines.join("\n"))
    };

    let mut sections = vec![role.intro(), scaffold.as_str(), tool_list.as_str()];

    // Omitted entirely when empty (rather than a "(none)" placeholder like the tool list
    // above) — most workspaces have zero skills, and a section that reads empty everywhere
    // would just be noise the tool list, which always has entries, doesn't have to deal with.
    let skill_list = if skills.is_empty() {
        None
    } else {
        let lines: Vec<String> = skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect();
        Some(format!(
            "Available skills:\n{}\n\nLoad one with the `skill` tool before following its \
             instructions.",
            lines.join("\n")
        ))
    };
    if let Some(skill_list) = &skill_list {
        sections.push(skill_list.as_str());
    }

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tools() -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor::new(
                "read_file",
                "Read a file within the workspace, optionally by line range.",
            ),
            ToolDescriptor::new(
                "list_dir",
                "List one level of a directory, .gitignore-aware.",
            ),
        ]
    }

    fn sample_skills() -> Vec<SkillDescriptor> {
        vec![SkillDescriptor::new(
            "pdf-processing",
            "Extract text and tables from PDF files.",
        )]
    }

    /// Pins the exact rendered output (`M1-4`'s "snapshot test"): a golden string literal
    /// rather than `insta`, so a wording change is a visible one-line diff in this file
    /// instead of a separate `.snap` file to keep in sync.
    #[test]
    fn root_preamble_snapshot() {
        let rendered = render_preamble(
            PreambleRole::Root,
            Path::new("/work/api"),
            "linux",
            &sample_tools(),
            &[],
        );

        assert_eq!(
            rendered,
            "You are mate, a terminal coding agent.\n\n\
             Workspace root: /work/api\n\
             Operating system: linux\n\n\
             Available tools:\n\
             - read_file: Read a file within the workspace, optionally by line range.\n\
             - list_dir: List one level of a directory, .gitignore-aware."
        );
    }

    #[test]
    fn subagent_preamble_snapshot() {
        let tools = vec![ToolDescriptor::new(
            "read_file",
            "Read a file within the workspace.",
        )];
        let rendered = render_preamble(
            PreambleRole::Subagent,
            Path::new("/work/api"),
            "linux",
            &tools,
            &[],
        );

        assert_eq!(
            rendered,
            "You are a subordinate agent spawned by mate to complete one narrow task.\n\
             You do not see the parent conversation and never will: your entire input is \
             this preamble plus the task you were given. Do not ask for missing context, \
             and do not wait for clarification; use the tools you have to complete the \
             task as given.\n\
             Your final answer becomes the whole report the parent agent receives. Keep \
             it short: summarize what you found or did, and do not paste raw file \
             contents or tool output verbatim.\n\n\
             Workspace root: /work/api\n\
             Operating system: linux\n\n\
             Available tools:\n\
             - read_file: Read a file within the workspace."
        );
    }

    #[test]
    fn empty_tool_list_renders_a_placeholder_instead_of_an_empty_section() {
        let rendered = render_preamble(
            PreambleRole::Root,
            Path::new("/work/api"),
            "linux",
            &[],
            &[],
        );
        assert!(rendered.ends_with("Available tools:\n(none)"));
    }

    #[test]
    fn root_and_subagent_intros_differ_for_the_same_workspace_and_tools() {
        let tools = sample_tools();
        let root = render_preamble(
            PreambleRole::Root,
            Path::new("/work/api"),
            "linux",
            &tools,
            &[],
        );
        let subagent = render_preamble(
            PreambleRole::Subagent,
            Path::new("/work/api"),
            "linux",
            &tools,
            &[],
        );
        assert_ne!(root, subagent);
        assert!(subagent.contains("You do not see the parent conversation"));
    }

    #[test]
    fn an_empty_skill_list_omits_the_available_skills_section_entirely() {
        let rendered = render_preamble(
            PreambleRole::Root,
            Path::new("/work/api"),
            "linux",
            &sample_tools(),
            &[],
        );
        assert!(!rendered.contains("Available skills"));
    }

    #[test]
    fn a_non_empty_skill_list_renders_after_the_tool_list() {
        let rendered = render_preamble(
            PreambleRole::Root,
            Path::new("/work/api"),
            "linux",
            &sample_tools(),
            &sample_skills(),
        );

        assert!(
            rendered.ends_with(
                "Available skills:\n\
                 - pdf-processing: Extract text and tables from PDF files.\n\n\
                 Load one with the `skill` tool before following its instructions."
            ),
            "skills section must render after the tool list: {rendered}"
        );
    }
}
