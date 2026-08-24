# Tools: `mate-tool-api`, `mate-tool-fs`, `mate-tool-http`

Read this when touching a tool implementation, the path jail, SSRF guards, or
`ToolActivity` telemetry. `spawn_agent` (`mate-tool-agent`) and the subagent
runtime that backs it are covered separately in `delegation.md` — this doc is
the other three tool crates plus the shared contracts in `mate-tool-api`.

## The shared contracts (`mate-tool-api`)

Every `mate-tool-*` crate builds on five things from `mate-tool-api`:

- **`ToolCtx`** (`ctx.rs`) — captured by value into each tool struct at
  construction (`rig`'s `call(&self, args)` takes no context parameter, so
  root, caps, spawner, and cancellation must already live on `self`). Fields:
  `agent: AgentId`, `root: PathBuf` (canonicalized workspace root),
  `max_output_bytes`, `spawner: Option<Arc<dyn SubagentSpawner>>`,
  `activity: ActivitySink`, `cancel: CancellationToken`, `approvals:
  Option<Arc<dyn Approvals>>`.
- **`ToolCtx::resolve`** — the path jail (§8.1, `M3-2`): join the user path
  under `root`, canonicalize the *joined* result (never the user input alone
  — that's the whole trick, since canonicalizing before joining would never
  see a `../` traversal), require the canonical path still starts with
  `root`, then check the fixed denylist (`.env`, `*.pem`, `id_rsa*`, any path
  with a `.git` component). Every filesystem-touching tool goes through this;
  there is no other place the security boundary lives, because Rig executes
  tool calls automatically with no interception point between model and
  `call()`.
- **`ToolCtx::resolve_for_write`** — the same jail for a target that may not
  exist yet: canonicalize the *containing directory* instead of the full
  candidate (`resolve` would reject any not-yet-created file as not found),
  require that under `root`, then re-canonicalize the final path too if
  something's already there — a pre-existing symlink at the target name can
  still point outside `root` even when its parent doesn't. `write_file` is
  the only caller today.
- **`ToolFailure`** (`error.rs`) — `NotFound`/`Denied`/`InvalidArgs`/
  `TooLarge`/`Timeout`/`Cancelled`/`Other`. Deliberately not fatal: Rig feeds
  a tool's `Err` back to the model as a tool result, so every variant's
  `Display` is written as a recovery instruction for the model, not an
  operator diagnostic. `From<ToolFailure> for ToolExecutionError` carries
  that same string through as `model_feedback()` rather than falling back to
  Rig's redacted kind-level text.
- **`ToolActivity`/`ActivitySink`** (`activity.rs`) — typed telemetry a tool
  emits *alongside* its return value: `FileTouched { path, op, lines, bytes
  }` or `NetRequest { method, host, path, status, ms, bytes, redirects,
  reason }`. `FileOp::{Write,Create}` are now produced by `write_file`
  (below); `Delete` still has no producer — the same "define the shape
  before the producer lands" reasoning applied throughout this codebase (see
  `streaming.md`). The sink is `mpsc::Sender<(AgentId, ToolActivity)>`; every
  call site sends with `try_send` and ignores the result — dropping a
  telemetry record under backpressure beats stalling a tool call to report
  on itself. Consumed by `mate-tui`'s panel; see `panel.md`.
- **`Approvals`/`ApprovalRequest`** (`approval.rs`) — the seam a tool asks a
  human through: `async fn request(&self, ApprovalRequest) -> bool`, always
  a plain grant/deny with no free-text back door. `ApprovalRequest { agent,
  name, detail, path }` — `path` is the resolved, in-jail target when the
  action has one, used by `mate-core`'s implementation to remember an
  "always allow this directory" scope (see `write_file`'s section below).
  Implemented by `mate-core::approval::SessionApprovalHub`, one per session,
  injected into `ToolCtx::approvals`; `None` in a frontend that hasn't wired
  one up (`mate-cli`'s plain frontend, every tool crate's own test helpers).

Text-shaping helpers also live here, used the same way by every tool:
`number_lines(text, start)` (1-based line prefixes), `truncate_with_notice
(text, max_bytes)` (byte-capped, never splits a UTF-8 char, appends a
truncation notice), `enforce_max_size`, `refuse_binary` (NUL byte in the
first 8 KiB — the same heuristic `file`/`grep -I` use).

`AgentId` lives in `mate-tool-api` (`ids.rs`), not `mate-core`, purely
because `ToolCtx` and `ToolActivity` both need to carry it and this crate can
never depend on `mate-core` (architecture.md's dependency graph). `SessionId`
stays out until something in this crate actually needs it.

## `mate-tool-fs` — `read_file`, `list_dir`, `find_files`, `write_file`

All four are `rig::tool::PortableTool` impls (not `Tool` — see
`refs/rig.md` for why that distinction matters), attached unconditionally by
`mate-core::toolset::build_toolset` (nothing in `AgentSpec` disables
filesystem access).

| Tool | Behavior | Cap |
|---|---|---|
| `read_file` | Line-numbered contents, optional `start_line`/`end_line` (clamped, not errored, at both ends). Refuses oversized/binary content rather than truncating — there's no line-numbered rendering of half a PNG. | `max_output_bytes`; 8 KiB NUL probe |
| `list_dir` | One level, `.gitignore`-aware via `ignore::WalkBuilder` (`require_git(false)`, so it applies even outside an actual git repo), directories suffixed `/` | `ENTRY_CAP = 200` |
| `find_files` | Glob within root via `ignore::overrides::OverrideBuilder`; rejects (doesn't clamp) a pattern with `..` segments or an absolute path | `RESULT_CAP = 200` |
| `write_file` | Creates or overwrites a file with full content, jailed through `ToolCtx::resolve_for_write` (the containing directory must already exist; the target itself need not). Gated on `ToolCtx::approvals` before every write — refuses outright (`ToolFailure::Denied`) if no approval channel is wired up, rather than writing unattended. | `max_output_bytes` on `content` |

**`ToolActivity::FileTouched` semantics per tool** — worth knowing before you
change what a tool reports, since the panel's document log dedupes by
`path`:

- `read_file` reports the **whole file's** lines/bytes, not the requested
  slice — so two different line-range reads of the same file coalesce into
  one document-log row instead of each slice looking like a different file
  size.
- `list_dir` reports the directory path, entry count as `lines`, rendered
  listing size as `bytes`.
- `find_files` reports the **glob pattern itself** as `path` (not a real,
  resolvable path — this is a deliberate exception, so a glob call still
  gets one document-log row), match count as `lines`.
- `write_file` reports `FileOp::Create` for a not-yet-existing target,
  `FileOp::Write` for an overwrite — `lines`/`bytes` describe the content
  just written. This is the first real producer of `FileOp::Write`/`Create`;
  `Delete` still has none.

Both `list_dir` and `find_files` walk via `tokio::task::spawn_blocking`
(`ignore::WalkBuilder` has no async API).

### `write_file`'s approval gate and "always allow" scope

`write_file` is the first tool anywhere in the workspace that actually calls
`ToolCtx::approvals::request` — every prior tool left the seam (`M13-1`)
unused. Each call sends an `ApprovalRequest { agent, name, detail, path }`,
where `path` is the write's resolved, in-jail target; `request` blocks until
a human answers or `SessionApprovalHub`'s 5-minute timeout auto-denies it
(`crate::approval`, `mate-core`).

`SessionApprovalHub` also remembers scope: a granted `resolve(id, granted,
remember)` with `remember: Some(dir)` adds `dir` to a per-hub, per-session
`always_allowed` set, and any later `request` whose `path` falls under a
remembered directory is granted immediately — no event, no prompt. This is
what `mate-tui`'s approval modal's `Always Allow` option (`M13-6`; alongside
`Allow` and `Disallow`, cycled with `↑`/`↓` and confirmed with `Enter` —
`Esc` is a shortcut straight to `Disallow`) drives: it remembers the
request's target's **parent** directory, never the whole workspace root.
The scope lives only as long as the hub — one session, never persisted to
disk, never covering a sibling directory it wasn't explicitly granted for.

## `mate-tool-http` — `http_request`, SSRF hardening

The whole point of this crate is that a model asked to fetch a URL can be
talked into reading `http://169.254.169.254/` (cloud metadata) or an internal
service, and a subagent doing the fetching means the injected instruction
lands on an agent whose output is *less* likely to get read closely — so
every guard below applies identically to every agent, root or subagent.

### `HttpShared` (`shared.rs`) — the process-wide state

Built once (by `mate-cli` / test setup) and handed down as `Arc<HttpShared>`
to every `HttpRequest` tool instance, session, and subagent runner, the same
way `Backend` is (`providers.md` §5.3). Holds:

- One `hickory_resolver::TokioResolver`.
- One per-host `governor::DefaultDirectRateLimiter`, created lazily in a
  `DashMap<String, Arc<_>>` and reused for the process's lifetime — this is
  *why* the limiter must live here and not on the tool: four tabs × three
  subagents is twelve agents that would each hammer one host at 12× the
  configured rate if the limiter were per-instance.
- `HttpLimits` (`limits.rs`): `connect_timeout` (5s), `total_timeout` (30s),
  `max_response_bytes` (2 MiB), `max_redirects` (5) — fixed, not exposed
  through `[http]` config, since they're the same for every agent.

`resolve_validated(host, allow_localhost)` resolves via DNS (or accepts a
literal IP directly) and returns the *first* candidate address that clears
`ip_guard::blocked_reason` — deliberately not requiring every resolved
address to be safe, since only the one address actually pinned matters.
`pinned_client(host, addr)` then builds a one-off `reqwest::Client` with that
exact `SocketAddr` pinned via `ClientBuilder::resolve(host, addr)` and
`redirect::Policy::none()`. **This pin is the whole DNS-rebinding defense**:
validating an address and then letting `reqwest` re-resolve independently
would be a TOCTOU hole — the socket that opens is the one that was checked,
full stop.

### `ip_guard.rs` — the pure range checks

`blocked_reason(ip, allow_localhost) -> Option<&'static str>` against fixed
IPv4 (`unspecified`, `private` RFC1918, `cgnat` RFC6598, `loopback`,
`link-local` incl. `169.254.169.254`, `multicast`) and IPv6 (`unspecified`,
`loopback`, `private` `fc00::/7`, `link-local` `fe80::/10`, `multicast`)
blocklists. An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is unwrapped and
re-checked against the v4 table — otherwise a resolver answering with the
mapped form walks straight past it. `allow_localhost` (the
`--http-allow-localhost` flag) lifts **only** the loopback block; every other
category stays blocked regardless — the flag is for "I run a dev server on
127.0.0.1", not "disable SSRF protection". No network, no config: pure
`IpAddr` functions, exhaustively table-tested.

### The manual redirect loop (`http_request.rs`)

`HttpShared`'s client is built with `redirect::Policy::none()` specifically
so `http_request.rs` can re-run the **whole** validation pipeline — scheme,
DNS resolution, IP-range check — on every hop's URL, not just the first. A
public host redirecting to `169.254.169.254` fails at `resolve_validated` the
moment the loop tries to follow that hop, before any connection to it is
attempted. Hitting `max_redirects` stops following further hops and returns
whatever response is in hand (typically still a redirect) with the hop count
in the output, rather than erroring — the model can see it didn't reach a
final page.

### Other guards

- **Method gating** (`http_request.rs`) — only `GET`/`HEAD` run unattended;
  everything else is refused outright with `ToolFailure::Denied`, since no
  approval flow exists yet to route mutating methods through (`ToolCtx.
  approvals` is `M13`).
- **Header hygiene** (`headers.rs`) — `Authorization`/`Cookie`/
  `Proxy-Authorization` are refused with a named reason (not silently
  stripped) if the model tries to set them; hop-by-hop/connection headers
  (`Connection`, `Host`, `Transfer-Encoding`, …) are silently dropped, since
  setting those is a confused model, not an attack.
- **Response caps and content routing** (`http_request.rs`, `render.rs`) —
  the body is streamed chunk-by-chunk and aborted the moment it exceeds
  `max_response_bytes`, never buffered whole first. Content type is checked
  *before* download via `render::is_renderable` (text/* plus json/xml/
  xhtml+xml); anything else is refused by name. A missing `Content-Type`
  header defaults to `text/plain` rather than `application/octet-stream` —
  common on empty bodies (redirect-cap hits, `HEAD` responses), and treating
  it as binary would refuse every such response for a type the server never
  actually declared.
- **Body rendering** (`render.rs`) — HTML via `readability::extractor`
  (falls back to raw `html2text` if extraction fails — a non-article page
  still deserves *some* text) then `html2text::from_read` (100-col wrap);
  JSON pretty-printed and depth-capped at 6 levels (deeper structures
  collapse to a placeholder string, not an error); everything else passes
  through as decoded text. `render_text: false` in the tool args bypasses
  all of it and returns the raw decoded body regardless of type.

### Activity telemetry (`http_request.rs`)

Exactly one `ToolActivity::NetRequest` reaches `ctx.activity` per call:
either the hop that finally blocks (`status: None`, `reason` set) or, if
every hop clears the guard, the final response (`status` set, `reason:
None`). Intermediate *followed* redirect hops get no record of their own —
the network log shows one row per `http_request` call, with `redirects`
naming how many hops it took.

## Testing patterns

- **`mate-tool-fs`**: `tempfile::TempDir` + `dunce::canonicalize`; every test
  builds a bare `ToolCtx` by hand (see any test module's `fn ctx(root)`
  helper) rather than going through `build_toolset`. Cover traversal,
  absolute paths, outward symlinks (`#[cfg(unix)]`), oversized files,
  binaries, denylisted names, and — the thing worth remembering — that
  `ToolActivity` records report the *whole file's* stats even when a request
  only asked for a slice. `write_file.rs`'s own tests build a tiny
  `StubApprovals` (an `AtomicBool` behind `async_trait::async_trait`) rather
  than pulling in `mate-core::approval::SessionApprovalHub` — that would
  invert the dependency graph (§8.1 note 1) — and cover both a granting and
  a denying stub, plus `approvals: None` refusing outright.
- **`mate-tool-http`**: `wiremock` for anything that needs a real HTTP
  response (`crates/mate-tool-http/tests/http_request.rs`); `ip_guard`'s
  range checks and `headers`' hygiene rules are pure functions, table-tested
  directly with no server at all. `HttpShared::with_limits` exists so a
  "response too large" test can shrink `max_response_bytes` instead of
  actually transferring megabytes of fixture data.
- Both crates follow the workspace-wide rule: every `assert_eq!` gets a
  third-argument message stating the fact under test, not just the values.
