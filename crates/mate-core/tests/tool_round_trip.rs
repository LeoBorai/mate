//! Exercises `build_toolset` and the multi-turn wiring `build_agent` attaches (`M4-2`, `M4-3`,
//! `M4-4`) end to end, against Rig's scripted `MockCompletionModel` (`M4-5`'s reusable mock
//! harness — this file plus `tests/support/mod.rs` is the scaffolding future milestones' tests
//! reuse): a real `read_file` tool call round-trips to a grounded answer, a bad path recovers
//! because `ToolFailure` reaches the model as a tool result instead of aborting the turn, and a
//! model that keeps calling tools terminates at the configured turn cap instead of looping
//! forever.
//!
//! Driven through `mate_core::toolset::build_toolset` + `rig::agent::AgentBuilder` directly
//! rather than through `build_agent`/`Backend`: `Backend` only ever wraps a real provider
//! client, so there's no seam to hand it a `MockCompletionModel`. `build_agent`'s own wiring
//! (attaching this same toolset, setting `default_max_turns`) is exercised separately in
//! `tests/openai_fallback.rs` against a scripted HTTP stub.

mod support;

use mate_core::streaming::{self, AgentEvent};
use mate_core::toolset::build_toolset;
use rig::agent::AgentBuilder;
use rig::test_utils::{MockCompletionModel, MockStreamEvent};
use tokio_util::sync::CancellationToken;

fn tool_call_turn(id: &str, args: serde_json::Value) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::tool_call(id, "read_file", args),
        MockStreamEvent::final_response_with_total_tokens(5),
    ]
}

fn text_turn(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::final_response_with_total_tokens(5),
    ]
}

#[tokio::test]
async fn a_read_file_tool_call_round_trips_to_a_grounded_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree").unwrap();

    let model = MockCompletionModel::from_stream_turns([
        tool_call_turn("call_1", serde_json::json!({"path": "a.txt"})),
        text_turn("the file contains one, two, three"),
    ]);

    let agent = AgentBuilder::new(model)
        .tool_server_handle(build_toolset(support::tool_ctx(root)))
        .default_max_turns(4)
        .build();

    let cancel = CancellationToken::new();
    let mut events = Vec::new();
    let outcome = streaming::stream_turn(&agent, "what's in a.txt?", &cancel, |envelope| {
        events.push(envelope.event)
    })
    .await;

    assert!(outcome.error.is_none());
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolResult { name, ok: true, .. } if name == "read_file"
    )));
    assert_eq!(outcome.text, "the file contains one, two, three");
}

#[tokio::test]
async fn a_tool_failure_reaches_the_model_as_a_tool_result_and_the_model_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("a.txt"), "hi").unwrap();

    let model = MockCompletionModel::from_stream_turns([
        tool_call_turn("call_1", serde_json::json!({"path": "missing.txt"})),
        tool_call_turn("call_2", serde_json::json!({"path": "a.txt"})),
        text_turn("found it: hi"),
    ]);

    let agent = AgentBuilder::new(model)
        .tool_server_handle(build_toolset(support::tool_ctx(root)))
        .default_max_turns(4)
        .build();

    let cancel = CancellationToken::new();
    let mut events = Vec::new();
    let outcome = streaming::stream_turn(&agent, "read a.txt", &cancel, |envelope| {
        events.push(envelope.event)
    })
    .await;

    assert!(outcome.error.is_none());
    let summaries: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(summaries, vec!["not found: missing.txt", "     1\thi"]);
    assert_eq!(outcome.text, "found it: hi");
}

#[tokio::test]
async fn a_model_that_always_calls_tools_terminates_at_the_turn_cap_instead_of_looping() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::write(root.join("a.txt"), "hi").unwrap();

    // Scripted with more tool-calling turns than the budget allows — if the cap didn't hold,
    // `drive` would keep consuming turns until the mock ran out of scripted responses (or, for
    // a real provider, forever).
    let model = MockCompletionModel::from_stream_turns([
        tool_call_turn("call_1", serde_json::json!({"path": "a.txt"})),
        tool_call_turn("call_2", serde_json::json!({"path": "a.txt"})),
        tool_call_turn("call_3", serde_json::json!({"path": "a.txt"})),
        tool_call_turn("call_4", serde_json::json!({"path": "a.txt"})),
        tool_call_turn("call_5", serde_json::json!({"path": "a.txt"})),
    ]);

    let agent = AgentBuilder::new(model)
        .tool_server_handle(build_toolset(support::tool_ctx(root)))
        .default_max_turns(3)
        .build();

    let cancel = CancellationToken::new();
    let mut tool_calls = 0;
    let outcome = streaming::stream_turn(&agent, "read a.txt", &cancel, |envelope| {
        if matches!(envelope.event, AgentEvent::ToolCallStarted { .. }) {
            tool_calls += 1;
        }
    })
    .await;

    assert_eq!(
        tool_calls, 3,
        "a budget of 3 turns should allow exactly 3 tool calls, not more"
    );
    assert!(
        outcome.error.is_some(),
        "exhausting the budget while the model still wants to call tools should surface as an \
         error, not hang or silently stop short"
    );
}
