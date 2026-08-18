//! Context compaction (§9.5's "context growth is the cue to `/compact`", `M13-4`): once a
//! turn's reported prompt size crosses [`COMPACTION_THRESHOLD_PCT`] of a conservative context
//! window, [`maybe_compact`] replaces everything but the most recent exchanges with a single
//! model-authored summary, so the next turn's history stays well under the limit instead of
//! growing until a request fails outright.
//!
//! **Never drops the preamble.** The preamble lives on the built `Agent<M>` itself (set once at
//! construction via `AgentBuilder::preamble`), never in `crate::session::session_task`'s
//! `history: Vec<Message>` this module operates on — so there is no code path here that could
//! touch it either way.
//!
//! **Never drops the most recent exchange.** [`KEEP_RECENT_MESSAGES`] trailing messages are
//! excluded from summarization and appended to the compacted history verbatim — the mechanism
//! behind "never drop... the most recent tool results" once mapped onto message history (a
//! session's `history` only ever holds the prompt/final-text pair from each past turn, not the
//! individual tool calls inside it — see `crate::session`'s own module doc for why).
//!
//! Gated on the *last turn's actually-reported* `usage.input_tokens` rather than a
//! locally-computed token estimate — the provider already told us exactly how many prompt
//! tokens that history cost, so re-deriving an approximate count would be strictly worse
//! information for the same threshold check.

use rig::agent::Agent;
use rig::completion::{CompletionModel, GetTokenUsage, Message, Usage};
use tokio_util::sync::CancellationToken;

use crate::streaming;

/// A conservative, model-independent assumption about the context window — `mate` has no
/// per-model context-length table today (only `[pricing]`'s cost-per-token one), so this is one
/// fixed budget rather than a number that would silently be wrong for whichever model isn't in
/// a table that doesn't exist yet.
const CONTEXT_WINDOW_TOKENS: u64 = 128_000;

/// §13.4's "~70% of the window" trigger point.
const COMPACTION_THRESHOLD_PCT: u64 = 70;

/// Trailing messages (user+assistant pairs, so always kept even) excluded from summarization —
/// two whole exchanges survive a compaction untouched.
const KEEP_RECENT_MESSAGES: usize = 4;

const COMPACTION_INSTRUCTION: &str = "Summarize the conversation so far in a few sentences, \
    preserving important facts, decisions, file paths, and any unresolved questions. This \
    summary replaces the earlier turns in your own context, so do not omit anything a later \
    turn might still need.";

const COMPACTION_PLACEHOLDER: &str = "(earlier conversation summarized below)";

/// Whether the last turn's reported prompt size alone justifies compacting, independent of
/// whether there's anything left to compact — [`maybe_compact`] checks both.
fn should_compact(last_input_tokens: u64) -> bool {
    last_input_tokens.saturating_mul(100) >= CONTEXT_WINDOW_TOKENS * COMPACTION_THRESHOLD_PCT
}

/// Best-effort compaction (`M13-4`): summarizes everything in `history` except the trailing
/// [`KEEP_RECENT_MESSAGES`] messages, via one extra completion call against `agent` — the same
/// model and preamble the session already uses, driven through
/// [`streaming::stream_turn_with_history`] exactly like a real turn is, just with its events
/// discarded (this call is never shown in the transcript). Leaves `history` untouched, rather
/// than risking a corrupted conversation, whenever there's nothing worth compacting yet, the
/// summarization call is cancelled or errors, or it comes back empty.
pub async fn maybe_compact<M>(
    agent: &Agent<M>,
    history: &mut Vec<Message>,
    last_turn_usage: Usage,
    cancel: &CancellationToken,
) where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    if !should_compact(last_turn_usage.input_tokens) {
        return;
    }
    if history.len() <= KEEP_RECENT_MESSAGES {
        return;
    }

    let split = history.len() - KEEP_RECENT_MESSAGES;
    let (to_summarize, keep) = history.split_at(split);

    let outcome = streaming::stream_turn_with_history(
        agent,
        COMPACTION_INSTRUCTION,
        to_summarize,
        cancel,
        |_| {},
    )
    .await;

    if outcome.cancelled || outcome.error.is_some() || outcome.text.trim().is_empty() {
        return;
    }

    let mut compacted = vec![
        Message::user(COMPACTION_PLACEHOLDER.to_string()),
        Message::assistant(outcome.text),
    ];
    compacted.extend_from_slice(keep);
    *history = compacted;
}

#[cfg(test)]
mod tests {
    use super::*;

    use rig::agent::AgentBuilder;
    use rig::test_utils::{MockCompletionModel, MockStreamEvent};

    fn usage(input_tokens: u64) -> Usage {
        Usage {
            input_tokens,
            output_tokens: 0,
            total_tokens: input_tokens,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    fn long_history() -> Vec<Message> {
        (0..10)
            .flat_map(|i| {
                [
                    Message::user(format!("question {i}")),
                    Message::assistant(format!("answer {i}")),
                ]
            })
            .collect()
    }

    #[test]
    fn should_compact_triggers_at_the_seventy_percent_threshold() {
        assert!(!should_compact(CONTEXT_WINDOW_TOKENS * 69 / 100));
        assert!(should_compact(CONTEXT_WINDOW_TOKENS * 70 / 100));
    }

    #[tokio::test]
    async fn below_threshold_leaves_a_long_history_untouched() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("should never be called"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model.clone()).build();
        let mut history = long_history();
        let original_len = history.len();
        let cancel = CancellationToken::new();

        maybe_compact(&agent, &mut history, usage(1), &cancel).await;

        assert_eq!(
            history.len(),
            original_len,
            "a turn well under the threshold must never trigger compaction"
        );
        assert!(
            model.requests().is_empty(),
            "no summarization call should even be attempted below the threshold"
        );
    }

    #[tokio::test]
    async fn a_short_history_above_threshold_is_left_alone_since_theres_nothing_to_compact() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("should never be called"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model.clone()).build();
        let mut history = vec![
            Message::user("hi".to_string()),
            Message::assistant("hello".to_string()),
        ];
        let original_len = history.len();
        let cancel = CancellationToken::new();

        maybe_compact(&agent, &mut history, usage(CONTEXT_WINDOW_TOKENS), &cancel).await;

        assert_eq!(
            history.len(),
            original_len,
            "a history at or below KEEP_RECENT_MESSAGES has nothing left to summarize"
        );
        assert!(
            model.requests().is_empty(),
            "no summarization call should be attempted when there's nothing to compact"
        );
    }

    #[tokio::test]
    async fn above_threshold_summarizes_the_oldest_messages_and_keeps_a_verbatim_tail() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("condensed summary"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model.clone()).build();
        let mut history = long_history();
        let original_len = history.len();
        let cancel = CancellationToken::new();

        maybe_compact(&agent, &mut history, usage(CONTEXT_WINDOW_TOKENS), &cancel).await;

        assert_eq!(
            history.len(),
            2 + KEEP_RECENT_MESSAGES,
            "compaction must collapse the summarized portion to exactly one user/assistant pair \
             plus the untouched tail"
        );
        // The summarization call's own request history length proves exactly which prefix was
        // sent to be summarized: everything except the trailing KEEP_RECENT_MESSAGES, plus the
        // instruction prompt itself (`stream_turn_with_history` folds prompt + history into one
        // `chat_history`, per `crate::streaming`'s own tests).
        assert_eq!(
            model.requests()[0].chat_history.len(),
            original_len - KEEP_RECENT_MESSAGES + 1,
            "the summarization call must see only the messages being compacted away, not the \
             kept tail"
        );
    }

    #[tokio::test]
    async fn a_cancelled_summarization_leaves_history_untouched() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("should not matter"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model).build();
        let mut history = long_history();
        let original_len = history.len();
        let cancel = CancellationToken::new();
        cancel.cancel();

        maybe_compact(&agent, &mut history, usage(CONTEXT_WINDOW_TOKENS), &cancel).await;

        assert_eq!(
            history.len(),
            original_len,
            "a compaction call that never runs (already cancelled) must not corrupt history"
        );
    }
}
