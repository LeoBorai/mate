# Design

## Context

See `proposal.md` - Why/What Changes for motivation and scope. Relevant
existing shape (`.agents/docs/tools.md`, `toolset.rs`,
`.agents/skills/mate-software-engineer/refs/rig.md`):

- `mate-core::toolset::build_toolset(ctx, http_policy, http_shared)` builds a
  `rig::tool::server::ToolServer`, conditionally `.tool(...)`-ing in each
  crate's `PortableTool` impl, and returns one `ToolServerHandle` shared
  across every `BuiltAgent` provider variant.
- Every `PortableTool` impl in this workspace declares `const NAME: &'static
  str` — verified in `read_file.rs`, `write_file.rs`, `list_dir.rs`,
  `find_files.rs`, `skill.rs`. There is no per-instance name override in the
  trait. `mate-tool-skills::Skill` is a single tool (`NAME = "skill"`)
  dispatching over `ctx.skills` (a runtime-discovered `Arc<[SkillMetadata]>`)
  by a `name` argument — this is the shape `McpProxy` reuses.
- `ToolCtx` (`mate-tool-api`) is captured by value into a tool struct at
  construction; `call(&self, args)` takes no context parameter. A tool
  needing shared, mutable, or async-initialized state (like open MCP
  sessions) carries it as an `Arc<...>` field on `self`, the same way
  `HttpRequest` carries `Arc<HttpShared>`.
- `rig = "0.41.0"` has no MCP support (`Cargo.lock` has no MCP-related
  crate); this design picks an MCP client dependency from scratch.

## Goals / Non-Goals

**Goals:**
- Define the crate boundary, the `McpProxy` tool shape, and the server
  lifecycle precisely enough that `tasks.md` can be broken into concrete,
  independently verifiable steps.
- Keep the failure model consistent with existing tools: a `ToolFailure`
  variant whose `Display` is a recovery instruction for the model, per
  `rig.md`'s `Tool`/`ToolFailure` error mapping section — never an operator
  diagnostic.

**Non-Goals** (see proposal.md for the product-level non-goals list):
- Choosing the exact MCP client crate's minor version or vendoring our own
  JSON-RPC/stdio framing if a suitable crate exists — `tasks.md` resolves the
  concrete dependency; this design only fixes the shape it must fit.
- Panel UI for the new `ToolActivity` variant — only the variant's shape is
  fixed here.

## Decisions

### One `McpProxy` tool, not one per server or per MCP tool

`PortableTool::NAME` is a compile-time associated constant with no
per-instance override (confirmed against every existing impl in this
workspace). N runtime-configured servers cannot each get a distinctly-named
Rust type without generating an arbitrary, capped family of "slot" types
(considered and rejected: the model-visible name wouldn't reflect the real
server name, and the cap is an artificial scaling limit with no natural
value). `McpProxy` therefore mirrors `mate-tool-skills::Skill` exactly:

```rust
pub struct McpArgs {
    /// Name of a configured MCP server, from the "Configured MCP servers" preamble section.
    pub server: String,
    /// Name of a tool that server advertises and allow-lists.
    pub tool: String,
    /// Arguments for that tool, matching its advertised input schema.
    pub arguments: serde_json::Value,
}

impl PortableTool for McpProxy {
    const NAME: &'static str = "mcp";
    type Args = McpArgs;
    type Output = serde_json::Value;
    type Error = ToolFailure;
    // description() enumerates every initialized server, its allow-listed
    // tool names, and each tool's own description, so the model can choose
    // without a separate list call.
}
```

`arguments` is `serde_json::Value` rather than a generated per-tool struct:
MCP tool schemas are only known after each server's `tools/list` response,
which arrives at runtime after the process is already compiled — the same
reason `Args` itself can't be a per-tool type. Schema validation against the
server's advertised schema happens inside `call`, before forwarding, not via
`schemars`-derived compile-time validation.

**Alternative considered**: a bounded set of compile-time "slot" tool types
(`mcp_slot_0`..`mcp_slot_7`) bound to configured servers in declaration
order. Rejected: arbitrary server-count cap, and the model-visible tool name
carries no information about which real server it targets, pushing that
entirely into the description text anyway — strictly worse than one tool
whose args carry the server name explicitly.

### Server registry: `Arc<McpServers>` shared like `HttpShared`

One process-wide (or session-wide — see Open Questions) registry holds every
configured server's live state: its child process handle, its JSON-RPC
transport, its advertised tool list, and its configured allow-list. Built
once at startup (parallel to `mate_tool_http::HttpShared`), handed down as
`Arc<McpServers>` into `McpProxy::new(ctx, servers)`, and into
`build_toolset` the same way `http_shared` already is. A server that fails
to spawn or initialize is recorded as failed in the registry rather than
omitted silently — `McpProxy`'s `call` distinguishes "server not configured"
from "server configured but failed to initialize" in its refusal message,
and the registry's construction step logs/surfaces the failure (exact
surface — `tracing` event vs. an `ActivitySink` record — left to
implementation; either satisfies the spec's "make the failure observable"
requirement).

### Allow-list enforcement happens in `McpProxy::call`, before any I/O

`call` first resolves `args.server` against the registry (refuse if absent
or failed-to-initialize), then checks `args.tool` against that server's
*advertised* tool list (refuse — "tool never advertised" — if absent), then
checks it against that server's *configured allow-list* (refuse — "tool not
allowed" — if absent), and only then forwards the MCP `tools/call` request.
Three distinct refusal reasons, three distinct `ToolFailure`-backed messages,
matching the spec's three separate scenarios (server not configured/failed,
tool not advertised, tool not allow-listed) — collapsing them into one
generic "denied" would leave the model unable to tell "you misspelled the
server" from "that tool needs to be added to config."

### stdio transport: child process + JSON-RPC over stdin/stdout

Each configured server is spawned via `tokio::process::Command` with the
configured command/args/env. MCP's stdio transport frames JSON-RPC messages
newline-delimited over the child's stdin/stdout; stderr is captured for
diagnostics, not parsed as protocol. Initialization is the MCP `initialize`
request/response followed by an `initialized` notification, then a
`tools/list` call — standard MCP handshake, no `mate`-specific variation.

### New `ToolFailure` reasons vs. reusing existing variants

Existing `ToolFailure` variants (`NotFound`/`Denied`/`InvalidArgs`/
`TooLarge`/`Timeout`/`Cancelled`/`Other`) already cover every refusal this
capability needs without a new variant: "server not configured" and "tool
not advertised" map to `NotFound`, "tool not allow-listed" maps to `Denied`,
a malformed `arguments` value against the server's schema maps to
`InvalidArgs`, and a dead/crashed server maps to `Other` (parallel to how
`mate-tool-http` uses `Other` for transport-level failures it can't classify
more specifically). No new `ToolFailure` variant is needed; the proposal's
"possibly a new `ToolFailure` reason" is resolved as **not needed**.

### New `ToolActivity::McpCall` variant

Neither `FileTouched` nor `NetRequest` fits an MCP call (no file path, and
while stdio isn't literally a network request, it's the same "external
system was contacted" telemetry class). New variant:

```rust
McpCall { server: String, tool: String, ok: bool, ms: u64 }
```

Consumed by nothing in this change (panel wiring is a follow-on per the
proposal's non-goals) but defined now so `McpProxy::call` has somewhere to
send it via `ctx.activity.try_send(...)`, matching every other tool's
telemetry pattern.

## Risks / Trade-offs

- **[Single global tool name reduces model-facing discoverability compared
  to distinctly-named per-server tools]** → Mitigated by the proxy's
  `description()` enumerating every configured server and its allow-listed
  tools up front, the same way `Skill`'s description points at the
  preamble's "Available skills" list rather than requiring a separate
  discovery call.
- **[A hung or slow-to-initialize MCP server delays agent construction]** →
  Bound server initialization with a timeout (parallel to
  `mate-tool-http::HttpLimits`'s fixed timeouts); a server that doesn't
  complete the handshake in time is treated as failed-to-initialize, same as
  any other init failure.
- **[Arbitrary external code now runs as a child process on the user's
  machine]** → The chosen trust model (allow-list per server in config, no
  per-call approval) means anything on a server's allow-list runs
  unattended. This is a deliberate, explicit config-time decision the user
  makes once per server/tool, not a runtime prompt — matches `http_request`'s
  posture (config-time policy, not per-call approval) rather than
  `write_file`'s (per-call approval). Config docs must be explicit that an
  allow-listed tool runs with no further confirmation.
- **[JSON-RPC/stdio framing bugs in a hand-rolled or unfamiliar client
  crate]** → `tasks.md` should prefer an existing, maintained MCP client
  crate over hand-rolling the wire protocol if one fits `mate`'s
  `default-features = false` / minimal-dependency conventions; hand-rolling
  is a fallback, not the default plan.

## Migration Plan

Additive only — no existing tool, config field, or spec changes behavior.
Rollout is "ship with zero servers configured by default" (matches the spec's
"no servers configured -> no MCP tool attached" requirement), so existing
users see no behavior change until they opt in by adding a `[[mcp.servers]]`
entry. No rollback concern beyond reverting the change; no data migration.

## Open Questions

- **Process-wide vs. session-scoped server lifetime.** Process-wide (like
  `HttpShared`) means one shared server process across every tab/session in
  `mate-tui`; session-scoped means each session gets its own child process
  per configured server. Either satisfies every spec requirement as written
  (they're phrased at the "agent"/"session" level, not the process level).
  Leaning process-wide for resource efficiency (matches `HttpShared`'s
  rationale exactly: N sessions shouldn't each spawn their own copy of the
  same server), but this can be decided during implementation without
  touching specs, approach, or task breakdown.
- **Exact MCP client dependency.** Left to `tasks.md`'s first step
  (evaluate available crates against `default-features = false` /
  `rustls`-only conventions already used elsewhere in this workspace, e.g.
  `reqwest`'s feature set in the root `Cargo.toml`).
