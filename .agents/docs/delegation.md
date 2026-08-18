# Delegation: subagents, `spawn_agent`, `SubagentRunner`

Read this when touching `mate-tool-agent`, `mate-core/src/subagent.rs`, the
`SubagentSpawner` seam in `mate-tool-api`, or any guardrail around
concurrent/nested agents. For the tool contracts subagents share with every
other tool (`ToolCtx`, `ToolFailure`, `ToolActivity`), see `tools.md`.

## Why a subagent exists: context firewalling

A subagent gets its own, empty context window, burns it on one narrow task,
and returns a short report. The parent's context grows by a paragraph
instead of by forty file reads. That's the entire value proposition, which
is why the report is hard-capped (`report_max_bytes`, default 2 KiB,
truncated with the standard notice) — a subagent that returns its raw
findings has achieved nothing.

## The seam: `SubagentSpawner` (`mate-tool-api::subagent`)

`mate-tool-agent`'s `spawn_agent` tool needs to build and run a full Rig
agent, which is `mate-core`'s job — but a tool crate depending on
`mate-core` would invert the dependency graph (architecture.md). So the
capability is a trait in `mate-tool-api`, implemented by `mate-core`'s
`SubagentRunner`, and injected into `ToolCtx::spawner` at construction:

```rust
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn run(&self, request: SubagentRequest) -> Result<SubagentReport, ToolFailure>;
}
```

`SubagentRequest` carries `parent: AgentId`, `label`, `task: String` (the
subagent's *entire* input — see the non-addressability section below),
`tools: ToolProfile`, `max_turns: Option<usize>`, and `cancel:
CancellationToken` — the calling tool's own `ToolCtx::cancel`, which the
spawner derives the subagent's own token from as a child.

`ToolProfile::{ReadOnly, ReadOnlyNet, Custom(Vec<String>)}` parses from the
tool's optional `tools` string argument via `TryFrom<Option<&str>>` — an
unrecognized string is a `ToolFailure::InvalidArgs` the model can read and
correct, not a panic. `None`/`"read_only"` is the default.

`ToolCtx::spawner.is_some()` is also the *entire* gate `mate_core::toolset::
build_toolset` uses to decide whether to attach `spawn_agent` at all — a
subagent whose own delegation depth is exhausted gets a `ToolCtx` with
`spawner: None`, so it structurally has no spawn tool to call. That's `M9-4`'s
depth guardrail enforced by the type system, not a runtime check.

## `spawn_agent` (`mate-tool-agent/src/spawn_agent.rs`)

A thin `PortableTool` impl: builds the `SubagentRequest`, awaits the report,
renders it. **Every guardrail — depth, concurrency, per-turn fan-out cap,
wall clock, turn budget — belongs to the spawner, never this tool.** The
tool's own job is narrow: reject with `ToolFailure::Denied("delegation not
enabled")` if `ctx.spawner` is `None`, and turn a `Cancelled` outcome into a
hard tool error (`ToolFailure::Cancelled`) rather than a report — cancellation
means the whole parent turn is already unwinding, so there's nothing useful
for the model to read.

The tool description and the `task` field's own schema doc comment are both
required to state, in so many words, that the subagent does not see the
conversation — models routinely delegate as if the child could see it, so
this is said twice on purpose (`M9-3`'s acceptance criterion, enforced by a
test that asserts both strings).

**Report format** (`render_report`):
```
subagent `deps` — completed in 6 turns, 14.2k tokens

<summary>
```
Token counts switch to `N.Nk` notation past 1000. `Failed`/`TimedOut`
outcomes render their own one-line variants; `Cancelled` never reaches
`render_report` at all (it errors before that point, per above).

## `SubagentRunner` (`mate-core/src/subagent.rs`) — the guardrail owner

One `SubagentRunner` is built per session, by `SessionManager::spawn`, only
when the session's root agent has `may_delegate: true`. Every subagent
spawned during that session's lifetime, **at any delegation depth**, shares
this runner's concurrency semaphore, per-turn spawn counter, and `AgentId`
allocator — `SubagentRunner::nested()` (used when a subagent is itself
allowed to delegate one level further) clones those `Arc`s rather than
starting fresh ones, so every limit below is session-wide, never
per-depth.

### Guardrails (`M9-4`), enforced in `acquire()`

`acquire()` is split out from `run()` specifically so these are
unit-testable without ever building a real agent:

| Limit | Config field | Behavior when exceeded |
|---|---|---|
| `max_depth` | `DelegationPolicy.max_depth` (default 1) | `ToolFailure::Denied` — checked first, before the per-turn counter even increments |
| `max_total_per_turn` | `.max_total_per_turn` (default 8) | `ToolFailure::Denied` once this turn's spawn count is exceeded |
| `max_concurrent` | `.max_concurrent` (default 4) | **Queues, does not reject** — blocks on a `tokio::sync::Semaphore::acquire_owned()` until a slot frees |
| `subagent_max_turns` | `.subagent_max_turns` (default 8) | Per-subagent turn budget, clamped: `request.max_turns.unwrap_or(policy.default).clamp(1, policy.default.max(1))` — a subagent can ask for fewer turns, never more than the session allows |
| `wall_clock_timeout_secs` | `.wall_clock_timeout_secs` (default 120) | `drive_subagent` races the turn against `tokio::time::sleep(deadline)`; on timeout it cancels the subagent's token and awaits the (now-cancelling) turn future rather than abandoning it, so the returned `SubagentReport` is always the real, final one |

`reset_turn()` zeroes the per-turn counter and is called once per root
prompt by `session_task` — so `max_total_per_turn` bounds one turn's
sequential tool rounds, not the session's whole lifetime.

Depth is opt-in via config only, **never** a tool argument the model
controls — `spawn_agent`'s `tools`/`max_turns` args narrow within what the
session already allows; nothing in the args can request more depth or more
concurrency.

### Per-subagent cancellation (`M12-9`)

`subagent_cancels: Arc<Mutex<HashMap<AgentId, CancellationToken>>>` — every
running subagent's own token, keyed by `AgentId`, populated in `run()` right
after the token is created and removed once its report is ready.
`SubagentRunner::cancel(id)` is what the panel's `x` key reaches through
(`panel.md`) to cancel *one* subagent without touching the session's own turn
or any sibling subagent; it returns `false` (not a panic) for an id that
isn't currently running.

### Cancellation is a token tree

`request.cancel` (the calling tool's `ToolCtx::cancel`) → `.child_token()` →
the subagent's own token → (if it delegates further) its own children's
tokens. `ToolCtx::cancel` is itself a child of the *session's* cancellation
token, so `SessionCmd::Shutdown` reaches every subagent at any depth at its
very next await point. **A per-turn `SessionCmd::Cancel` does not currently
reach this tree** — only `Shutdown` does — because `ToolCtx` (and the tools
built from it) is constructed once per session, not once per turn. This is a
known, documented gap, not an oversight: narrowing cancellation to a single
turn needs `ToolCtx` to carry a turn-swappable token, which nothing today
produces.

### `drive_subagent` — generic over the completion model

Split out from `SubagentRunner::run` so it's directly testable against
`rig::test_utils::MockCompletionModel` with no `Backend` and no network. It:

1. Emits `AgentEvent::SubagentSpawned { id, label, task }` before the turn
   starts, tagged with the subagent's own `AgentId` (never `AgentId::ROOT`).
2. Drives the turn via `streaming::stream_turn` (see `streaming.md`),
   forwarding every event it produces onto the session's shared channel via
   the same `forward` helper `mate-core::session` uses for the root agent.
3. Counts turns by counting `AgentEvent::Usage` events (one per completion
   round) rather than tracking it separately — matches how
   `SubagentReport::turns` is later read by the roster (`panel.md`).
4. Emits `AgentEvent::SubagentFinished { id, outcome }` after, and returns
   the `SubagentReport`.

`request.task` is the **only** prompt this agent's history ever contains —
`stream_turn`'s own "no prior history" contract, matched exactly. This is
the mechanism behind the non-addressability invariant below, not a
convention layered on top of it.

## Invariant: subagents are not addressable (§7.6)

**No user input reaches a subagent, ever, after it's spawned.** Its whole
input is fixed at spawn time: preamble + `task` string + whatever its own
tools return. This is enforced in the *types*, not the UI:

- `SubagentRequest::task` is a plain `String` set once at construction, no
  setter.
- There is no `SubagentCmd` enum and no `mpsc::Sender` pointing *into* a
  running subagent — only `cancel: CancellationToken` (user-initiated stop)
  and the outbound `events` channel exist. A future contributor who wants to
  add user steering has to add a whole new channel and command type, which
  is a visible design change, not a one-line accident.
- `mate-core/src/subagent.rs`'s own test
  (`the_subagent_sees_only_the_task_no_other_history`) asserts the mock
  model receives `chat_history.len() == 1` — exactly the task, nothing
  threaded in from any parent conversation. This is the test that stops a
  later refactor from quietly reopening the channel.

The user's only channel into subagent work is the root agent — talking to it
respawns a subagent with a better task. See `panel.md` for what the roster
does and doesn't let the user do with a running subagent (observation and
cancellation only, never text).

## Testing patterns

- **Guardrails** (`acquire`): tested directly, no agent ever built — real
  `tokio::spawn` tasks racing a real `Semaphore` to prove `max_concurrent`
  queues rather than rejects.
- **Turn-driving** (`drive_subagent`): `rig::test_utils::{MockCompletionModel,
  MockStreamEvent, MockControlledTool, MockAddTool}` — a `MockControlledTool`
  that never returns on its own is how the wall-clock-timeout and
  parent-cancellation tests prove they don't hang, wrapped in an outer
  `tokio::time::timeout` so a regression fails fast instead of hanging CI.
- **`SubagentSpawner::run` itself** (guardrails + real `Backend`/
  `build_agent` glue): only gets offline-constructible-`Backend` coverage,
  the same reason `SessionManager::spawn`'s does (see `testing.md`) — no
  live network in any test.
