# Rig practices in `mate`

Read this before touching agent construction (`mate-core/src/agent.rs`,
`backend.rs`), the streaming/event layer (`streaming.rs`), a tool impl in
any `mate-tool-*` crate, or `toolset.rs`. `mate` pins `rig = "0.41.0"`
(root facade re-exporting `rig-core` + `rig-agent`) — version-specific
behavior below is as of that pin; re-check on any `rig` bump.

## `PortableTool`, not `Tool`

Every tool in this workspace (`ReadFile`, `ListDir`, `FindFiles`,
`HttpRequest`, `SpawnAgent`) implements `rig::tool::PortableTool`, not the
plain `Tool` trait. Two reasons this matters:

- **`Tool` is not object-safe** (`const NAME` plus associated
  `Args`/`Output`/`Error` types), so `Vec<Arc<dyn Tool>>` won't compile.
  `PortableTool` is what `rig::tool::server::ToolServer` accepts, and
  `ToolServer::run()` returns a `ToolServerHandle` — an object-safe handle
  that attaches to an `AgentBuilder` via `.tool_server_handle(handle)`.
- **`.tool_server_handle()` is generic over the provider model**, unlike
  `.tool()` which pins the builder's typestate. `mate-core::toolset::
  build_toolset` builds one `ToolServerHandle` and it works for any of
  `Backend`'s three provider paths (`BuiltAgent::HuggingFace` /
  `::OpenAiCompatible` / `::Gemini`) without rebuilding the toolset per
  variant. Reach for
  `ToolServer`/`ToolServerHandle` any time you need one toolset value usable
  against more than one concrete completion model.

A tool struct captures everything `call(&self, args)` needs at construction
— `call` takes **no context parameter**. Root, output caps, the subagent
spawner, and cancellation all have to already live on `self` (i.e. on
`ToolCtx`, captured by value into the tool). This is why a fresh tool
instance gets built per agent (root or subagent): the agent boundary is
baked into the tool value itself, not passed in per call.

## `Tool`/`ToolFailure` error mapping

`type Error = ToolFailure` on every tool; `map_error(&self, error) ->
ToolExecutionError` is `error.into()`, backed by `impl From<ToolFailure> for
ToolExecutionError` in `mate-tool-api`. `ToolExecutionError::new(kind,
message)` makes `message` the value `.model_feedback()` returns — this is
*why* `ToolFailure`'s `Display` impls are written as recovery instructions
for the model, not operator diagnostics: that exact string is what
`ToolExecutionError`'s redacted kind-level fallback text would otherwise
replace it with. Don't construct a `ToolExecutionError` any other way in
this codebase.

## `schemars` v1 gotcha: doc comments, not attributes

Field descriptions in a tool's JSON schema come from ordinary `///` doc
comments on the args struct, **not** `#[schemars(description = "...")]`.
Older `schemars` 0.8-era examples online use the attribute form; it compiles
against `schemars` 1.x but produces a schema with no description for that
field — and a missing description is most of what makes a model call the
wrong tool, or the right tool wrong. Every args struct in this workspace
(`ReadFileArgs`, `SpawnAgentArgs`, `HttpRequestArgs`, …) uses `///` on every
field for exactly this reason, and every tool has a
`schema_carries_field_descriptions` test asserting a specific field's
`schema["properties"][name]["description"]` is non-empty and contains an
expected substring. Add that test for any new tool args struct — schemars
silently producing a description-free schema is not something clippy or
`cargo check` catches.

## Agent construction: enum-per-provider-model, not one generic type

```rust
pub enum BuiltAgent {
    HuggingFace(Agent<huggingface::completion::CompletionModel>),
    OpenAiCompatible(Agent<openai::completion::CompletionModel>),
    Gemini(Agent<gemini::completion::CompletionModel>),
}
```

`Agent<M>` is generic over the completion model, and `Backend`'s three
provider paths (`HuggingFace` native, the `openai`-compatible fallback
pointed at HF's router or a local server, and `gemini` native) produce
genuinely distinct model types. `BuiltAgent` carries that distinction
forward as an enum rather than erasing it behind a trait object — every
call site that needs to drive a turn (`stream_turn` in `streaming.rs`,
`spawn_supervised` in `session.rs`, `drive_subagent` in `subagent.rs`)
`match`es on it and calls the same generic function against whichever
variant it got. If you add a fourth provider path, this is the pattern to
extend, not a boxed `dyn` abstraction over `Agent<_>`.

Functions that need to work across both variants without knowing which are
written generic over the completion model instead, bounded by what they
actually need — see `drive_subagent`'s `M: CompletionModel + 'static, M::
StreamingResponse: GetTokenUsage` bound in `subagent.rs`. This is also what
makes those functions directly testable against `rig::test_utils::
MockCompletionModel` with no `Backend` and no network at all.

## `SubProvider` doesn't do what the name implies

Rig 0.41.0's HuggingFace `SubProvider` enum only actually affects the
outgoing request for the `Fireworks` partner (it rewrites the model id to
Fireworks' native `accounts/fireworks/models/…` form). Every other partner
name is otherwise silently ignored by Rig itself: the request URL is always
`v1/chat/completions` and the model id passes through unqualified, so HF's
router auto-selects a provider instead of honoring the one `mate` configured
— and 404s if auto-selection can't resolve the model. `Backend::
qualify_model(model)` works around this by appending `:<partner-slug>` (HF's
documented `model:provider` pinning syntax) to the model id itself, before
it ever reaches Rig. Don't assume setting `sub_provider` on a `ClientBuilder`
is sufficient for anything but `Fireworks` — check `backend.rs` /
`providers.md` before relying on partner routing for a new sub-provider.

## `VerifyClient::verify()` hits the wrong host for HF's router path

Rig's built-in `client.verify()` for the HuggingFace client sends `GET
{base_url}/api/whoami-v2` — a Hub account-info route that lives on
`huggingface.co`, not `router.huggingface.co` (the router only proxies
inference endpoints). On the **default** router path this 404s
unconditionally, before any agent is built, regardless of model,
sub-provider, or token validity. `Backend::verify_huggingface_hub_token`
sidesteps it with its own `reqwest::Client` hitting
`https://huggingface.co/api/whoami-v2` directly. If you add a new verify-
style check against an HF client, check which host the built-in method
actually targets before trusting it end-to-end — see `providers.md` for the
full account. This only applies to the default router path; a caller-
overridden `base_url` (a dedicated Inference Endpoint) or the
`OpenAiCompatible` variant use Rig's `verify()` unchanged, since there the
caller is taken at their word about what's listening there.

## Streaming: two match functions, on purpose

`streaming.rs` has `map_assistant_content` (matches `StreamedAssistantContent`
— **not** `#[non_exhaustive]` in Rig, zero wildcard arms) and `map_item`
(matches `MultiTurnStreamItem` — **is** `#[non_exhaustive]`, one trailing
`_ => None`). Keep that split when extending either: adding a new arm to
`map_assistant_content` means thinking about every existing variant too,
since the compiler will force it; `map_item`'s wildcard is a compiler
requirement, not a license to quietly swallow a case you didn't handle —
match every variant that exists today explicitly above it regardless.

Tool-call streaming varies by provider in how finely a call is chunked —
some stream `ToolCallDeltaContent::Name`/`Arguments(String)` fragments
before the complete call, some send it whole. Buffer fragments per
`internal_call_id`; **never** synthesize a "tool call started" event from
partial deltas — only the complete `StreamedAssistantContent::ToolCall` item
produces `AgentEvent::ToolCallStarted`.

`drive()` is generic over `Stream<Item = Result<MultiTurnStreamItem<R>,
StreamingError>> + Unpin`, so tests build synthetic streams with
`futures::stream::iter(..)` — no live provider or mock model needed for the
event-mapping layer itself. Use `R = ()` for synthetic items;
`MultiTurnStreamItem`'s variants only depend on `R` through
`StreamedAssistantContent::Final(R)`, which mapping tests don't need.

`tokio::select! { biased; ... }` between cancellation and the next stream
item matters: it guarantees cancellation wins a simultaneous-ready race, so
a turn cancelled the instant another item is also ready still stops rather
than processing one more event. Dropping the stream (once `drive` returns)
is what actually aborts the in-flight request — Rig's `Abortable` wrapper
propagates through `Drop`.

## `AgentHook` for advisory request patching

`TurnCapHook` (`mate-core/src/turn_cap.rs`) implements `rig::agent::
AgentHook::on_completion_call`, returning either `CompletionCallAction::
continue_run()` or `::patch(RequestPatch::new().tool_choice(ToolChoice::
None).context(doc))` to force a tool-free final answer once a turn budget is
exhausted. Two things worth knowing before writing another hook:

- **This is advisory, not enforcement.** `ToolChoice::None` is a request to
  the provider; a model (or a test double that ignores it) can still emit a
  tool call anyway. The actual "does this terminate at all" guarantee comes
  from Rig's own `default_max_turns` budget on the `AgentBuilder`,
  independent of any hook — a hook only improves the *outcome* for
  cooperative models.
- Register with `AgentBuilder::add_hook(...)`; a hook only fires at
  `on_completion_call`, once per completion round, with `event.turn`
  telling you which round you're in — compare against your own budget, not
  against the builder's `default_max_turns` (they can differ, and the hook
  has no way to read the builder's own setting back out).

## Mock testing utilities (`rig::test_utils`)

`MockCompletionModel::from_stream_turns([...])` takes one `Vec<MockStreamEvent>`
per completion round; `MockStreamEvent::{text, tool_call, final_response_with_total_tokens}`
build the events. `MockAddTool` is a trivial real tool for exercising a tool-
call round; `MockControlledTool::new(started: Arc<Notify>, never_finishes:
Arc<Notify>)` is how this codebase tests wall-clock timeouts and mid-flight
cancellation — it notifies `started` on entry and then waits forever on
`never_finishes`, so a test can `select!` on `started.notified()` to know
the tool call is genuinely in-flight before cancelling or waiting out a
timeout, rather than racing against real wall-clock time with a `sleep`.
`AgentBuilder::new(model).tool(...).default_max_turns(n).add_hook(...).build()`
assembles a real `Agent<MockCompletionModel>` with no network at all —
`model.requests()` afterward gives you every request the mock model actually
received, for asserting things like `chat_history.len()` or `tool_choice`.
Use this pattern (not a live/`#[ignore]`d test) for anything that exercises
agent behavior rather than raw HTTP/provider wiring.

## `Usage` accumulation

`rig::completion::Usage` has `input_tokens`, `output_tokens`, `total_tokens`,
`cached_input_tokens`, `cache_creation_input_tokens`,
`tool_use_prompt_tokens`, `reasoning_tokens` — when folding usage across
turns (`UsageRollup::record_root_turn`/`record_subagent_turn` in
`streaming.rs`), accumulate every field, not just the three obvious ones;
tests construct a full `Usage` literal (see any test's `fn usage(...)`
helper) rather than `Default::default()` plus two fields, so a forgotten
field shows up as a wrong total instead of silently defaulting to zero.
