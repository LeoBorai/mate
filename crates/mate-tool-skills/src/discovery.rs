//! [`discover_skills`]: one-level, non-recursive scan of every skills directory `mate`
//! recognizes, under a session's workspace root. Not a general glob walk (unlike
//! `mate-tool-fs::find_files`) — the shape is fixed: `<source>/<name>/SKILL.md`.

use std::collections::HashMap;
use std::path::Path;

use mate_tool_api::SkillMetadata;

use crate::frontmatter;

/// Precedence order: earlier wins on a `name` collision. `.claude/skills` is listed first
/// because this repo's own layout expects it to be the canonical copy (frequently a symlink
/// into `.agents/skills/*` — see `.claude/skills/mate-software-engineer`).
const SKILL_SOURCE_DIRS: [&str; 4] = [
    ".claude/skills",
    ".opencode/skills",
    ".copilot/skills",
    ".agents/skills",
];

/// Discovers every valid `SKILL.md` under `root`'s recognized skills directories. A skill
/// directory with no `SKILL.md`, an unreadable `SKILL.md`, or malformed/invalid frontmatter is
/// skipped with a `tracing::warn!` rather than failing the whole scan — one bad skill shouldn't
/// break a session. Result is sorted by `name` for deterministic preamble/tool-listing output.
pub fn discover_skills(root: &Path) -> Vec<SkillMetadata> {
    let mut found: HashMap<String, SkillMetadata> = HashMap::new();

    for source in SKILL_SOURCE_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(source)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let skill_dir = entry.path();
            let skill_md = skill_dir.join("SKILL.md");
            let content = match std::fs::read_to_string(&skill_md) {
                Ok(content) => content,
                Err(_) => continue,
            };

            let parsed = match frontmatter::parse(&content) {
                Ok(parsed) => parsed,
                Err(error) => {
                    tracing::warn!(
                        path = %skill_md.display(),
                        %error,
                        "skipping malformed SKILL.md"
                    );
                    continue;
                }
            };

            // `HashMap::entry().or_insert_with` only inserts when the key is absent, so
            // iterating `SKILL_SOURCE_DIRS` in precedence order is what makes the first
            // source's `name` win a collision — nothing here re-checks precedence explicitly.
            found.entry(parsed.name.clone()).or_insert_with(|| {
                let dir = skill_dir.strip_prefix(root).unwrap_or(&skill_dir);
                SkillMetadata {
                    name: parsed.name,
                    description: parsed.description,
                    dir: dir.to_path_buf(),
                }
            });
        }
    }

    let mut skills: Vec<SkillMetadata> = found.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn write_skill(root: &Path, source: &str, name: &str, description: &str) {
        let dir = root.join(source).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody.\n"),
        )
        .unwrap();
    }

    #[test]
    fn returns_an_empty_list_when_no_skills_directory_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(discover_skills(tmp.path()), Vec::new());
    }

    #[test]
    fn discovers_a_skill_under_dot_claude_skills() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            ".claude/skills",
            "pdf-processing",
            "Extract PDFs.",
        );

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf-processing");
        assert_eq!(skills[0].description, "Extract PDFs.");
        assert_eq!(
            skills[0].dir,
            PathBuf::from(".claude/skills/pdf-processing")
        );
    }

    #[test]
    fn discovers_skills_under_every_recognized_source_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), ".claude/skills", "a", "A.");
        write_skill(tmp.path(), ".opencode/skills", "b", "B.");
        write_skill(tmp.path(), ".copilot/skills", "c", "C.");
        write_skill(tmp.path(), ".agents/skills", "d", "D.");

        let skills = discover_skills(tmp.path());
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn dot_claude_skills_wins_a_name_collision_over_dot_agents_skills() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), ".claude/skills", "shared", "from claude");
        write_skill(tmp.path(), ".agents/skills", "shared", "from agents");

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1, "one name collision must fold to one entry");
        assert_eq!(skills[0].description, "from claude");
        assert_eq!(skills[0].dir, PathBuf::from(".claude/skills/shared"));
    }

    #[test]
    fn a_directory_with_no_skill_md_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude/skills/empty")).unwrap();

        assert_eq!(discover_skills(tmp.path()), Vec::new());
    }

    #[test]
    fn malformed_frontmatter_is_skipped_without_failing_the_whole_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let bad_dir = tmp.path().join(".claude/skills/broken");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("SKILL.md"), "not a valid skill file").unwrap();
        write_skill(tmp.path(), ".claude/skills", "good", "Good skill.");

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
    }

    #[test]
    fn results_are_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), ".claude/skills", "zebra", "Z.");
        write_skill(tmp.path(), ".claude/skills", "alpha", "A.");

        let skills = discover_skills(tmp.path());
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zebra"]);
    }
}
