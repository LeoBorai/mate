# Proposal

## Why

`mate` agents can only use tools that ship as `mate-tool-*` crates today —
extending what an agent can do (query a database, call a ticketing API, drive
some other local automation) currently means writing Rust and shipping a
release. Model Context Protocol (MCP) is the emerging standard for exposing
exactly that kind of external tool to an LLM agent without the agent's host
needing native code per integration. `rig = "0.41.0"` (this workspace's pin)
has no MCP support at all — no `mcp` feature, no MCP client dependency
anywhere in `Cargo.lock` — so this is a build-it-ourselves integration, not a
flag flip.

## What Changes

- New `mate-tool-mcp` crate: an MCP client over the **stdio transport only**
  (JSON-RPC over a spawned child process's stdin/stdout). Remote transports
  (HTTP/SSE) are explicitly out of scope for this change.
- **One single `McpProxy` `PortableTool` covering every configured MCP
  server** — not one tool per server, and not one tool per MCP-advertised
  tool. Confirmed against this workspace's actual tool impls (`read_file`,
  `write_file`, `list_dir`, `find_files`, `skill`): `PortableTool::NAME` is a
  literal compile-time `const &'static str` with no instance-level override,
  so it cannot vary per runtime-configured server name. `mate-tool-skills`'s
  `Skill` tool is the exact precedent — one tool (`NAME = "skill"`)
  dispatching over a runtime-discovered list by a `name` arg — and
  `McpProxy` follows the same shape: `NAME = "mcp"`, `call` args carry
  `{server, tool, arguments}`, dispatch happens inside `call` against the
  set of initialized servers. The proxy's description enumerates every
  configured server and its allow-listed tool names/descriptions so the
  model can pick one without a separate list call.
- New config surface (mate-cli's layered config, `config.md`): zero or more
  named MCP servers, each with a spawn command, args, and env — plus an
  explicit **per-server allow-list of tool names** the model may call
  unattended. A tool name absent from a server's allow-list is refused
  outright (`ToolFailure::Denied`), the same default-narrow posture
  `http_request` uses for its GET/HEAD-only method gate. No approval-prompt
  flow for MCP calls in this change.
- `mate-core::toolset::build_toolset` attaches one `McpProxy` per
  successfully-initialized configured server, conditionally, the same way
  `http_request`/`skill` attach today.
- Subprocess lifecycle: each configured server is spawned and its MCP
  session initialized once, with crash isolation so one broken server
  doesn't take the agent down or block tool calls to the others; exact
  scope (process-wide vs. per-session) is a design decision, not fixed here.
- `tool_descriptors` / preamble rendering gains an entry per attached
  `McpProxy`.
- New `ToolActivity` variant for an MCP call (existing variants are
  `FileTouched`/`NetRequest`, neither fits) — the panel widget consuming it
  is a follow-on, not required by this change.

**Non-goals** (explicitly deferred):
- Remote MCP transports (HTTP/SSE) and the SSRF/DNS-rebinding hardening they
  would require (`mate-tool-http`'s threat model).
- Per-call human approval for MCP tool calls (`write_file`'s pattern) — this
  change uses a static per-server allow-list instead.
- Registering each MCP-advertised tool as its own top-level Rig tool.
- Subagent access to MCP tools — subagents get none in this change.

## Capabilities

### New Capabilities
- `mcp-tools`: configuring MCP servers, spawning and initializing them over
  stdio, proxying allow-listed tool calls through a per-server `PortableTool`,
  and refusing calls to tools not on that server's allow-list.

### Modified Capabilities
(none — no existing specs cover tool registration or the toolset builder;
`openspec list --specs` returns no capabilities in this project yet)

## Impact

- New crate `mate-tool-mcp` (workspace member, new MCP-client dependency).
- `mate-core`: `toolset.rs` (conditional attachment), `config.rs` (new
  `[[mcp.servers]]`-style section), `preamble.rs`/`tool_descriptors` (new
  descriptor entries).
- `mate-tool-api`: new `ToolActivity` variant; possibly a new `ToolFailure`
  reason for "tool not on this server's allow-list".
- `Cargo.toml` workspace deps: new MCP client/JSON-RPC dependency.
- `mate-cli`: config parsing/validation for the new section, `--help`/docs.
- Docs: `.agents/docs/architecture.md`, `tools.md`, `config.md` get a new
  section once implemented (tracked in tasks, not part of this proposal).
