# Testing

## Per-area conventions

- **Filesystem tools** (`mate-tool-fs`, `M3`): test over `tempfile::TempDir` —
  traversal, absolute paths, outward symlinks, oversized files, binaries,
  denylisted names.
- **HTTP tool** (`mate-tool-http`, not yet landed): test over `wiremock` —
  redirect chains, oversized bodies, redirect loops, header stripping,
  DNS-rebinding-style cases.
- **Session manager** (`mate-core/src/session.rs`, landed): drive
  `session_task` directly against Rig's mock model and mock tools (e.g.
  `MockControlledTool` for a genuine in-flight await point, a local panicking
  `Tool` for crash isolation) — no `Backend`/network needed except the one
  offline-construction test that exercises `SessionManager::spawn`'s
  `max_sessions` cap.
- **Subagent runtime** (`mate-core/src/subagent.rs`, landed): the guardrail gate
  (depth, per-turn cap, concurrency-queues-not-rejects) is split into
  `SubagentRunner::acquire` and tested directly — concurrent `tokio::spawn`
  tasks racing on a real `Semaphore`, no agent ever built. The turn-driving
  half (`drive_subagent`) is split out generic over the completion model and
  tested against Rig's mock models — report rendering/truncation, wall-clock
  timeout, and cancellation propagation all drive real (mock) turns with no
  `Backend`/network involved. `SubagentSpawner::run` itself (the guardrails +
  real `Backend`/`build_agent` glue) only gets offline-constructible-`Backend`
  coverage for the same reason `SessionManager::spawn`'s does.
- **Agent/provider behavior**: Rig ships mock models and VCR-style cassette
  tests; record a scenario per case (tool call, error recovery, multi-turn)
  and replay in CI rather than hitting a live provider.
- **Streaming/event mapping** (`mate-core/src/streaming.rs`, landed): build
  synthetic `MultiTurnStreamItem` streams directly — see `streaming.md`'s
  testing section. No mock model needed for the event-mapping layer itself.
- **TUI** (`mate-tui`, `M7`/`M8`): `ui.rs` (rendering: layout, tab bar, status bar, transcript/
  input widgets) has no test module — `TestBackend`/`insta` snapshot tests, and unit tests
  against `tab_bar_segments`'s plain-string output, were removed as not worth their upkeep;
  don't add rendering tests back there. App-level routing (key handling, session-event-to-tab
  routing, spawn/close bookkeeping) is still tested in `app.rs` without a terminal, using
  `Backend::huggingface`'s offline construction the same way `mate-core/src/session.rs`'s own
  tests do — a real `SessionManager::spawn` builds an `Agent` value with no network I/O, so `App`
  tests can spawn real tabs and exercise `switch_to`, `on_session_event`, `close_tab`, and
  `submit_spawn_form` without a mock model or a live provider.
- **Config precedence** (landed): `figment::Jail`, one test per layer — see
  `config.md`.

## Live-network tests

Exactly one kind is allowed to touch a real provider:
`crates/mate-core/tests/hf_backend.rs`, marked `#[ignore]`, run manually with
a real `API_TOKEN`. **Never let a live-network test run in CI** — everything
CI runs must be fully offline/mocked.

## CI (not yet wired — `Justfile` is the reference)

The pipeline, once built, must run:

- `cargo fmt --check`
- `cargo clippy --all-targets -D warnings`
- `cargo nextest run --workspace` (per `Justfile`)
- `cargo deny check` (config already in `deny.toml`)

Done when: green on a PR, fails on an introduced warning. Use the `Justfile`
recipes as the reference for what each step actually runs — don't
reimplement them inline in a CI config.
