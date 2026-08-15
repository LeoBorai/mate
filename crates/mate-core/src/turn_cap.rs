//! `TurnCapHook` (`M4-4`): when the model has exactly one model call left in its configured
//! budget, forces that call to answer in text instead of attempting one more tool call it has
//! no budget left to see the result of. Without this, a model that keeps calling tools right up
//! to the edge of its budget ends the run in `PromptError::MaxTurnsError` — a bare failure —
//! instead of whatever partial answer it could give with what it already found.
//!
//! This is advisory, not enforcement: `ToolChoice::None` is a request to the provider, and a
//! model (or a test double that ignores it) can still emit a tool call anyway. The actual
//! guarantee that a tool-looping model terminates at all — rather than looping forever — comes
//! from Rig's own `default_max_turns` budget, independent of this hook (see
//! `mate-core/tests/tool_round_trip.rs`'s turn-cap test). This hook only improves the *outcome*
//! for models that honor `tool_choice`, which is the common case for real providers.

use rig::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch};
use rig::completion::Document;
use rig::message::ToolChoice;

const BUDGET_NOTICE_ID: &str = "turn-budget-notice";
const BUDGET_NOTICE_TEXT: &str = "You have used your last available turn. Do not call any more \
    tools. Answer now, in full, using only what you have already found.";

pub struct TurnCapHook {
    pub max_turns: usize,
}

impl AgentHook for TurnCapHook {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if self.max_turns == 0 || event.turn != self.max_turns {
            return CompletionCallAction::continue_run();
        }

        CompletionCallAction::patch(RequestPatch::new().tool_choice(ToolChoice::None).context(
            Document {
                id: BUDGET_NOTICE_ID.to_string(),
                text: BUDGET_NOTICE_TEXT.to_string(),
                additional_props: Default::default(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rig::agent::AgentBuilder;
    use rig::test_utils::{MockAddTool, MockCompletionModel, MockStreamEvent};
    use tokio_util::sync::CancellationToken;

    use crate::streaming;

    #[tokio::test]
    async fn forces_a_tool_free_final_turn_and_notes_the_budget() {
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("call_1", "add", serde_json::json!({"x": 1, "y": 2})),
                MockStreamEvent::final_response_with_total_tokens(5),
            ],
            vec![
                MockStreamEvent::text("3"),
                MockStreamEvent::final_response_with_total_tokens(5),
            ],
        ]);

        let agent = AgentBuilder::new(model.clone())
            .tool(MockAddTool)
            .default_max_turns(2)
            .add_hook(TurnCapHook { max_turns: 2 })
            .build();

        let cancel = CancellationToken::new();
        let outcome = streaming::stream_turn(&agent, "add 1 and 2", &cancel, |_| {}).await;
        assert!(outcome.error.is_none());

        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].tool_choice, None);
        assert_eq!(requests[1].tool_choice, Some(ToolChoice::None));
        assert!(
            requests[1]
                .documents
                .iter()
                .any(|doc| doc.id == BUDGET_NOTICE_ID)
        );
    }

    #[tokio::test]
    async fn does_not_patch_turns_before_the_budget_is_exhausted() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("well within budget"),
            MockStreamEvent::final_response_with_total_tokens(5),
        ]]);

        let agent = AgentBuilder::new(model.clone())
            .tool(MockAddTool)
            .default_max_turns(5)
            .add_hook(TurnCapHook { max_turns: 5 })
            .build();

        let cancel = CancellationToken::new();
        streaming::stream_turn(&agent, "hi", &cancel, |_| {}).await;

        let requests = model.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tool_choice, None);
    }
}
