//! Rig's completion stream mapped into `mate`'s [`AgentEvent`] model (§4.1, §5.2, `M2`).
//!
//! `M2-1` drives `agent.stream_prompt(...)`'s `MultiTurnStreamItem` stream and translates it,
//! item by item, into the events the transcript and (eventually) the side panel consume.
//! `M2-2` buffers a tool call's streamed name/argument fragments so a call is only ever
//! surfaced once complete. `M2-4` folds a [`CancellationToken`] into the same poll loop so
//! cancelling mid-turn stops cleanly rather than needing a second code path. `M2-5` captures
//! the run's aggregated [`Usage`] off the stream's `FinalResponse` item.
//!
//! [`AgentId`] and [`AgentEventEnvelope`] land now rather than at `M6` (§11 note 1): adding a
//! routing key to an event type after the fact means touching every match arm downstream, so
//! it's in from the first event this crate ever emits. The envelope here stays deliberately
//! just `{ agent, event }` — `crate::session::SessionEvent` (`M6`) wraps it with `session:
//! SessionId` on top rather than this module inventing a new shape of its own.
//!
//! `stream_turn_with_history` (`M6-2`) threads a session's conversation history into the
//! request, for `crate::session`'s session task. `stream_turn` stays as it was for `M5`'s
//! plain frontend — see its own doc comment for why the two aren't unified into one.
//!
//! `AgentId` and `SubagentOutcome` are defined in `mate-tool-api`, not here (`M3`) — `M3`'s
//! `ToolCtx` and `ToolActivity` need to carry an `AgentId` too, and `mate-tool-api` can never
//! depend on `mate-core` (§8.1 note 1), so the lower crate owns the type and this crate imports
//! it rather than keeping a second definition in sync.

use std::collections::HashMap;
use std::path::PathBuf;

use futures::{Stream, StreamExt};
use mate_tool_api::{AgentId, SubagentOutcome, ToolActivity};
use rig::OneOrMany;
use rig::agent::{Agent, MultiTurnStreamItem, StreamingError};
use rig::completion::{CompletionModel, GetTokenUsage, Message, Usage};
use rig::message::ToolResultContent;
use rig::streaming::{
    StreamedAssistantContent, StreamedUserContent, StreamingChat, StreamingPrompt,
    ToolCallDeltaContent,
};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

/// One event produced by a running agent's turn (§5.2). The transcript renders `Token`,
/// `ToolCallStarted`, `ToolResult`, `TurnComplete`, and `Error` for the root agent; the rest
/// (approvals, subagent lifecycle) is side-panel material once those milestones land.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Token(String),
    ToolCallStarted {
        name: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        summary: String,
    },
    ApprovalRequired {
        id: Ulid,
        name: String,
        detail: String,
        /// The resolved, in-jail target of the action being approved, when there is one
        /// (`M13-5`'s "always allow writes under this directory" reads this to compute the
        /// directory it offers to remember). `None` for a request with no single filesystem
        /// target.
        path: Option<PathBuf>,
    },
    SubagentSpawned {
        id: AgentId,
        label: String,
        task: String,
    },
    SubagentFinished {
        id: AgentId,
        outcome: SubagentOutcome,
    },
    Usage(Usage),
    /// One tool's typed telemetry record (§9.3, `M11-4`), folded onto this stream from
    /// `ToolCtx::activity` by `crate::session::SessionManager::spawn` rather than produced by
    /// `drive`/`map_item` itself — a tool call and its activity record don't arrive from the
    /// same place `MultiTurnStreamItem` does.
    Activity(ToolActivity),
    TurnComplete,
    Error(String),
}

/// Stand-in for §5.2's `SessionEvent` until `SessionId` exists (`M6`, §11 note 1): every event
/// this crate emits already carries the agent that produced it, hardcoded to [`AgentId::ROOT`]
/// until subagents exist (`M9`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEventEnvelope {
    pub agent: AgentId,
    pub event: AgentEvent,
}

/// Accumulates one tool call's streamed name/argument fragments (`M2-2`). Providers vary in
/// how finely they chunk a call — some send `Name`/`Delta` fragments before the complete call,
/// others send the complete call directly — so fragments are buffered per `internal_call_id`
/// and never surfaced as an event on their own; only the complete call (§4.1: "buffer until
/// complete before rendering") produces an [`AgentEvent::ToolCallStarted`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ToolCallBuffer {
    name: String,
    arguments: String,
}

impl ToolCallBuffer {
    fn push(&mut self, content: &ToolCallDeltaContent) {
        match content {
            ToolCallDeltaContent::Name(fragment) => self.name.push_str(fragment),
            ToolCallDeltaContent::Delta(fragment) => self.arguments.push_str(fragment),
        }
    }
}

/// What a driven turn leaves behind once its stream ends, however it ended (`M2-4`, `M2-5`).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    /// Assistant text accumulated from `Token` events, in arrival order. Populated even when
    /// `cancelled` or `error` is set — a cancelled turn keeps whatever text streamed before the
    /// cancellation (`M2-4`'s "cancelling mid-stream leaves a coherent history").
    pub text: String,
    /// Aggregated usage for the turn, captured off the stream's `FinalResponse` item
    /// (`M2-5`). The zero sentinel (see [`Usage::new`]) if the turn never reached one
    /// (cancelled, or errored first).
    pub usage: Usage,
    /// Set when the `CancellationToken` fired before the stream finished on its own.
    pub cancelled: bool,
    /// Set when the stream yielded a `StreamingError` before finishing. Mutually exclusive
    /// with `cancelled` — cancellation is checked first each poll (see `drive`'s `biased`
    /// select), so a cancelled turn never also reports an error.
    pub error: Option<String>,
}

impl TurnOutcome {
    fn new() -> Self {
        Self {
            text: String::new(),
            usage: Usage::new(),
            cancelled: false,
            error: None,
        }
    }
}

/// Sparkline window for §9.5's context widget (`M11-5`): older per-turn samples are dropped
/// once a session outgrows it, so a long-running tab's sparkline data doesn't grow without
/// bound — [`UsageRollup::turns`] is the uncapped counter `per_turn_avg` needs instead.
const PER_TURN_HISTORY: usize = 32;

/// Session-level usage accumulation (§5.1, §9.11): `root` and `per_turn` are populated as root
/// turns complete; `subagents` folds in every `AgentEvent::Usage` tagged with a non-root
/// `AgentId` — one per completion call inside a subagent's own turn, the same granularity a
/// root turn reports at (`M9-2`, `M11-5`), just without `per_turn`/`turns` bookkeeping of its
/// own (the sparkline, §9.5, is root-only; a subagent's own turn count is on its
/// `SubagentReport` instead).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRollup {
    pub root: Usage,
    pub subagents: Usage,
    /// Sent (prompt) tokens for the last [`PER_TURN_HISTORY`] completed root turns, oldest
    /// first — the sparkline data for §9.5's context widget. Bounded here, not just by the
    /// widget that reads it, so nothing downstream has to re-truncate it itself.
    pub per_turn: Vec<u64>,
    /// Root turns completed this session, uncapped — the denominator `per_turn_avg` (§9.5)
    /// needs, since `per_turn.len()` stops growing once [`PER_TURN_HISTORY`] is reached.
    pub turns: usize,
}

impl UsageRollup {
    /// Folds one completed root turn's usage into the rollup.
    pub fn record_root_turn(&mut self, usage: Usage) {
        self.root.input_tokens += usage.input_tokens;
        self.root.output_tokens += usage.output_tokens;
        self.root.total_tokens += usage.total_tokens;
        self.root.cached_input_tokens += usage.cached_input_tokens;
        self.root.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.root.tool_use_prompt_tokens += usage.tool_use_prompt_tokens;
        self.root.reasoning_tokens += usage.reasoning_tokens;
        self.turns += 1;
        self.per_turn.push(usage.input_tokens);
        if self.per_turn.len() > PER_TURN_HISTORY {
            self.per_turn.remove(0);
        }
    }

    /// Folds one subagent completion call's usage into the rollup (`M11-5`) — called once per
    /// `AgentEvent::Usage` tagged with a non-root `AgentId`, the same cadence
    /// [`Self::record_root_turn`] is called at for the root agent. No `per_turn`/`turns`
    /// bookkeeping here: the sparkline (§9.5) is root-only.
    pub fn record_subagent_turn(&mut self, usage: Usage) {
        self.subagents.input_tokens += usage.input_tokens;
        self.subagents.output_tokens += usage.output_tokens;
        self.subagents.total_tokens += usage.total_tokens;
        self.subagents.cached_input_tokens += usage.cached_input_tokens;
        self.subagents.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.subagents.tool_use_prompt_tokens += usage.tool_use_prompt_tokens;
        self.subagents.reasoning_tokens += usage.reasoning_tokens;
    }
}

/// Runs one streamed prompt against `agent` end to end (`M2-1`): drives `stream_prompt`'s
/// `MultiTurnStreamItem` stream, maps each item to zero or one [`AgentEvent`] via `on_event`,
/// and returns once the stream ends, is cancelled, or errors.
///
/// Deliberately *not* [`stream_turn_with_history`] called with an empty slice: Rig's
/// `stream_chat`/`.history(...)` sets the request's chat history to `Some(vec![])`, which its
/// own docs say bypasses the agent's conversation memory — a different thing from
/// `stream_prompt`'s `None`, which leaves that memory in play. `M5`'s plain frontend already
/// depends on this exact path; changing it out from under it here to save a few lines isn't
/// this milestone's call to make.
pub async fn stream_turn<M>(
    agent: &Agent<M>,
    prompt: impl Into<Message> + Send,
    cancel: &CancellationToken,
    on_event: impl FnMut(AgentEventEnvelope),
) -> TurnOutcome
where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    let mut stream = agent.stream_prompt(prompt).await;
    drive(&mut stream, cancel, on_event).await
}

/// Runs one streamed prompt with prior conversation `history` threaded in (`M6-2`: the
/// session task owns history across turns, unlike `M5`'s one-shot plain frontend). Otherwise
/// identical to [`stream_turn`] — same event mapping, same cancellation semantics.
pub async fn stream_turn_with_history<M>(
    agent: &Agent<M>,
    prompt: impl Into<Message> + Send,
    history: &[Message],
    cancel: &CancellationToken,
    on_event: impl FnMut(AgentEventEnvelope),
) -> TurnOutcome
where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    let mut stream = agent.stream_chat(prompt, history.to_vec()).await;
    drive(&mut stream, cancel, on_event).await
}

/// The medium-independent poll loop (`M2-1`/`M2-4`): generic over the provider's raw response
/// type `R` purely because `MultiTurnStreamItem<R>` is, and over `S` so it can be driven
/// against a real Rig stream or, in tests, a synthetic one built with `futures::stream::iter`
/// or an `mpsc` channel.
async fn drive<S, R>(
    stream: &mut S,
    cancel: &CancellationToken,
    mut on_event: impl FnMut(AgentEventEnvelope),
) -> TurnOutcome
where
    S: Stream<Item = Result<MultiTurnStreamItem<R>, StreamingError>> + Unpin,
{
    let mut outcome = TurnOutcome::new();
    let mut tool_call_buffers: HashMap<String, ToolCallBuffer> = HashMap::new();
    let mut tool_call_names: HashMap<String, String> = HashMap::new();

    loop {
        // `biased`: cancellation always wins a simultaneous ready-item race, so a turn that's
        // cancelled the instant it also has a buffered item ready still stops rather than
        // processing one more event first.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                outcome.cancelled = true;
                break;
            }
            next = stream.next() => {
                match next {
                    None => break,
                    Some(Err(err)) => {
                        let message = err.to_string();
                        on_event(AgentEventEnvelope {
                            agent: AgentId::ROOT,
                            event: AgentEvent::Error(message.clone()),
                        });
                        outcome.error = Some(message);
                        break;
                    }
                    Some(Ok(item)) => {
                        if let Some(event) = map_item(
                            item,
                            &mut tool_call_buffers,
                            &mut tool_call_names,
                            &mut outcome,
                        ) {
                            on_event(AgentEventEnvelope { agent: AgentId::ROOT, event });
                        }
                    }
                }
            }
        }
    }

    outcome
}

/// Maps one `MultiTurnStreamItem` to at most one [`AgentEvent`] (`M2-3`). `MultiTurnStreamItem`
/// is `#[non_exhaustive]` (rig may add new medium-level lifecycle items in a future minor
/// release), so the trailing wildcard is required by the compiler, not a deliberate omission —
/// every variant that exists today is matched explicitly above it.
fn map_item<R>(
    item: MultiTurnStreamItem<R>,
    tool_call_buffers: &mut HashMap<String, ToolCallBuffer>,
    tool_call_names: &mut HashMap<String, String>,
    outcome: &mut TurnOutcome,
) -> Option<AgentEvent> {
    match item {
        MultiTurnStreamItem::StreamAssistantItem(content) => {
            map_assistant_content(content, tool_call_buffers, outcome)
        }
        MultiTurnStreamItem::ToolExecutionCommitted {
            tool_call,
            internal_call_id,
        } => {
            tool_call_names.insert(internal_call_id, tool_call.function.name);
            None
        }
        MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
            tool_result,
            internal_call_id,
        }) => {
            let name = tool_call_names
                .remove(&internal_call_id)
                .unwrap_or_default();
            Some(AgentEvent::ToolResult {
                name,
                // Rig's `ToolResult` carries no error flag at this layer — that distinction
                // arrives with `M4`'s tool round-trip, once a tool crate can actually fail.
                ok: true,
                summary: render_tool_result(&tool_result.content),
            })
        }
        MultiTurnStreamItem::CompletionCall(call) => Some(AgentEvent::Usage(call.usage)),
        MultiTurnStreamItem::ModelTurnRetried { .. } => None,
        MultiTurnStreamItem::FinalResponse(response) => {
            outcome.usage = response.usage();
            Some(AgentEvent::TurnComplete)
        }
        _ => None,
    }
}

/// Maps one `StreamedAssistantContent` item to at most one [`AgentEvent`] (`M2-1`/`M2-2`).
/// Exhaustive over every variant that exists today — `StreamedAssistantContent` is not
/// `#[non_exhaustive]`, so unlike `map_item` this one carries no wildcard arm at all.
fn map_assistant_content<R>(
    content: StreamedAssistantContent<R>,
    tool_call_buffers: &mut HashMap<String, ToolCallBuffer>,
    outcome: &mut TurnOutcome,
) -> Option<AgentEvent> {
    match content {
        StreamedAssistantContent::Text(text) => {
            outcome.text.push_str(&text.text);
            Some(AgentEvent::Token(text.text))
        }
        StreamedAssistantContent::ToolCallDelta {
            internal_call_id,
            content,
            ..
        } => {
            tool_call_buffers
                .entry(internal_call_id)
                .or_default()
                .push(&content);
            None
        }
        StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id,
        } => {
            tool_call_buffers.remove(&internal_call_id);
            Some(AgentEvent::ToolCallStarted {
                name: tool_call.function.name,
            })
        }
        StreamedAssistantContent::Reasoning(_) => None,
        StreamedAssistantContent::ReasoningDelta { .. } => None,
        StreamedAssistantContent::Final(_) => None,
        StreamedAssistantContent::Unknown(_) => None,
    }
}

/// Renders a tool result's content items as the text an `AgentEvent::ToolResult` carries.
/// Truncation to a display-safe size is `mate-tool-api`'s job (`M3`'s `truncate_with_notice`);
/// this just flattens whatever Rig handed back.
fn render_tool_result(content: &OneOrMany<ToolResultContent>) -> String {
    content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text(text) => text.text.clone(),
            ToolResultContent::Image(_) => "[image]".to_string(),
            ToolResultContent::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use rig::agent::{CompletionCall, PromptResponse};
    use rig::message::{Text, ToolCall, ToolFunction, ToolResult};
    use rig::test_utils::{MockCompletionModel, MockStreamEvent};
    use tokio_stream::wrappers::ReceiverStream;

    fn text_item(text: &str) -> Result<MultiTurnStreamItem<()>, StreamingError> {
        Ok(MultiTurnStreamItem::StreamAssistantItem(
            StreamedAssistantContent::Text(Text::new(text)),
        ))
    }

    #[tokio::test]
    async fn text_deltas_arrive_in_order() {
        let mut stream = Box::pin(futures::stream::iter(vec![
            text_item("Hel"),
            text_item("lo"),
            text_item("!"),
        ]));
        let cancel = CancellationToken::new();
        let mut tokens = Vec::new();

        let outcome = drive(&mut stream, &cancel, |envelope| {
            if let AgentEvent::Token(text) = envelope.event {
                tokens.push(text);
            }
        })
        .await;

        assert_eq!(tokens, vec!["Hel", "lo", "!"]);
        assert_eq!(outcome.text, "Hello!");
        assert!(!outcome.cancelled);
        assert!(outcome.error.is_none());
    }

    #[test]
    fn tool_call_argument_deltas_reassemble_when_split_mid_token() {
        // "src/build.rs" split at arbitrary byte offsets that don't line up with any token
        // boundary in the surrounding JSON.
        let mut buffer = ToolCallBuffer::default();
        buffer.push(&ToolCallDeltaContent::Name("read_f".to_string()));
        buffer.push(&ToolCallDeltaContent::Name("ile".to_string()));
        buffer.push(&ToolCallDeltaContent::Delta("{\"pat".to_string()));
        buffer.push(&ToolCallDeltaContent::Delta("h\": \"src/buil".to_string()));
        buffer.push(&ToolCallDeltaContent::Delta("d.rs\"}".to_string()));

        assert_eq!(buffer.name, "read_file");
        assert_eq!(buffer.arguments, "{\"path\": \"src/build.rs\"}");
    }

    #[tokio::test]
    async fn tool_call_deltas_alone_emit_nothing_only_the_complete_call_does() {
        let items: Vec<Result<MultiTurnStreamItem<()>, StreamingError>> = vec![
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ToolCallDelta {
                    id: "call_1".to_string(),
                    internal_call_id: "internal_1".to_string(),
                    content: ToolCallDeltaContent::Name("read_file".to_string()),
                },
            )),
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ToolCallDelta {
                    id: "call_1".to_string(),
                    internal_call_id: "internal_1".to_string(),
                    content: ToolCallDeltaContent::Delta("{\"path\":\"a\"}".to_string()),
                },
            )),
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ToolCall {
                    tool_call: ToolCall {
                        id: "call_1".to_string(),
                        call_id: None,
                        function: ToolFunction::new(
                            "read_file".to_string(),
                            serde_json::json!({"path": "a"}),
                        ),
                        signature: None,
                        additional_params: None,
                    },
                    internal_call_id: "internal_1".to_string(),
                },
            )),
        ];
        let mut stream = Box::pin(futures::stream::iter(items));
        let cancel = CancellationToken::new();
        let mut events = Vec::new();

        drive(&mut stream, &cancel, |envelope| events.push(envelope.event)).await;

        assert_eq!(
            events,
            vec![AgentEvent::ToolCallStarted {
                name: "read_file".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn maps_tool_result_completion_call_and_final_response() {
        let items: Vec<Result<MultiTurnStreamItem<()>, StreamingError>> = vec![
            Ok(MultiTurnStreamItem::ToolExecutionCommitted {
                tool_call: ToolCall {
                    id: "call_1".to_string(),
                    call_id: None,
                    function: ToolFunction::new(
                        "read_file".to_string(),
                        serde_json::json!({"path": "a"}),
                    ),
                    signature: None,
                    additional_params: None,
                },
                internal_call_id: "internal_1".to_string(),
            }),
            Ok(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult {
                    tool_result: ToolResult {
                        id: "call_1".to_string(),
                        call_id: None,
                        content: OneOrMany::one(ToolResultContent::Text(Text::new(
                            "contents of a",
                        ))),
                    },
                    internal_call_id: "internal_1".to_string(),
                },
            )),
            Ok(MultiTurnStreamItem::CompletionCall(CompletionCall::new(
                0,
                Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    total_tokens: 15,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    tool_use_prompt_tokens: 0,
                    reasoning_tokens: 0,
                },
            ))),
            Ok(MultiTurnStreamItem::FinalResponse(PromptResponse::new(
                "done",
                Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    total_tokens: 15,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    tool_use_prompt_tokens: 0,
                    reasoning_tokens: 0,
                },
            ))),
        ];
        let mut stream = Box::pin(futures::stream::iter(items));
        let cancel = CancellationToken::new();
        let mut events = Vec::new();

        let outcome = drive(&mut stream, &cancel, |envelope| {
            assert_eq!(envelope.agent, AgentId::ROOT);
            events.push(envelope.event)
        })
        .await;

        assert_eq!(
            events,
            vec![
                AgentEvent::ToolResult {
                    name: "read_file".to_string(),
                    ok: true,
                    summary: "contents of a".to_string(),
                },
                AgentEvent::Usage(Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    total_tokens: 15,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    tool_use_prompt_tokens: 0,
                    reasoning_tokens: 0,
                }),
                AgentEvent::TurnComplete,
            ]
        );
        assert_eq!(outcome.usage.total_tokens, 15);
    }

    #[tokio::test]
    async fn cancelling_mid_stream_leaves_a_coherent_history() {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<MultiTurnStreamItem<()>, StreamingError>>(8);
        tx.send(text_item("Hel")).await.unwrap();
        tx.send(text_item("lo")).await.unwrap();
        let mut stream = Box::pin(ReceiverStream::new(rx));

        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        let canceller_task = tokio::spawn(async move {
            // Give `drive` time to drain the two buffered items and block on the (never
            // sent) third before cancelling — proves cancellation wins over a hang, not
            // just over an already-finished stream.
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });

        let mut tokens = Vec::new();
        let outcome = drive(&mut stream, &cancel, |envelope| {
            if let AgentEvent::Token(text) = envelope.event {
                tokens.push(text);
            }
        })
        .await;
        canceller_task.await.unwrap();

        assert!(outcome.cancelled);
        assert!(outcome.error.is_none());
        assert_eq!(tokens, vec!["Hel", "lo"]);
        assert_eq!(outcome.text, "Hello");
        // The channel — and so the underlying provider stream in the real path — is still
        // open; `drive` stopped polling it rather than the stream ending on its own.
        drop(tx);
    }

    #[tokio::test]
    async fn a_streaming_error_is_reported_and_keeps_text_seen_so_far() {
        let items: Vec<Result<MultiTurnStreamItem<()>, StreamingError>> = vec![
            text_item("partial"),
            Err(StreamingError::Completion(
                rig::completion::CompletionError::ProviderError("boom".to_string()),
            )),
        ];
        let mut stream = Box::pin(futures::stream::iter(items));
        let cancel = CancellationToken::new();
        let mut saw_error = false;

        let outcome = drive(&mut stream, &cancel, |envelope| {
            if matches!(envelope.event, AgentEvent::Error(_)) {
                saw_error = true;
            }
        })
        .await;

        assert!(saw_error);
        assert!(outcome.error.is_some());
        assert!(!outcome.cancelled);
        assert_eq!(outcome.text, "partial");
    }

    #[test]
    fn usage_rollup_sums_across_turns_and_records_per_turn_sent_tokens() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(Usage {
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        });
        rollup.record_root_turn(Usage {
            input_tokens: 50,
            output_tokens: 10,
            total_tokens: 60,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        });

        assert_eq!(rollup.root.input_tokens, 150);
        assert_eq!(rollup.root.output_tokens, 30);
        assert_eq!(rollup.root.total_tokens, 180);
        assert_eq!(rollup.per_turn, vec![100, 50]);
        assert_eq!(rollup.turns, 2);
        // `record_subagent_turn` was never called.
        assert_eq!(rollup.subagents, Usage::new());
    }

    #[test]
    fn per_turn_history_is_bounded_but_turns_and_totals_stay_uncapped() {
        let mut rollup = UsageRollup::default();
        for _ in 0..40 {
            rollup.record_root_turn(Usage {
                input_tokens: 1,
                output_tokens: 0,
                total_tokens: 1,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
                tool_use_prompt_tokens: 0,
                reasoning_tokens: 0,
            });
        }

        assert_eq!(
            rollup.per_turn.len(),
            32,
            "the sparkline window must stay capped at PER_TURN_HISTORY regardless of turn count"
        );
        assert_eq!(
            rollup.turns, 40,
            "the uncapped turn counter must still reflect every turn, not just the window"
        );
        assert_eq!(
            rollup.root.input_tokens, 40,
            "totals must reconcile with every turn recorded, not just the windowed sum"
        );
    }

    #[test]
    fn record_subagent_turn_folds_into_the_subagent_side_without_touching_root() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(Usage {
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        });
        rollup.record_subagent_turn(Usage {
            input_tokens: 40,
            output_tokens: 8,
            total_tokens: 48,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        });

        assert_eq!(
            rollup.root.input_tokens, 100,
            "a subagent's usage must never bleed into the root side of the rollup"
        );
        assert_eq!(rollup.subagents.input_tokens, 40);
        assert_eq!(rollup.subagents.total_tokens, 48);
        assert_eq!(
            rollup.turns, 1,
            "the root turn counter must not count a subagent's report as a root turn"
        );
    }

    #[test]
    fn agent_id_root_is_zero() {
        assert_eq!(AgentId::ROOT, AgentId(0));
    }

    #[tokio::test]
    async fn stream_turn_sends_no_prior_history() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("hi"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = rig::agent::AgentBuilder::new(model.clone()).build();
        let cancel = CancellationToken::new();

        stream_turn(&agent, "hello", &cancel, |_| {}).await;

        assert_eq!(model.requests()[0].chat_history.len(), 1);
    }

    #[tokio::test]
    async fn stream_turn_with_history_threads_prior_messages_into_the_request() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("hi"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = rig::agent::AgentBuilder::new(model.clone()).build();
        let cancel = CancellationToken::new();
        let history = vec![
            Message::user("earlier question"),
            Message::assistant("earlier answer"),
        ];

        stream_turn_with_history(&agent, "follow up", &history, &cancel, |_| {}).await;

        assert_eq!(model.requests()[0].chat_history.len(), 3);
    }
}
