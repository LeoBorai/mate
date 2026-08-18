# Streaming and the agent event model

Lives in `mate-core/src/streaming.rs`; entry point is
`BuiltAgent::stream_turn` (`agent.rs`), which just resolves `BuiltAgent`'s
provider match and calls `streaming::stream_turn` underneath.

## The shapes

- `AgentId(u32)` — index of an agent within a session; `AgentId::ROOT` is the
  root agent. Defined here (not deferred to the session manager) so it never
  has to be retrofitted onto an existing event type later.
- `AgentEvent` — one event per streamed item: `Token`, `ToolCallStarted`,
  `ToolResult`, `ApprovalRequired`, `SubagentSpawned`, `SubagentFinished`,
  `Usage`, `Activity`, `TurnComplete`, `Error`. `SubagentSpawned`/`Finished`
  are produced by `mate-core/src/subagent.rs`'s `drive_subagent` (`M9`,
  `delegation.md`), tagged with the subagent's own `AgentId`, not
  `AgentId::ROOT` — the routing key the side panel (`M12`, `panel.md`) uses.
  `Activity(ToolActivity)` (`M11-4`) is folded onto this stream from
  `ToolCtx::activity` by `SessionManager::spawn`, not produced by `drive`/
  `map_item` itself — a tool call and its telemetry record don't arrive from
  the same place `MultiTurnStreamItem` does. `ApprovalRequired` still has no
  producer (`M13`) — it exists so the approval-flow work that does produce
  it isn't a breaking enum change.
- `SubagentOutcome` — `Completed{summary}` / `Failed{reason}` / `Cancelled` /
  `TimedOut`. Same "define before there's a producer" reasoning.
- `AgentEventEnvelope { agent: AgentId, event: AgentEvent }` — every event is
  already tagged with the agent that produced it. This is a deliberate
  stand-in for a future session-scoped envelope that adds a session key on
  top; don't invent a second envelope shape when that lands, extend this one.

## The drive loop

`stream_turn(agent, prompt, cancel, on_event)` calls `agent.stream_prompt(..)`
(Rig's `StreamingPrompt` trait) and hands the resulting `MultiTurnStreamItem`
stream to `drive()`, the medium-independent poll loop:

```rust
tokio::select! {
    biased;
    _ = cancel.cancelled() => { /* stop, mark cancelled */ }
    next = stream.next() => { /* map to AgentEvent, call on_event */ }
}
```

`biased` matters: cancellation always wins a simultaneous ready-item race, so
a turn cancelled the instant another item is also ready still stops rather
than processing one more event first. Dropping the stream (which happens once
`drive` returns) is what actually aborts the in-flight request — Rig's
`Abortable` wrapper propagates through `Drop`.

`drive()` returns a `TurnOutcome { text, usage, cancelled, error }`. `text` is
whatever assistant text streamed before the loop stopped, however it stopped
— a cancelled or errored turn still has a coherent partial transcript, never
a torn one.

## Tool-call delta buffering

Providers vary in how finely they chunk a tool call: some stream
`ToolCallDeltaContent::Name`/`Delta` fragments before the complete call, some
just send the complete call directly. `ToolCallBuffer` accumulates fragments
per `internal_call_id`; **deltas alone never produce an event** — only the
complete `StreamedAssistantContent::ToolCall` item does, via
`AgentEvent::ToolCallStarted`. Don't try to synthesize a tool-call-started
event from partial deltas — wait for completion.

## Mapping and `#[non_exhaustive]`

Two separate match functions, on purpose:

- `map_assistant_content` matches `StreamedAssistantContent` — **not**
  `#[non_exhaustive]` in Rig, so this match has zero wildcard arms. If you add
  a new arm here, you're required to think about every existing variant too.
- `map_item` matches `MultiTurnStreamItem` — **is** `#[non_exhaustive]` in
  Rig (it may grow new medium-level lifecycle variants in a future Rig minor),
  so it carries one trailing `_ => None`. That wildcard is a compiler
  requirement, not a place to quietly swallow a case you didn't want to
  handle — every variant that exists today is matched explicitly above it.

## Usage

`UsageRollup { root, subagents, per_turn, turns }` accumulates token usage
across turns. `record_root_turn(usage)` folds one completed root turn's
`Usage` into `root`, increments `turns` (uncapped — the denominator
`cost.rs`'s `per_turn_avg` needs, since `per_turn` itself is capped), and
pushes `input_tokens` onto `per_turn` (sent-tokens-per-turn, oldest first,
capped at `PER_TURN_HISTORY` — the raw data the panel's sparkline reads;
`panel.md`). `record_subagent_turn(usage)` (`M11-5`) folds one subagent
completion round's `Usage` into `subagents` — called once per
`AgentEvent::Usage` tagged with a non-root `AgentId`, the same cadence
`record_root_turn` is called at for the root. No `per_turn`/`turns`
bookkeeping for subagent turns — the sparkline is root-only.

## Testing pattern

`drive()` is generic over any `Stream<Item = Result<MultiTurnStreamItem<R>,
StreamingError>> + Unpin`, so tests build synthetic streams with
`futures::stream::iter(..)` or a `tokio::sync::mpsc` channel + `ReceiverStream`
— no live provider or mock model needed. Use `R = ()` for synthetic items;
`MultiTurnStreamItem`'s variants only depend on `R` through the
`StreamedAssistantContent::Final(R)` case, which none of these tests need.
