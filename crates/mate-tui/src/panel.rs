//! Network/documents activity log data for the side panel (§9.3/§9.7/§9.8) — the ring-buffered
//! half of the full `M12` [`crate::panel_widgets::AgentStatusPanel`] (model/context/subagent-
//! roster live in [`crate::app::SessionTab`] and [`crate::roster::Roster`] instead), folded
//! from `ToolActivity` records off the shared `AgentId`-tagged sink `M11-4` wired up. Kept on
//! [`crate::app::SessionTab`], never on `App` (§9.1) — a tab rooted at one workspace showing
//! another tab's file reads would be actively misleading.

use std::collections::VecDeque;
use std::path::PathBuf;

use mate_tool_api::{AgentId, FileOp, SkillMetadata, ToolActivity};

/// Ring buffer bound for both logs (§9.7/§9.8): a session that runs for an hour must not
/// accumulate thousands of rows nobody scrolls to.
const RING_CAPACITY: usize = 50;

pub(crate) struct NetRow {
    pub(crate) agent: AgentId,
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) status: Option<u16>,
    pub(crate) ms: u64,
    pub(crate) reason: Option<String>,
}

pub(crate) struct DocRow {
    pub(crate) agent: AgentId,
    pub(crate) path: PathBuf,
    pub(crate) op: FileOp,
    pub(crate) lines: usize,
}

/// One discovered skill, as shown in the SKILLS widget — the catalog (`name`/`description`) is
/// fixed at tab-open time (`Panel::new`, from the same `ToolCtx::skills` the session's toolset
/// was built from); `active` flips true the first time this session sees a `SkillLoaded` record
/// naming it, and stays true for the rest of the session (§9.3-style "sticky" state, not a live
/// "in use right now" flag — a skill's instructions stay relevant to the conversation long after
/// the `skill` tool call that loaded them returns).
pub(crate) struct SkillRow {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) active: bool,
}

/// One tab's tool activity log, newest first in both buffers, plus the skill catalog (§9.3/
/// §9.7/§9.8/skills).
#[derive(Default)]
pub(crate) struct Panel {
    pub(crate) network: VecDeque<NetRow>,
    pub(crate) documents: VecDeque<DocRow>,
    /// Requests folded in since the last [`Self::reset_turn`] — the network widget's per-turn
    /// header count (§9.7), "is this thing hammering something right now".
    pub(crate) turn_requests: u32,
    /// Every skill discovered for this session's workspace root, in discovery order (already
    /// sorted by name) — fixed for the tab's lifetime; only each row's `active` flag changes.
    pub(crate) skills: Vec<SkillRow>,
}

impl Panel {
    /// Builds a tab's panel seeded with its discovered skill catalog — the same
    /// `ToolCtx::skills` list the tab's toolset was built from, so the SKILLS widget never
    /// lists a skill the `skill` tool couldn't actually load, or vice versa. Every other field
    /// starts empty, same as [`Panel::default`].
    pub(crate) fn new(catalog: Vec<SkillMetadata>) -> Self {
        Self {
            skills: catalog
                .into_iter()
                .map(|s| SkillRow {
                    name: s.name,
                    description: s.description,
                    active: false,
                })
                .collect(),
            ..Self::default()
        }
    }

    /// Called once per prompt sent (`crate::app::App::on_key`), so the count answers "this
    /// turn", not "this session".
    pub(crate) fn reset_turn(&mut self) {
        self.turn_requests = 0;
    }

    /// Folds one `ToolActivity` record into the log, tagged with whichever agent produced it —
    /// root or subagent, both share this one panel (§9.3). `Note` carries nothing either log
    /// renders, so it's dropped rather than given a row.
    pub(crate) fn push(&mut self, agent: AgentId, activity: ToolActivity) {
        match activity {
            ToolActivity::NetRequest {
                host,
                path,
                status,
                ms,
                reason,
                ..
            } => {
                self.turn_requests += 1;
                self.network.push_front(NetRow {
                    agent,
                    host,
                    path,
                    status,
                    ms,
                    reason,
                });
                self.network.truncate(RING_CAPACITY);
            }
            ToolActivity::FileTouched {
                path, op, lines, ..
            } => {
                // Repeat reads of one file update the existing row and jump it back to the
                // front rather than appending a duplicate (§9.8) — a subagent looping on one
                // file should look like one row going stale, not runaway growth.
                if let Some(existing) = self.documents.iter().position(|d| d.path == path) {
                    self.documents.remove(existing);
                }
                self.documents.push_front(DocRow {
                    agent,
                    path,
                    op,
                    lines,
                });
                self.documents.truncate(RING_CAPACITY);
            }
            ToolActivity::Note { .. } => {}
            // Transcript-only (§ write_file diff preview) — the DOCUMENTS log already has this
            // write via the `FileTouched` record sent alongside it.
            ToolActivity::FileDiff { .. } => {}
            ToolActivity::SkillLoaded { name } => {
                if let Some(row) = self.skills.iter_mut().find(|s| s.name == name) {
                    row.active = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(status: Option<u16>) -> ToolActivity {
        ToolActivity::NetRequest {
            method: http::Method::GET,
            host: "example.com".to_string(),
            path: "/".to_string(),
            status,
            ms: 10,
            bytes: 0,
            redirects: 0,
            reason: None,
        }
    }

    fn touched(path: &str, lines: usize) -> ToolActivity {
        ToolActivity::FileTouched {
            path: PathBuf::from(path),
            op: FileOp::Read,
            lines,
            bytes: 0,
        }
    }

    #[test]
    fn network_requests_count_toward_the_per_turn_header_and_reset() {
        let mut panel = Panel::default();
        panel.push(AgentId::ROOT, net(Some(200)));
        panel.push(AgentId::ROOT, net(Some(200)));
        assert_eq!(panel.turn_requests, 2);

        panel.reset_turn();
        assert_eq!(panel.turn_requests, 0);
        assert_eq!(
            panel.network.len(),
            2,
            "resetting the turn counter must not clear the log itself"
        );
    }

    #[test]
    fn a_repeat_read_of_one_path_updates_its_row_instead_of_appending() {
        let mut panel = Panel::default();
        panel.push(AgentId::ROOT, touched("a.txt", 10));
        panel.push(AgentId::ROOT, touched("b.txt", 5));
        panel.push(AgentId::ROOT, touched("a.txt", 12));

        assert_eq!(
            panel.documents.len(),
            2,
            "the second read of a.txt must update its row, not add a third"
        );
        assert_eq!(panel.documents[0].path, PathBuf::from("a.txt"));
        assert_eq!(
            panel.documents[0].lines, 12,
            "the row must carry the latest read's data"
        );
    }

    #[test]
    fn a_note_is_dropped_without_touching_either_log() {
        let mut panel = Panel::default();
        panel.push(
            AgentId::ROOT,
            ToolActivity::Note {
                text: "hi".to_string(),
            },
        );
        assert!(panel.network.is_empty());
        assert!(panel.documents.is_empty());
    }

    #[test]
    fn each_ring_buffer_is_capped_at_ring_capacity() {
        let mut panel = Panel::default();
        for i in 0..RING_CAPACITY + 5 {
            panel.push(AgentId::ROOT, touched(&format!("f{i}.txt"), 1));
        }
        assert_eq!(panel.documents.len(), RING_CAPACITY);
    }

    fn skill(name: &str, description: &str) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            description: description.to_string(),
            dir: PathBuf::from(format!(".claude/skills/{name}")),
        }
    }

    #[test]
    fn new_seeds_the_catalog_with_every_row_inactive() {
        let panel = Panel::new(vec![skill("a", "A."), skill("b", "B.")]);
        assert_eq!(panel.skills.len(), 2);
        assert!(panel.skills.iter().all(|s| !s.active));
    }

    #[test]
    fn a_skill_loaded_record_activates_the_matching_row_only() {
        let mut panel = Panel::new(vec![skill("a", "A."), skill("b", "B.")]);
        panel.push(
            AgentId::ROOT,
            ToolActivity::SkillLoaded {
                name: "b".to_string(),
            },
        );

        assert!(!panel.skills.iter().find(|s| s.name == "a").unwrap().active);
        assert!(panel.skills.iter().find(|s| s.name == "b").unwrap().active);
    }

    #[test]
    fn a_skill_loaded_record_naming_an_unknown_skill_is_a_harmless_no_op() {
        let mut panel = Panel::new(vec![skill("a", "A.")]);
        panel.push(
            AgentId::ROOT,
            ToolActivity::SkillLoaded {
                name: "does-not-exist".to_string(),
            },
        );
        assert!(!panel.skills[0].active);
    }

    #[test]
    fn activation_stays_sticky_across_further_unrelated_activity() {
        let mut panel = Panel::new(vec![skill("a", "A.")]);
        panel.push(
            AgentId::ROOT,
            ToolActivity::SkillLoaded {
                name: "a".to_string(),
            },
        );
        panel.push(AgentId::ROOT, touched("b.txt", 1));
        assert!(
            panel.skills[0].active,
            "later, unrelated activity must not clear an already-active skill"
        );
    }
}
