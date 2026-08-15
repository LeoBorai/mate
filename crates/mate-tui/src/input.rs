//! The input box (`M7-5`): a `ratatui-textarea` buffer plus mate's own key routing — `Enter`
//! sends, `Alt+Enter` inserts a newline, and `Up`/`Down` recall prompt history when the box is
//! otherwise empty. Routing lives here rather than in the crate's own `input()` shortcuts
//! because `Enter` already has a fixed meaning there (insert a newline) that this app needs to
//! mean something else.
//!
//! Keys are translated into `ratatui-textarea`'s backend-agnostic `Input` field by field
//! (`translate`) rather than through the crate's own `crossterm` conversion feature, which
//! would pull a second copy of `crossterm` into the workspace at a version nothing else here
//! depends on.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::{Input, Key, TextArea};

const PLACEHOLDER: &str = "message the agent";

pub struct InputBox {
    area: TextArea<'static>,
    history: Vec<String>,
    /// Index into `history` while recalling; `None` means the box holds a fresh draft.
    history_index: Option<usize>,
    /// The draft that was in progress before `Up` started recalling history, restored once
    /// `Down` recalls past the newest entry.
    draft_before_history: Option<String>,
}

impl InputBox {
    pub fn new() -> Self {
        let mut area = TextArea::default();
        area.set_placeholder_text(PLACEHOLDER);
        Self {
            area,
            history: Vec::new(),
            history_index: None,
            draft_before_history: None,
        }
    }

    pub fn textarea(&self) -> &TextArea<'static> {
        &self.area
    }

    pub fn is_empty(&self) -> bool {
        self.area.lines().iter().all(|line| line.trim().is_empty())
    }

    /// Handles one key press against the box. Returns the submitted prompt on a plain `Enter`
    /// — the caller decides whether a turn can actually start right now (§5.2: one turn at a
    /// time) and sends it on.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<String> {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if alt => {
                self.area.insert_newline();
                None
            }
            KeyCode::Enter => {
                if self.is_empty() {
                    return None;
                }
                let text = self.area.lines().join("\n");
                self.history.push(text.clone());
                self.history_index = None;
                self.draft_before_history = None;
                self.area = TextArea::default();
                self.area.set_placeholder_text(PLACEHOLDER);
                Some(text)
            }
            KeyCode::Up if self.is_empty() || self.history_index.is_some() => {
                self.recall_older();
                None
            }
            KeyCode::Down if self.history_index.is_some() => {
                self.recall_newer();
                None
            }
            _ => {
                if let Some(input) = translate(key) {
                    self.area.input(input);
                }
                None
            }
        }
    }

    fn recall_older(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            None => {
                self.draft_before_history = Some(self.area.lines().join("\n"));
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(next);
        let text = self.history[next].clone();
        self.replace_text(&text);
    }

    fn recall_newer(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.history_index = Some(i + 1);
            let text = self.history[i + 1].clone();
            self.replace_text(&text);
        } else {
            self.history_index = None;
            let draft = self.draft_before_history.take().unwrap_or_default();
            self.replace_text(&draft);
        }
    }

    fn replace_text(&mut self, text: &str) {
        self.area = if text.is_empty() {
            TextArea::default()
        } else {
            TextArea::new(text.lines().map(str::to_string).collect())
        };
        self.area.set_placeholder_text(PLACEHOLDER);
    }
}

impl Default for InputBox {
    fn default() -> Self {
        Self::new()
    }
}

fn translate(key: KeyEvent) -> Option<Input> {
    let key_kind = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    Some(Input {
        key: key_kind,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn plain_enter_on_empty_box_submits_nothing() {
        let mut input = InputBox::new();
        assert_eq!(input.on_key(key(KeyCode::Enter, KeyModifiers::NONE)), None);
    }

    #[test]
    fn plain_enter_submits_and_clears_the_draft() {
        let mut input = InputBox::new();
        for c in "hi".chars() {
            input.on_key(char_key(c));
        }
        let submitted = input.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(submitted, Some("hi".to_string()));
        assert!(input.is_empty());
    }

    #[test]
    fn alt_enter_inserts_a_newline_instead_of_submitting() {
        let mut input = InputBox::new();
        input.on_key(char_key('a'));
        let submitted = input.on_key(key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(submitted, None);
        input.on_key(char_key('b'));
        assert_eq!(input.textarea().lines(), ["a", "b"]);
    }

    #[test]
    fn up_recalls_the_most_recent_history_entry_on_an_empty_box() {
        let mut input = InputBox::new();
        for c in "first".chars() {
            input.on_key(char_key(c));
        }
        input.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        for c in "second".chars() {
            input.on_key(char_key(c));
        }
        input.on_key(key(KeyCode::Enter, KeyModifiers::NONE));

        input.on_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(input.textarea().lines(), ["second"]);
        input.on_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(input.textarea().lines(), ["first"]);
    }

    #[test]
    fn down_past_the_newest_entry_restores_the_draft_in_progress() {
        let mut input = InputBox::new();
        for c in "sent".chars() {
            input.on_key(char_key(c));
        }
        input.on_key(key(KeyCode::Enter, KeyModifiers::NONE));

        // `Up` only starts recalling from an empty box, so the draft it saves here is the empty
        // box itself. Editing the recalled entry, then paging past the newest with `Down`,
        // restores that saved draft.
        input.on_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(input.textarea().lines(), ["sent"]);
        // Cursor lands at the start of the recalled text, so `!` inserts before it.
        input.on_key(char_key('!'));
        assert_eq!(input.textarea().lines(), ["!sent"]);
        input.on_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(input.is_empty());
    }

    #[test]
    fn up_does_not_recall_history_while_editing_a_nonempty_draft() {
        let mut input = InputBox::new();
        for c in "sent".chars() {
            input.on_key(char_key(c));
        }
        input.on_key(key(KeyCode::Enter, KeyModifiers::NONE));
        input.on_key(char_key('x'));

        input.on_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(input.textarea().lines(), ["x"]);
    }

    #[test]
    fn backspace_deletes_the_previous_character() {
        let mut input = InputBox::new();
        input.on_key(char_key('a'));
        input.on_key(char_key('b'));
        input.on_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.textarea().lines(), ["a"]);
    }

    #[test]
    fn key_release_events_still_translate_when_forwarded() {
        // `on_key` itself doesn't filter by `KeyEventKind` — that's the caller's job (`M7-2`).
        // Constructing one here just pins that `translate` doesn't panic on a non-default kind.
        let mut evt = char_key('z');
        evt.kind = KeyEventKind::Release;
        assert!(translate(evt).is_some());
    }
}
