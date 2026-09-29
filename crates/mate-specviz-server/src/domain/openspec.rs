//! Pure readers for OpenSpec's Markdown conventions — task checkboxes,
//! requirement headings, delta section headings, and the canonical order of
//! a change's artifacts. No I/O: the repository feeds file contents in, the
//! renderer feeds heading text in.

use serde::Serialize;

/// Checked vs. total task-list items in a change's `tasks.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskProgress {
    pub done: usize,
    pub total: usize,
}

/// Counts `- [ ]` / `- [x]` task-list items (any of `-`, `*`, `+` as the
/// bullet; `x` or `X` means done, any other single character inside the
/// brackets counts as not done — the same reading OpenSpec's own apply phase
/// uses). Lines inside fenced code blocks are ignored. `None` when there are
/// no task-list items at all, so "no tasks" never renders as `0/0`.
pub fn task_progress(markdown: &str) -> Option<TaskProgress> {
    let mut progress = TaskProgress { done: 0, total: 0 };

    for line in outside_code_fences(markdown) {
        let Some(checked) = task_marker(line) else {
            continue;
        };
        progress.total += 1;
        if checked {
            progress.done += 1;
        }
    }

    (progress.total > 0).then_some(progress)
}

/// `Some(checked)` if `line` is a task-list item.
fn task_marker(line: &str) -> Option<bool> {
    let rest = line.trim_start();
    let rest = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))?;
    let rest = rest.strip_prefix('[')?;
    let mut chars = rest.chars();
    let mark = chars.next()?;
    let after = chars.as_str().strip_prefix(']')?;
    if !(after.is_empty() || after.starts_with(char::is_whitespace)) {
        return None;
    }
    Some(matches!(mark, 'x' | 'X'))
}

/// Number of `### Requirement:` headings, ignoring fenced code blocks.
pub fn requirement_count(markdown: &str) -> usize {
    outside_code_fences(markdown)
        .filter(|line| {
            line.trim_start()
                .strip_prefix("### ")
                .is_some_and(is_requirement_heading)
        })
        .count()
}

/// Whether a level-3 heading's text (without the `### `) names a requirement.
pub fn is_requirement_heading(text: &str) -> bool {
    text.trim_start().starts_with("Requirement:")
}

/// Whether a level-4 heading's text names a scenario.
pub fn is_scenario_heading(text: &str) -> bool {
    text.trim_start().starts_with("Scenario:")
}

/// The four delta operations a change's spec can carry, one per `## ...
/// Requirements` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOp {
    Added,
    Modified,
    Removed,
    Renamed,
}

impl DeltaOp {
    /// The label shown in the rendered badge, and the suffix of its CSS class.
    pub fn label(self) -> &'static str {
        match self {
            DeltaOp::Added => "ADDED",
            DeltaOp::Modified => "MODIFIED",
            DeltaOp::Removed => "REMOVED",
            DeltaOp::Renamed => "RENAMED",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            DeltaOp::Added => "sv-delta-added",
            DeltaOp::Modified => "sv-delta-modified",
            DeltaOp::Removed => "sv-delta-removed",
            DeltaOp::Renamed => "sv-delta-renamed",
        }
    }
}

/// Parses a level-2 heading's text as a delta section, e.g.
/// `"ADDED Requirements"` (case-insensitive, surrounding whitespace ignored).
pub fn delta_op(heading: &str) -> Option<DeltaOp> {
    let (op, rest) = heading.trim().split_once(char::is_whitespace)?;
    if !rest.trim().eq_ignore_ascii_case("requirements") {
        return None;
    }
    [
        DeltaOp::Added,
        DeltaOp::Modified,
        DeltaOp::Removed,
        DeltaOp::Renamed,
    ]
    .into_iter()
    .find(|candidate| op.eq_ignore_ascii_case(candidate.label()))
}

/// Sort key for a file inside a change directory (slash-separated, relative
/// to the change): proposal, delta specs, design, tasks, then everything
/// else. Ties are broken by path at the call site.
pub fn artifact_rank(relative_path: &str) -> u8 {
    match relative_path {
        "proposal.md" => 0,
        p if p.starts_with("specs/") => 1,
        "design.md" => 2,
        "tasks.md" => 3,
        _ => 4,
    }
}

/// Lines of `markdown` that are not inside a ```` ``` ```` / `~~~` fenced
/// code block (fence lines themselves excluded too).
fn outside_code_fences(markdown: &str) -> impl Iterator<Item = &str> {
    let mut in_fence = false;
    markdown.lines().filter(move |line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            return false;
        }
        !in_fence
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_checked_and_unchecked_tasks() {
        let md = "# Tasks\n\n## 1. Group\n\n- [x] 1.1 done\n- [ ] 1.2 open\n- [X] 1.3 upper done\n";
        assert_eq!(
            task_progress(md),
            Some(TaskProgress { done: 2, total: 3 }),
            "`[x]` and `[X]` both count as done; `[ ]` counts only toward the total"
        );
    }

    #[test]
    fn thirteen_of_twenty_one() {
        let mut md = String::new();
        for i in 0..21 {
            md.push_str(if i < 13 { "- [x] t\n" } else { "- [ ] t\n" });
        }
        assert_eq!(
            task_progress(&md),
            Some(TaskProgress {
                done: 13,
                total: 21
            }),
            "the spec's partially-complete scenario reads as 13/21"
        );
    }

    #[test]
    fn a_file_with_no_task_items_has_no_progress() {
        assert_eq!(
            task_progress("# Tasks\n\nNothing yet.\n- plain bullet\n"),
            None,
            "no task-list items means no progress badge, not 0/0"
        );
    }

    #[test]
    fn other_markers_count_as_not_done() {
        assert_eq!(
            task_progress("- [~] partial\n- [-] skipped\n"),
            Some(TaskProgress { done: 0, total: 2 }),
            "only x/X marks an item done, matching OpenSpec's own reading"
        );
    }

    #[test]
    fn tasks_inside_code_fences_are_ignored() {
        let md = "- [x] real\n```\n- [ ] example in a fence\n```\n";
        assert_eq!(
            task_progress(md),
            Some(TaskProgress { done: 1, total: 1 }),
            "a fenced example is documentation, not a task"
        );
    }

    #[test]
    fn a_link_in_brackets_is_not_a_task() {
        assert_eq!(
            task_progress("- [a](http://x) link\n"),
            None,
            "a bracket not followed by `]` and whitespace is not a checkbox"
        );
    }

    #[test]
    fn counts_requirement_headings() {
        let md = "## ADDED Requirements\n\n### Requirement: A\n\n#### Scenario: s\n\n### Requirement: B\n### Requirement: C\n### Requirement: D\n### Not a requirement\n";
        assert_eq!(
            requirement_count(md),
            4,
            "only `### Requirement:` headings count"
        );
    }

    #[test]
    fn parses_delta_headings_case_insensitively() {
        assert_eq!(delta_op("ADDED Requirements"), Some(DeltaOp::Added), "canonical form");
        assert_eq!(
            delta_op("  modified requirements "),
            Some(DeltaOp::Modified),
            "case and surrounding whitespace don't matter"
        );
        assert_eq!(delta_op("REMOVED Requirements"), Some(DeltaOp::Removed), "removed");
        assert_eq!(delta_op("RENAMED Requirements"), Some(DeltaOp::Renamed), "renamed");
        assert_eq!(delta_op("ADDED Things"), None, "must end in `Requirements`");
        assert_eq!(delta_op("Requirements"), None, "needs an operation word");
    }

    #[test]
    fn orders_standard_change_artifacts() {
        let mut files = vec!["tasks.md", "design.md", "proposal.md", "specs/foo/spec.md"];
        files.sort_by_key(|f| (artifact_rank(f), *f));
        assert_eq!(
            files,
            vec!["proposal.md", "specs/foo/spec.md", "design.md", "tasks.md"],
            "proposal, delta specs, design, tasks"
        );
    }

    #[test]
    fn extra_artifacts_follow_the_standard_ones() {
        let mut files = vec!["research.md", "tasks.md", "proposal.md"];
        files.sort_by_key(|f| (artifact_rank(f), *f));
        assert_eq!(
            files,
            vec!["proposal.md", "tasks.md", "research.md"],
            "a custom schema's extra file sorts after the known artifacts"
        );
    }
}
