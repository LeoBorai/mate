//! Subagent roster (§9.6, `M12-6`) — one tab's live + recent subagents, fed by
//! `AgentEvent::SubagentSpawned`/`SubagentFinished`/`Activity`/`ApprovalRequired`/`Usage`
//! events tagged with a non-root [`AgentId`] (`crate::app::App::on_session_event`).
//!
//! **One line per subagent is a hard constraint** (§9.6) — there's no field here for a second
//! line, so a caller can't accidentally grow one. The activity text is derived mechanically
//! from a subagent's own most recent [`ToolActivity`], never from the model: asking the
//! subagent to narrate itself costs tokens and can lie, deriving from its tool calls is free
//! and can't.

use std::collections::VecDeque;
use std::time::Instant;

use mate_tool_api::{AgentId, FileOp, SubagentOutcome, ToolActivity};

use crate::text::truncate_end;

/// §9.6's activity budget — the roster row itself does its own width-aware truncation on top
/// of this at render time, but nothing upstream should ever hand it more than this to begin
/// with.
const ACTIVITY_CHARS: usize = 24;
/// Roster rows actually shown (§9.2's `1 + n (cap 8)`); [`Roster::len`] beyond this renders as
/// `+n more`.
pub(crate) const ROSTER_SHOWN: usize = 8;
/// Total rows retained, well past what's ever shown — bounds a long session's memory the same
/// way the network/documents ring buffers do (§9.11).
const ROSTER_CAPACITY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentStatus {
    /// §9.6 lists this alongside `Running` for a subagent still queued behind
    /// `max_concurrent` (`M9-5`) — today that queuing happens entirely inside
    /// `SubagentRunner::acquire`, before `AgentEvent::SubagentSpawned` ever fires, so nothing
    /// in this crate can observe or construct a `Queued` row yet. Forward-declared for the
    /// same reason `mate_tool_api::FileOp::Write` is (§9.3): a variant costs nothing unused, a
    /// breaking match-arm addition later costs every downstream `match`.
    #[allow(dead_code)]
    Queued,
    Running,
    AwaitingApproval,
    Done,
    Failed,
    Cancelled,
    TimedOut,
}

impl SubagentStatus {
    /// The one-glyph status marker a roster row leads with (§9.6's mockup: `⣾`/`✓`/`✗`/`⊘`).
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            SubagentStatus::Queued => "⋯",
            SubagentStatus::Running => "⣾",
            SubagentStatus::AwaitingApproval => "?",
            SubagentStatus::Done => "✓",
            SubagentStatus::Failed => "✗",
            SubagentStatus::Cancelled => "⊘",
            SubagentStatus::TimedOut => "⏱",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            SubagentStatus::Done
                | SubagentStatus::Failed
                | SubagentStatus::Cancelled
                | SubagentStatus::TimedOut
        )
    }
}

pub(crate) struct SubagentRow {
    pub(crate) id: AgentId,
    pub(crate) label: String,
    pub(crate) status: SubagentStatus,
    pub(crate) started: Instant,
    pub(crate) activity: String,
    pub(crate) turns: usize,
}

/// One tab's roster, newest spawn first — a finished row stays exactly where it was rather
/// than moving, so a fan-out's original order survives to its summary (§9.6: "stay through
/// the end of the turn, then dim into a recent group").
#[derive(Default)]
pub(crate) struct Roster {
    rows: VecDeque<SubagentRow>,
}

impl Roster {
    pub(crate) fn spawn(&mut self, id: AgentId, label: String) {
        self.rows.push_front(SubagentRow {
            id,
            label,
            status: SubagentStatus::Running,
            started: Instant::now(),
            activity: "starting…".to_string(),
            turns: 0,
        });
        self.rows.truncate(ROSTER_CAPACITY);
    }

    fn row_mut(&mut self, id: AgentId) -> Option<&mut SubagentRow> {
        self.rows.iter_mut().find(|r| r.id == id)
    }

    /// Folds one `ToolActivity` into `id`'s activity line (§9.6's derivation table) — a no-op
    /// for an id this roster never saw spawned, and for `Note` (dropped the same as the
    /// network/documents logs drop it, per `crate::panel::Panel::push`) it still updates, since
    /// a note *is* the activity line's content for that widget.
    pub(crate) fn note_activity(&mut self, id: AgentId, activity: &ToolActivity) {
        let text = derive_activity(activity);
        if let Some(row) = self.row_mut(id) {
            row.activity = text;
        }
    }

    pub(crate) fn awaiting_approval(&mut self, id: AgentId) {
        if let Some(row) = self.row_mut(id) {
            row.status = SubagentStatus::AwaitingApproval;
            row.activity = "needs approval".to_string();
        }
    }

    /// One `AgentEvent::Usage` tagged with `id` — a completion round inside that subagent's
    /// own turn, the same cadence `SubagentReport::turns` counts at (`crate::subagent`'s
    /// `drive_subagent` increments its own counter on exactly this event).
    pub(crate) fn record_turn(&mut self, id: AgentId) {
        if let Some(row) = self.row_mut(id) {
            row.turns += 1;
        }
    }

    pub(crate) fn finish(&mut self, id: AgentId, outcome: &SubagentOutcome) {
        if let Some(row) = self.row_mut(id) {
            row.status = match outcome {
                SubagentOutcome::Completed { .. } => SubagentStatus::Done,
                SubagentOutcome::Failed { .. } => SubagentStatus::Failed,
                SubagentOutcome::Cancelled => SubagentStatus::Cancelled,
                SubagentOutcome::TimedOut => SubagentStatus::TimedOut,
            };
        }
    }

    pub(crate) fn rows(&self) -> &VecDeque<SubagentRow> {
        &self.rows
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

/// §9.6's derivation table, verbatim: `FileTouched`/`NetRequest`/`Note` map to a short present-
/// tense line; nothing here ever asks the subagent itself what it's doing.
fn derive_activity(activity: &ToolActivity) -> String {
    let text = match activity {
        ToolActivity::FileTouched { path, op, .. } => {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            let verb = match op {
                FileOp::Read => "reading",
                FileOp::Write => "writing",
                FileOp::Create => "creating",
                FileOp::Delete => "deleting",
            };
            format!("{verb} {name}")
        }
        ToolActivity::NetRequest {
            method,
            host,
            reason: None,
            ..
        } => format!("{method} {host}"),
        ToolActivity::NetRequest {
            host,
            reason: Some(_),
            ..
        } => format!("blocked {host}"),
        ToolActivity::Note { text } => text.clone(),
        ToolActivity::SkillLoaded { name } => format!("loaded skill {name}"),
    };
    truncate_end(&text, ACTIVITY_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn spawned() -> Roster {
        let mut roster = Roster::default();
        roster.spawn(AgentId(1), "deps".to_string());
        roster
    }

    #[test]
    fn a_fresh_spawn_starts_running_with_the_starting_placeholder() {
        let roster = spawned();
        let row = &roster.rows()[0];
        assert_eq!(row.status, SubagentStatus::Running);
        assert_eq!(row.activity, "starting…");
        assert_eq!(row.turns, 0);
    }

    #[test]
    fn a_file_read_derives_a_reading_line_named_after_the_file_not_the_full_path() {
        let mut roster = spawned();
        roster.note_activity(
            AgentId(1),
            &ToolActivity::FileTouched {
                path: PathBuf::from("crates/mate-core/src/build.rs"),
                op: FileOp::Read,
                lines: 10,
                bytes: 100,
            },
        );
        assert_eq!(roster.rows()[0].activity, "reading build.rs");
    }

    #[test]
    fn a_blocked_request_reads_as_blocked_not_as_a_normal_get() {
        let mut roster = spawned();
        roster.note_activity(
            AgentId(1),
            &ToolActivity::NetRequest {
                method: http::Method::GET,
                host: "169.254.169.254".to_string(),
                path: "/".to_string(),
                status: None,
                ms: 0,
                bytes: 0,
                redirects: 0,
                reason: Some("private-ip".to_string()),
            },
        );
        assert_eq!(roster.rows()[0].activity, "blocked 169.254.169.254");
    }

    #[test]
    fn activity_for_an_id_never_spawned_is_a_harmless_no_op() {
        let mut roster = Roster::default();
        roster.note_activity(
            AgentId(99),
            &ToolActivity::Note {
                text: "hi".to_string(),
            },
        );
        assert_eq!(roster.len(), 0);
    }

    #[test]
    fn finish_maps_every_outcome_to_its_own_terminal_status() {
        let mut roster = spawned();
        roster.finish(
            AgentId(1),
            &SubagentOutcome::Completed {
                summary: "done".to_string(),
            },
        );
        assert_eq!(roster.rows()[0].status, SubagentStatus::Done);
        assert!(roster.rows()[0].status.is_terminal());
    }

    #[test]
    fn record_turn_increments_only_the_matching_row() {
        let mut roster = spawned();
        roster.spawn(AgentId(2), "buildrs".to_string());

        roster.record_turn(AgentId(1));
        roster.record_turn(AgentId(1));

        let by_id =
            |roster: &Roster, id: AgentId| roster.rows().iter().find(|r| r.id == id).unwrap().turns;
        assert_eq!(by_id(&roster, AgentId(1)), 2);
        assert_eq!(by_id(&roster, AgentId(2)), 0);
    }

    #[test]
    fn the_roster_is_capped_well_past_what_the_panel_ever_shows() {
        let mut roster = Roster::default();
        for i in 0..40 {
            roster.spawn(AgentId(i), format!("s{i}"));
        }
        assert_eq!(roster.len(), ROSTER_CAPACITY);
    }
}
