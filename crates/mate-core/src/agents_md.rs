//! `discover_agents_md`: reads a workspace root's project-instructions file, checking every
//! filename other agent frameworks use for the same concept. First match wins; the rest are
//! ignored rather than concatenated — a `CLAUDE.md` and an `AGENTS.md` in the same root are the
//! same authored instructions aimed at two different tools, not two layers to merge.

use std::path::Path;

use mate_tool_api::{AgentsMdSource, truncate_with_notice};

/// Precedence order: earlier wins when more than one is present. `AGENTS.md` is the
/// spec-canonical name and wins outright; the rest are other agent frameworks' filenames for
/// the same concept (Claude Code, Gemini CLI, legacy Cursor).
const AGENTS_MD_FILENAMES: [&str; 4] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md", ".cursorrules"];

/// Reads the first file under `AGENTS_MD_FILENAMES` present at `root`. `None` if none exist or
/// none are readable — never an error; a missing project-instructions file is the common case.
pub fn discover_agents_md(root: &Path) -> Option<AgentsMdSource> {
    AGENTS_MD_FILENAMES.iter().find_map(|&filename| {
        std::fs::read_to_string(root.join(filename))
            .ok()
            .map(|content| AgentsMdSource { filename, content })
    })
}

/// [`discover_agents_md`] plus the `enabled`/`max_bytes` gate every session-build call site
/// (`mate-cli`'s plain frontend, `mate-tui`'s `session_factory`) otherwise repeats: `None`
/// outright when `enabled` is `false`, and content capped via `truncate_with_notice` when it
/// isn't.
pub fn discover_agents_md_capped(
    root: &Path,
    enabled: bool,
    max_bytes: usize,
) -> Option<AgentsMdSource> {
    if !enabled {
        return None;
    }
    discover_agents_md(root).map(|mut source| {
        source.content = truncate_with_notice(&source.content, max_bytes);
        source
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_when_no_recognized_filename_is_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(discover_agents_md(tmp.path()), None);
    }

    #[test]
    fn discovers_agents_md_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "Run `just test`.").unwrap();

        let source = discover_agents_md(tmp.path()).unwrap();
        assert_eq!(source.filename, "AGENTS.md");
        assert_eq!(source.content, "Run `just test`.");
    }

    #[test]
    fn falls_back_to_claude_md_when_agents_md_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "Claude-specific notes.").unwrap();

        let source = discover_agents_md(tmp.path()).unwrap();
        assert_eq!(source.filename, "CLAUDE.md");
        assert_eq!(source.content, "Claude-specific notes.");
    }

    #[test]
    fn agents_md_wins_over_claude_md_when_both_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "from AGENTS.md").unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "from CLAUDE.md").unwrap();

        let source = discover_agents_md(tmp.path()).unwrap();
        assert_eq!(
            source.filename, "AGENTS.md",
            "the spec-canonical name must win a collision over a legacy alias"
        );
        assert_eq!(source.content, "from AGENTS.md");
    }

    #[test]
    fn discover_agents_md_capped_returns_none_outright_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "some instructions").unwrap();

        assert_eq!(discover_agents_md_capped(tmp.path(), false, 1024), None);
    }

    #[test]
    fn discover_agents_md_capped_truncates_content_over_the_byte_cap() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "a".repeat(100)).unwrap();

        let source = discover_agents_md_capped(tmp.path(), true, 10).unwrap();
        assert!(
            source.content.len() < 100,
            "content over max_bytes must be truncated"
        );
        assert!(source.content.contains("truncated"));
    }

    #[test]
    fn an_unreadable_entry_falls_through_to_the_next_filename() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory named `AGENTS.md` fails `read_to_string`, the same way a permission
        // error or any other unreadable file would.
        std::fs::create_dir(tmp.path().join("AGENTS.md")).unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "from CLAUDE.md").unwrap();

        let source = discover_agents_md(tmp.path()).unwrap();
        assert_eq!(source.filename, "CLAUDE.md");
    }
}
