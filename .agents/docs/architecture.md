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
| `mate-cli` | Args (`cli.rs`), layered config (`config.rs`), tracing setup (`logging.rs`), `MateError` boundary (`error.rs`), and the plain frontend (`plain.rs` — `--plain`/`--print`, SIGINT cancellation) all in place. The tabbed TUI (`M7`) is not started, so any other invocation shape errors out. |
| `mate-core` | `Backend` (HF native + OpenAI-compatible fallback), `build_agent`/`BuiltAgent`, preamble rendering, provider error mapping + retry, the streaming/event layer (`streaming.rs`), and the session manager (`session.rs` — `SessionId`/`SessionEvent`, per-session Tokio task with threaded history, the shared bounded event channel, `Cancel`/`Shutdown` lifecycle, crash isolation) are in place. Subagent runtime is not started. |
| `mate-tool-api` | `ToolCtx` (path jail via `resolve`), `ToolFailure`, `ToolActivity`/`FileOp`/`ActivitySink`, `SubagentSpawner` + request/report types, `AgentId` (moved here from `mate-core` so `ToolCtx` can carry it without inverting the dependency graph), and the `number_lines`/`truncate_with_notice`/`enforce_max_size`/`refuse_binary` helpers are in place. |
| `mate-tool-fs` | `read_file`, `list_dir`, `find_files` (`rig::tool::PortableTool` impls) are in place. |
| `mate-tool-http`, `mate-tool-agent` | Stub crates — no types yet. |
| `mate-tui` | Stub crate — no types yet. |

For what each of those pieces actually does, see the other docs in this
directory: `config.md`, `logging.md`, `error-handling.md`, `providers.md`,
`streaming.md`, `testing.md`.

## Existing infra — don't duplicate

- `Justfile` — `fmt`, `test`, `deny` recipes already defined; treat as the
  reference for what CI will eventually run.
- `deny.toml` — licence allowlist and advisory ignores already configured.
- `rust-toolchain.toml` — pins toolchain + components.
- `CONTRIBUTING.md` — full write-up of the error-handling strategy.
