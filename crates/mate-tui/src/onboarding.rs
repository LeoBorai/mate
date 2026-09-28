//! First-run onboarding: when `mate-cli` has no `API_TOKEN` to start sessions with, the TUI
//! renders anyway and walks the user through three steps — pick a backend, pick a model from
//! [`mate_core::model_catalog`] filtered to that backend, then enter a token — before handing
//! that choice back to `mate-cli` (via [`PendingOnboarding::complete`]) to verify the token and
//! spawn the sessions.
//!
//! The steps are strictly ordered: the model list depends on the backend, and a token is only
//! asked for once both are chosen. `Esc` steps backward; on the first step it quits.
//!
//! The token is a plain in-memory `String`, held only by [`Onboarding`] and moved into the
//! completion closure. It never reaches `Config`, `figment`, a file, or a log line: [`Onboarding`]
//! has a hand-written `Debug` that redacts it, and derives nothing serializable.

use std::collections::HashMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::future::LocalBoxFuture;
use mate_core::cost::ModelRate;
use mate_core::model_catalog::{self, CatalogBackend, ModelEntry};
use mate_core::session::{SessionEvent, SessionManager};
use tokio::sync::mpsc;

use crate::app::InitialSession;
use crate::session_factory::SessionDefaults;

/// Everything `mate_tui::run` needs to open the tabbed view, produced once onboarding's
/// completion closure has verified the token and spawned the sessions. The same five values
/// `run` takes directly on the token-already-set path.
pub struct StartedSessions {
    pub manager: SessionManager,
    pub events: mpsc::Receiver<SessionEvent>,
    pub sessions: Vec<InitialSession>,
    pub defaults: SessionDefaults,
    pub pricing: HashMap<String, ModelRate>,
}

/// `mate-cli`'s half of onboarding: given the chosen backend, model id and token, verify the
/// token and spawn the sessions — or return a message to show inline on the token step. Called
/// once per attempt, so a failed attempt can be retried; the closure must capture owned state.
pub type CompleteOnboarding = Box<
    dyn Fn(
        CatalogBackend,
        String,
        String,
    ) -> LocalBoxFuture<'static, Result<StartedSessions, String>>,
>;

/// What `mate_tui::run_with_onboarding` needs: the closure that turns a finished onboarding into
/// running sessions. The catalog itself is a static in `mate-core`, and the workspace roots and
/// config template live inside the closure.
pub struct PendingOnboarding {
    pub complete: CompleteOnboarding,
}

/// Which of the three steps has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Backend,
    Model,
    Token,
}

/// What a key press asks the caller to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Stay in onboarding; redraw.
    Continue,
    /// The user backed out of the first step, or pressed `Ctrl+C`.
    Cancel,
    /// The token step was confirmed with a non-empty token.
    Submit,
}

/// A finished onboarding's choices, ready to hand to the completion closure.
pub(crate) struct Submission {
    pub(crate) backend: CatalogBackend,
    pub(crate) model: &'static ModelEntry,
    pub(crate) token: String,
}

/// The onboarding modal's state (`Backend` → `Model` → `Token`).
pub(crate) struct Onboarding {
    pub(crate) step: Step,
    pub(crate) backend_sel: usize,
    pub(crate) model_sel: usize,
    token: String,
    pub(crate) error: Option<String>,
    /// Set from submitting until the completion closure answers — the UI shows a "verifying"
    /// line and ignores keys other than `Ctrl+C`.
    pub(crate) verifying: bool,
}

impl fmt::Debug for Onboarding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Onboarding")
            .field("step", &self.step)
            .field("backend_sel", &self.backend_sel)
            .field("model_sel", &self.model_sel)
            .field(
                "token",
                &format_args!("<redacted, {} chars>", self.token.chars().count()),
            )
            .field("error", &self.error)
            .field("verifying", &self.verifying)
            .finish()
    }
}

impl Onboarding {
    pub(crate) fn new() -> Self {
        Self {
            step: Step::Backend,
            backend_sel: 0,
            model_sel: 0,
            token: String::new(),
            error: None,
            verifying: false,
        }
    }

    pub(crate) fn backend(&self) -> CatalogBackend {
        CatalogBackend::ALL[self.backend_sel]
    }

    /// The model list the second step shows: the catalog filtered to the chosen backend.
    pub(crate) fn model_rows(&self) -> Vec<&'static ModelEntry> {
        model_catalog::models_for(self.backend()).collect()
    }

    pub(crate) fn selected_model(&self) -> Option<&'static ModelEntry> {
        self.model_rows().get(self.model_sel).copied()
    }

    /// How many characters the token holds, for the masked display — the token itself is never
    /// exposed outside this module.
    pub(crate) fn token_len(&self) -> usize {
        self.token.chars().count()
    }

    /// Applies one key press. Everything is ignored while `verifying`, except `Ctrl+C`.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            return Outcome::Cancel;
        }
        if self.verifying {
            return Outcome::Continue;
        }
        match self.step {
            Step::Backend => self.on_backend_key(key),
            Step::Model => self.on_model_key(key),
            Step::Token => self.on_token_key(key),
        }
    }

    fn on_backend_key(&mut self, key: KeyEvent) -> Outcome {
        let n = CatalogBackend::ALL.len();
        match key.code {
            KeyCode::Up => {
                self.backend_sel = (self.backend_sel + n - 1) % n;
                self.model_sel = 0;
            }
            KeyCode::Down => {
                self.backend_sel = (self.backend_sel + 1) % n;
                self.model_sel = 0;
            }
            KeyCode::Enter => self.step = Step::Model,
            KeyCode::Esc => return Outcome::Cancel,
            _ => {}
        }
        Outcome::Continue
    }

    fn on_model_key(&mut self, key: KeyEvent) -> Outcome {
        let n = self.model_rows().len();
        match key.code {
            KeyCode::Up if n > 0 => self.model_sel = (self.model_sel + n - 1) % n,
            KeyCode::Down if n > 0 => self.model_sel = (self.model_sel + 1) % n,
            KeyCode::Enter if n > 0 => self.step = Step::Token,
            KeyCode::Esc => self.step = Step::Backend,
            _ => {}
        }
        Outcome::Continue
    }

    fn on_token_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => {
                self.error = None;
                self.step = Step::Model;
            }
            KeyCode::Enter => {
                if self.token.trim().is_empty() {
                    self.error = Some("enter a token to continue".to_string());
                } else {
                    self.error = None;
                    return Outcome::Submit;
                }
            }
            KeyCode::Backspace => {
                self.token.pop();
            }
            KeyCode::Char('u' | 'U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.token.clear();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.token.push(c);
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Marks the attempt as in flight and returns the choices to hand to the completion
    /// closure. `None` if no model is selectable (not reachable through [`Self::on_key`], which
    /// never advances past an empty model list). The token is cloned so a failed attempt can be
    /// corrected and retried.
    pub(crate) fn begin_submit(&mut self) -> Option<Submission> {
        let model = self.selected_model()?;
        self.verifying = true;
        self.error = None;
        Some(Submission {
            backend: self.backend(),
            model,
            token: self.token.clone(),
        })
    }

    /// Records the completion closure's answer. On success, returns the started sessions with
    /// the chosen model's catalog pricing filled in wherever `pricing` has no entry of its own
    /// (a `[pricing]` config entry always wins). On failure, shows the message inline and puts
    /// focus back on the token step with the chosen backend, model and token untouched.
    pub(crate) fn finish(
        &mut self,
        result: Result<StartedSessions, String>,
    ) -> Option<StartedSessions> {
        self.verifying = false;
        match result {
            Ok(mut started) => {
                if let Some(model) = self.selected_model()
                    && let Some(rate) = model.pricing
                {
                    started.pricing.entry(model.id.to_string()).or_insert(rate);
                }
                Some(started)
            }
            Err(message) => {
                self.error = Some(message);
                self.step = Step::Token;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_token(onboarding: &mut Onboarding, text: &str) {
        for c in text.chars() {
            onboarding.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_fresh_onboarding_starts_on_the_backend_step() {
        let onboarding = Onboarding::new();

        assert_eq!(
            onboarding.step,
            Step::Backend,
            "the backend is always asked first"
        );
        assert_eq!(
            onboarding.token_len(),
            0,
            "no token is held before the token step"
        );
    }

    #[test]
    fn all_three_steps_complete_in_order_with_a_backend_model_and_token() {
        let mut onboarding = Onboarding::new();

        assert_eq!(
            onboarding.on_key(key(KeyCode::Down)),
            Outcome::Continue,
            "pick Gemini"
        );
        assert_eq!(
            onboarding.backend(),
            CatalogBackend::Gemini,
            "Down moves to the second backend"
        );
        assert_eq!(
            onboarding.on_key(key(KeyCode::Enter)),
            Outcome::Continue,
            "confirm backend"
        );
        assert_eq!(
            onboarding.step,
            Step::Model,
            "backend Enter advances to the model step"
        );

        assert_eq!(
            onboarding.on_key(key(KeyCode::Down)),
            Outcome::Continue,
            "move in the list"
        );
        assert_eq!(
            onboarding.on_key(key(KeyCode::Enter)),
            Outcome::Continue,
            "confirm model"
        );
        assert_eq!(
            onboarding.step,
            Step::Token,
            "model Enter advances to the token step"
        );

        type_token(&mut onboarding, "sekret");
        assert_eq!(
            onboarding.on_key(key(KeyCode::Enter)),
            Outcome::Submit,
            "token Enter submits"
        );

        let submission = onboarding.begin_submit().expect("a model is selected");
        assert_eq!(
            submission.backend,
            CatalogBackend::Gemini,
            "the chosen backend is carried"
        );
        assert_eq!(
            submission.model.id,
            model_catalog::models_for(CatalogBackend::Gemini)
                .nth(1)
                .expect("the catalog lists at least two Gemini models")
                .id,
            "the model is the second Gemini entry, as navigated"
        );
        assert_eq!(
            submission.token, "sekret",
            "the typed token is carried through"
        );
        assert!(
            onboarding.verifying,
            "submitting marks the attempt in flight"
        );
    }

    #[test]
    fn the_model_list_follows_the_chosen_backend() {
        let mut onboarding = Onboarding::new();
        let hf_rows = onboarding.model_rows();

        onboarding.on_key(key(KeyCode::Down));
        let gemini_rows = onboarding.model_rows();

        assert!(
            hf_rows
                .iter()
                .all(|m| m.backend == CatalogBackend::Huggingface),
            "the first backend lists only Hugging Face models"
        );
        assert!(
            gemini_rows
                .iter()
                .all(|m| m.backend == CatalogBackend::Gemini),
            "after moving the backend selection the list is Gemini-only"
        );
        assert_ne!(
            hf_rows.iter().map(|m| m.id).collect::<Vec<_>>(),
            gemini_rows.iter().map(|m| m.id).collect::<Vec<_>>(),
            "the rendered model list changes when the backend selection changes"
        );
    }

    #[test]
    fn changing_the_backend_resets_the_model_highlight() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Down));
        onboarding.on_key(key(KeyCode::Down));
        assert_eq!(
            onboarding.model_sel, 2,
            "two Downs move the model highlight"
        );

        onboarding.on_key(key(KeyCode::Esc));
        onboarding.on_key(key(KeyCode::Down));

        assert_eq!(
            onboarding.model_sel, 0,
            "a new backend's list starts at its first model"
        );
    }

    #[test]
    fn typed_characters_are_ignored_on_the_list_steps() {
        let mut onboarding = Onboarding::new();
        type_token(&mut onboarding, "abc");
        onboarding.on_key(key(KeyCode::Enter));
        type_token(&mut onboarding, "def");
        onboarding.on_key(key(KeyCode::Enter));

        assert_eq!(
            onboarding.step,
            Step::Token,
            "two Enters reach the token step"
        );
        assert_eq!(
            onboarding.token_len(),
            0,
            "text typed before the token step is never captured"
        );
    }

    #[test]
    fn a_token_is_never_asked_for_before_backend_and_model_are_chosen() {
        let mut onboarding = Onboarding::new();

        assert_eq!(
            onboarding.on_key(key(KeyCode::Enter)),
            Outcome::Continue,
            "backend Enter"
        );
        assert_eq!(
            onboarding.step,
            Step::Model,
            "still not at the token step after one Enter"
        );
        assert_ne!(
            onboarding.step,
            Step::Token,
            "the model must be chosen first"
        );
    }

    #[test]
    fn esc_steps_backward_and_quits_from_the_first_step() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Enter));
        assert_eq!(onboarding.step, Step::Token, "setup: on the token step");

        onboarding.on_key(key(KeyCode::Esc));
        assert_eq!(
            onboarding.step,
            Step::Model,
            "Esc on the token step returns to the model step"
        );
        onboarding.on_key(key(KeyCode::Esc));
        assert_eq!(
            onboarding.step,
            Step::Backend,
            "Esc on the model step returns to the backend step"
        );
        assert_eq!(
            onboarding.on_key(key(KeyCode::Esc)),
            Outcome::Cancel,
            "Esc on step one quits"
        );
    }

    #[test]
    fn ctrl_c_cancels_from_any_step_even_while_verifying() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut onboarding = Onboarding::new();
        onboarding.verifying = true;

        assert_eq!(
            onboarding.on_key(ctrl_c),
            Outcome::Cancel,
            "Ctrl+C always cancels"
        );
    }

    #[test]
    fn an_empty_token_is_rejected_inline_without_submitting() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Enter));
        type_token(&mut onboarding, "   ");

        let outcome = onboarding.on_key(key(KeyCode::Enter));

        assert_eq!(
            outcome,
            Outcome::Continue,
            "a whitespace-only token does not submit"
        );
        assert!(
            onboarding.error.is_some(),
            "the user is told why nothing happened"
        );
    }

    #[test]
    fn backspace_and_ctrl_u_edit_the_token() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Enter));
        type_token(&mut onboarding, "abcd");

        onboarding.on_key(key(KeyCode::Backspace));
        assert_eq!(onboarding.token_len(), 3, "Backspace removes one character");
        onboarding.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(onboarding.token_len(), 0, "Ctrl+U clears the whole token");
    }

    #[test]
    fn keys_other_than_ctrl_c_are_ignored_while_verifying() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Enter));
        type_token(&mut onboarding, "abc");
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.begin_submit();

        onboarding.on_key(key(KeyCode::Backspace));
        onboarding.on_key(key(KeyCode::Esc));

        assert_eq!(
            onboarding.token_len(),
            3,
            "the token can't be edited mid-verification"
        );
        assert_eq!(
            onboarding.step,
            Step::Token,
            "and Esc can't navigate away mid-verification"
        );
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let mut onboarding = Onboarding::new();
        onboarding.on_key(key(KeyCode::Enter));
        onboarding.on_key(key(KeyCode::Enter));
        type_token(&mut onboarding, "hf_super_secret_value");

        let rendered = format!("{onboarding:?}");

        assert!(
            !rendered.contains("hf_super_secret_value"),
            "a stray `{{:?}}` in a log line must not leak the token: {rendered}"
        );
        assert!(
            rendered.contains("redacted"),
            "the redaction is visible: {rendered}"
        );
    }
}
