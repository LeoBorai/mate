//! The transcript model (`M7-3`): an append-only, capped `VecDeque<Entry>` fed by
//! `AgentEvent`s and the user's own submitted prompts. Tool calls land collapsed to one line;
//! `Ctrl+O` expands the most recent one (§9.9).

use std::collections::VecDeque;

/// Scrollback cap (`M7-3`'s "10k entries stay responsive") — oldest entries evicted past this.
const MAX_ENTRIES: usize = 10_000;

pub type EntryId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User {
        id: EntryId,
        text: String,
    },
    Assistant {
        id: EntryId,
        text: String,
    },
    ToolCall {
        id: EntryId,
        name: String,
        ok: Option<bool>,
        summary: String,
        expanded: bool,
    },
    SystemError {
        id: EntryId,
        text: String,
    },
    /// Local command feedback (`M13-3`'s slash commands) — the info-level counterpart to
    /// `SystemError`: an unknown command, a `/tokens` summary, a `/rename` confirmation. Never
    /// sent to the model; distinct from `SystemError` so the UI doesn't paint a `/model` reply
    /// the same alarming red as a real failure.
    System {
        id: EntryId,
        text: String,
    },
}

impl Entry {
    pub fn id(&self) -> EntryId {
        match self {
            Entry::User { id, .. }
            | Entry::Assistant { id, .. }
            | Entry::ToolCall { id, .. }
            | Entry::SystemError { id, .. }
            | Entry::System { id, .. } => *id,
        }
    }
}

/// Result of [`Transcript::push_token`]: the entry whose text just changed (new or appended-to,
/// always present so the caller can invalidate that id's [`crate::wrap::WrapCache`] entry), plus
/// the id evicted by the cap, if any.
pub struct TokenPush {
    pub changed: EntryId,
    pub evicted: Option<EntryId>,
}

pub struct Transcript {
    entries: VecDeque<Entry>,
    next_id: EntryId,
    /// `Some(id)` while the most recent entry is an assistant message still receiving
    /// `Token`s — the next `Token` appends to it instead of starting a new entry. Cleared by
    /// anything that interrupts a turn's text (a tool call, `TurnComplete`, an error, or the
    /// user starting a new prompt).
    open_assistant: Option<EntryId>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            next_id: 0,
            open_assistant: None,
        }
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn push(&mut self, build: impl FnOnce(EntryId) -> Entry) -> Option<EntryId> {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_back(build(id));
        if self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front().map(|e| e.id())
        } else {
            None
        }
    }

    /// Records the user's own submitted prompt. Returns the evicted entry's id, if the cap
    /// was exceeded.
    pub fn push_user(&mut self, text: String) -> Option<EntryId> {
        self.open_assistant = None;
        self.push(|id| Entry::User { id, text })
    }

    /// Appends one streamed token, opening a new assistant entry first if the last one isn't
    /// still receiving tokens (text and tool calls interleave within a turn, §4.1).
    pub fn push_token(&mut self, text: &str) -> TokenPush {
        if let Some(id) = self.open_assistant
            && let Some(Entry::Assistant { text: existing, .. }) =
                self.entries.iter_mut().rev().find(|e| e.id() == id)
        {
            existing.push_str(text);
            return TokenPush {
                changed: id,
                evicted: None,
            };
        }
        let evicted = self.push(|id| Entry::Assistant {
            id,
            text: text.to_string(),
        });
        let id = self.next_id - 1;
        self.open_assistant = Some(id);
        TokenPush {
            changed: id,
            evicted,
        }
    }

    pub fn push_tool_call(&mut self, name: String) -> Option<EntryId> {
        self.open_assistant = None;
        self.push(|id| Entry::ToolCall {
            id,
            name,
            ok: None,
            summary: String::new(),
            expanded: false,
        })
    }

    /// Fills in the most recent still-pending tool call matching `name` — `AgentEvent` doesn't
    /// correlate a result back to its call id, so the nearest pending call by name is the best
    /// available match.
    pub fn resolve_tool_call(&mut self, name: &str, ok: bool, summary: String) {
        if let Some(Entry::ToolCall {
            ok: slot_ok,
            summary: slot_summary,
            ..
        }) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| matches!(e, Entry::ToolCall { name: n, ok: None, .. } if n == name))
        {
            *slot_ok = Some(ok);
            *slot_summary = summary;
        }
    }

    pub fn push_error(&mut self, text: String) -> Option<EntryId> {
        self.open_assistant = None;
        self.push(|id| Entry::SystemError { id, text })
    }

    /// A slash command's own feedback (`M13-3`) — usage errors, `/tokens`/`/model` output, an
    /// unknown-command notice. Closes the open assistant entry the same way `push_error` does:
    /// a command is always typed as a fresh line, never a continuation of streamed text.
    pub fn push_system(&mut self, text: String) -> Option<EntryId> {
        self.open_assistant = None;
        self.push(|id| Entry::System { id, text })
    }

    pub fn end_turn(&mut self) {
        self.open_assistant = None;
    }

    /// Toggles the most recent tool call's expansion (`Ctrl+O`). No-op if the transcript has
    /// no tool call yet.
    pub fn toggle_last_tool_expanded(&mut self) {
        if let Some(Entry::ToolCall { expanded, .. }) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| matches!(e, Entry::ToolCall { .. }))
        {
            *expanded = !*expanded;
        }
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_append_to_the_open_assistant_entry_until_interrupted() {
        let mut t = Transcript::new();
        t.push_token("Hel");
        t.push_token("lo");
        assert_eq!(t.iter().count(), 1);
        assert_eq!(
            t.iter().next(),
            Some(&Entry::Assistant {
                id: 0,
                text: "Hello".to_string()
            })
        );
    }

    /// Regression: an append to the already-open entry must report that entry's id as
    /// `changed` (not just `evicted: None`), so the caller can invalidate its `WrapCache`
    /// entry every token — otherwise the rendered wrap freezes at whatever the first token
    /// produced.
    #[test]
    fn push_token_reports_the_open_entrys_id_as_changed_even_when_not_evicted() {
        let mut t = Transcript::new();
        let first = t.push_token("Hel");
        assert_eq!(first.changed, 0);
        assert_eq!(first.evicted, None);

        let second = t.push_token("lo");
        assert_eq!(second.changed, 0);
        assert_eq!(second.evicted, None);
    }

    #[test]
    fn a_tool_call_between_two_token_bursts_opens_a_second_assistant_entry() {
        let mut t = Transcript::new();
        t.push_token("before");
        t.push_tool_call("read_file".to_string());
        t.push_token("after");

        let texts: Vec<&Entry> = t.iter().collect();
        assert_eq!(texts.len(), 3);
        assert!(matches!(texts[0], Entry::Assistant { text, .. } if text == "before"));
        assert!(matches!(texts[1], Entry::ToolCall { ok: None, .. }));
        assert!(matches!(texts[2], Entry::Assistant { text, .. } if text == "after"));
    }

    #[test]
    fn resolve_tool_call_fills_in_the_nearest_pending_call_by_name() {
        let mut t = Transcript::new();
        t.push_tool_call("read_file".to_string());
        t.resolve_tool_call("read_file", true, "contents".to_string());

        let entry = t.iter().next().unwrap();
        assert!(matches!(
            entry,
            Entry::ToolCall {
                ok: Some(true),
                summary,
                ..
            } if summary == "contents"
        ));
    }

    #[test]
    fn end_turn_closes_the_open_assistant_entry() {
        let mut t = Transcript::new();
        t.push_token("hi");
        t.end_turn();
        t.push_token(" again");

        assert_eq!(t.iter().count(), 2);
    }

    #[test]
    fn a_new_user_prompt_closes_the_open_assistant_entry() {
        let mut t = Transcript::new();
        t.push_token("partial");
        t.push_user("next question".to_string());
        t.push_token("answer");

        assert_eq!(t.iter().count(), 3);
    }

    #[test]
    fn pushing_past_the_cap_evicts_the_oldest_entry_and_reports_its_id() {
        let mut t = Transcript::new();
        for i in 0..MAX_ENTRIES {
            assert_eq!(t.push_user(format!("msg {i}")), None);
        }
        let evicted = t.push_user("overflow".to_string());
        assert_eq!(evicted, Some(0));
        assert_eq!(t.iter().count(), MAX_ENTRIES);
    }

    #[test]
    fn push_system_closes_the_open_assistant_entry_like_push_error_does() {
        let mut t = Transcript::new();
        t.push_token("partial");
        t.push_system("unknown command: /xyz".to_string());
        t.push_token("more");

        let entries: Vec<&Entry> = t.iter().collect();
        assert_eq!(
            entries.len(),
            3,
            "a system entry must never merge into the streaming assistant text around it"
        );
        assert!(matches!(entries[1], Entry::System { text, .. } if text == "unknown command: /xyz"));
    }

    #[test]
    fn toggle_last_tool_expanded_flips_only_the_most_recent_tool_call() {
        let mut t = Transcript::new();
        t.push_tool_call("a".to_string());
        t.push_tool_call("b".to_string());

        t.toggle_last_tool_expanded();

        let calls: Vec<&Entry> = t
            .iter()
            .filter(|e| matches!(e, Entry::ToolCall { .. }))
            .collect();
        assert!(matches!(
            calls[0],
            Entry::ToolCall {
                expanded: false,
                ..
            }
        ));
        assert!(matches!(calls[1], Entry::ToolCall { expanded: true, .. }));
    }
}
