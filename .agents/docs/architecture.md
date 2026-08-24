# Architecture

`mate` is a Rust CLI coding agent built on Rig, talking to HuggingFace Inference
Providers (with an OpenAI-compatible fallback path). Binary: `mate`. Crate
prefix: `mate-*`; tool crates: `mate-tool-*`.

## Workspace layout

```
mate/
├── Cargo.toml                  # [workspace]
└── crates/
    ├── mate-cli/               # bin `mate`: args, config, picks frontend
    ├── mate-core/               # agent construction, session manager, subagent runtime
    ├── mate-tool-api/          # shared contracts: ToolCtx, errors, SubagentSpawner
    ├── mate-tool-fs/           # read_file, list_dir, find_files
    ├── mate-tool-http/         # http_request — network access, guarded
    ├── mate-tool-agent/        # spawn_agent — delegation within a session
    └── mate-tui/               # Ratatui frontend: tabs, side panel, transcript
```

## Dependency graph

```
cli ──► tui ──► core ──► rig
                  ├────► mate-tool-fs    ──┐
                  ├────► mate-tool-http  ──┤
                  └────► mate-tool-agent ──┴──► mate-tool-api ──► rig (Tool trait only)
```

`mate-tool-api` must never depend on `mate-core` — that would invert the graph.
Capabilities `mate-core` needs to hand down into a tool crate (approvals,
subagent spawning) are declared as traits in `mate-tool-api` and implemented in
`mate-core`, then injected at construction time.

## Toolchain

Rust `1.97.1` (pinned in `rust-toolchain.toml`, includes `rustfmt` + `clippy`),
edition 2024, workspace resolver `3`.

## Crate status (update as work lands)

| Crate | State |
|---|---|
| `mate-cli` | Args (`cli.rs`), layered config (`config.rs`), tracing setup (`logging.rs`), `MateError` boundary (`error.rs`), the plain frontend (`plain.rs` — `--plain`/`--print`, SIGINT cancellation, `resolve_workspace_roots` for one root per `-C`), and the default frontend's wiring (`tui.rs` — spawns one session per resolved root through `SessionManager`, via `mate_tui`'s own `build_spec`/`build_tool_ctx`, and hands the manager plus every `InitialSession` to `mate_tui::run`) are in place. Both frontends build one process-wide `mate_tool_http::HttpShared` from `config.http.rate_limit_per_host_per_min` and pass it into `build_agent`/`SessionManager::new`. |
| `mate-core` | `Backend` (HF native + OpenAI-compatible fallback), `build_agent`/`BuiltAgent` (now also threading a shared `Arc<mate_tool_http::HttpShared>` alongside `Backend`), preamble rendering, provider error mapping + retry, the streaming/event layer (`streaming.rs`), the session manager (`session.rs` — `SessionId`/`SessionEvent`, per-session Tokio task with threaded history, the shared bounded event channel, `Cancel`/`Shutdown` lifecycle, crash isolation, and `SessionManager::close` for `mate-tui`'s `M8-3` tab-close), the subagent runtime (`subagent.rs`'s `SubagentRunner` — `mate-tool-api::SubagentSpawner` impl covering depth/concurrency/per-turn/wall-clock/turn-budget limits, report rendering and truncation, and cancellation propagated from `ToolCtx::cancel`, now wired as a child of the session's own token in `SessionManager::spawn`), and `approval.rs`'s `SessionApprovalHub` (`mate_tool_api::Approvals` impl, one per session, wired into `ToolCtx::approvals` the same way `cancel`/`activity` are) are in place. The hub auto-denies an unanswered request after 5 minutes, and remembers a per-session "always allow this directory" scope (`SessionCmd::Approve`'s `remember: Option<PathBuf>`) that a later request's `path` is checked against before ever prompting again. `toolset::build_toolset` attaches `http_request` whenever `HttpPolicy::enabled`, narrowed to `HttpAccessPolicy::AllowLocalhost` the same way every other per-agent policy narrows, and attaches `mate-tool-fs`'s `write_file` unconditionally alongside the other filesystem tools. |
| `mate-tool-api` | `ToolCtx` (path jail via `resolve`, plus `resolve_for_write` for a target that may not exist yet), `ToolFailure`, `ToolActivity`/`FileOp`/`ActivitySink`, `Approvals`/`ApprovalRequest` (`agent`/`name`/`detail`/`path`), `SubagentSpawner` + request/report types (`SubagentRequest` carries a `cancel: CancellationToken`), `ToolProfile`'s `TryFrom<Option<&str>>` parser, `AgentId` (moved here from `mate-core` so `ToolCtx` can carry it without inverting the dependency graph), and the `number_lines`/`truncate_with_notice`/`enforce_max_size`/`refuse_binary` helpers are in place. |
| `mate-tool-fs` | `read_file`, `list_dir`, `find_files`, `write_file` (`rig::tool::PortableTool` impls) are in place. `write_file` is the first tool that routes through `ToolCtx::approvals`, refusing outright when none is wired up. |
| `mate-tool-agent` | `spawn_agent` (`rig::tool::PortableTool` impl) is in place — only ever attached to a toolset when `ToolCtx::spawner` is `Some` (`mate-core::toolset::build_toolset`). |
| `mate-tool-http` | `http_request` (`rig::tool::PortableTool` impl, `http_request.rs`) is in place: method gating (GET/HEAD only — no approval flow exists yet to route mutating methods through), header hygiene (`headers.rs` — refuses model-supplied `Authorization`/`Cookie`/`Proxy-Authorization`, drops hop-by-hop headers), a manual redirect loop capped at 5 hops that re-validates scheme/DNS/IP on every hop, a streamed response cap that aborts mid-download, and content-type-gated body rendering (`render.rs` — HTML via `readability` + `html2text`, JSON pretty-printed and depth-capped, `render_text: false` escape hatch). SSRF defenses live in `ip_guard.rs` (pure `IpAddr` range checks: loopback/private/link-local/unspecified/multicast/CGNAT, `--http-allow-localhost` lifts only loopback) and `shared.rs`'s `HttpShared` — the process-wide state (one `hickory-resolver` `TokioResolver`, one per-host `governor` rate limiter map, the pinned-client factory) built once by `mate-cli` and threaded as an `Arc` through `mate-core::agent::build_agent`/`toolset::build_toolset`/`session::SessionManager`/`subagent::SubagentRunner`, the same way `Backend` is shared. |
| `mate-tui` | `M7`: terminal lifecycle (`app.rs::run`, via `ratatui::try_init`/`restore`), the `select!` event loop, transcript model with capped scrollback (`transcript.rs`), a per-entry wrap cache (`wrap.rs`), and the `ratatui-textarea`-backed input box with `Enter`/`Alt+Enter`/history routing (`input.rs`). `M8`: a tab bar (`ui.rs`'s `tab_bar_segments`, windowed and unit-tested against plain strings rather than `TestBackend` grids), per-tab view state (`SessionTab` in `app.rs` — transcript/wrap/input/scroll/streaming all moved off `App` onto one struct per tab), `Ctrl+T`'s spawn form plus `session_factory.rs` (the `SessionSpec`/`ToolCtx` assembly `mate-cli` also uses for the first tab(s)), `Ctrl+W` close with a streaming confirm (`SessionManager::close`, `mate-core`), unread/needs-attention markers plus `Ctrl+G` (`M8-4`), and one tab per `-C` path at startup (`M8-5`). `M12`: the agent status panel — `Ctrl+B` toggle, `Ctrl+P`/`Tab`/arrows navigation, five widgets (model, context+cost, subagent roster, network log, documents log) with a vertical-budget allocator that collapses documents before network before subagents. See `panel.md`. `M13`: the approval modal — `y` grants once, `a` grants and remembers the request's target's parent directory as "always allow" for the rest of the session, `n`/`Esc` denies; a queued request takes priority over every other key while open. Uses `ratatui-textarea`, not `tui-textarea` — that crate's `Widget` impl targets `ratatui` 0.29 and can't render into this workspace's `ratatui` 0.30. |

`mate-core`'s subagent runtime (`subagent.rs`'s `SubagentRunner`, `M9`) and
`mate-tool-agent`'s `spawn_agent` tool are landed — depth/concurrency/
per-turn/wall-clock guardrails, cancellation propagation, and the
non-addressability invariant all have real producers and tests now. See
`delegation.md` for the whole seam. `mate-tool-http`'s `http_request` (`M10`)
and the `ToolActivity`/`ActivitySink` telemetry path (`M11`) are also landed
— see `tools.md`.

For what each of those pieces actually does, see the other docs in this
directory: `config.md`, `logging.md`, `error-handling.md`, `providers.md`,
`streaming.md`, `tools.md`, `delegation.md`, `panel.md`, `testing.md`.

## Existing infra — don't duplicate

- `Justfile` — `fmt`, `test`, `deny` recipes already defined; treat as the
  reference for what CI will eventually run.
- `deny.toml` — licence allowlist and advisory ignores already configured.
- `rust-toolchain.toml` — pins toolchain + components.
- `CONTRIBUTING.md` — full write-up of the error-handling strategy.
