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
- **Subagent runtime** (not yet landed): test headless, against Rig's mock
  models — no TUI needed to exercise concurrency/cancellation bugs.
- **Agent/provider behavior**: Rig ships mock models and VCR-style cassette
  tests; record a scenario per case (tool call, error recovery, multi-turn)
  and replay in CI rather than hitting a live provider.
- **Streaming/event mapping** (`mate-core/src/streaming.rs`, landed): build
  synthetic `MultiTurnStreamItem` streams directly — see `streaming.md`'s
  testing section. No mock model needed for the event-mapping layer itself.
- **TUI** (not yet landed): `ratatui::TestBackend` + `insta` snapshots.
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
