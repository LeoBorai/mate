# AGENTS.md

## Hard rule

**Never run `cargo` commands** (`cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`,
`cargo deny`, `cargo nextest`, etc.) directly in this environment. Use the `Justfile` recipes
as reference for what CI runs, but do not execute them yourself. If verification is needed,
ask the user to run it or rely on CI.

## Project context (from `plan.md`)

`mate` — Rust CLI coding agent built on Rig + HuggingFace.

### Workspace layout (§3)

```
mate/
├── Cargo.toml                  # [workspace]
└── crates/
    ├── mate-cli/               # bin `mate`: args, config, picks frontend
    ├── mate-core/              # agent construction, session manager, subagent runtime
    ├── mate-tool-api/          # shared contracts: ToolCtx, errors, SubagentSpawner
    ├── mate-tool-fs/           # read_file, list_dir, find_files
    ├── mate-tool-http/         # http_request — network access, guarded
    ├── mate-tool-agent/        # spawn_agent — delegation within a session
    └── mate-tui/               # Ratatui frontend: tabs, side panel, transcript
```

Dependency graph: `cli ─► tui ─► core ─► rig`, with `core` also depending on
`mate-tool-fs` / `mate-tool-http` / `mate-tool-agent`, all of which depend on
`mate-tool-api` (never the reverse — `mate-tool-api` must not depend on `mate-core`).

Toolchain: Rust `1.97.1` (`rust-toolchain.toml`), edition 2024, workspace resolver `3`.

### M0-2 · CI pipeline (§15, backlog)

Infra ticket. Pipeline must run:
- `fmt --check`
- `clippy --all-targets -D warnings`
- `test --workspace` (via `cargo nextest run`, per `Justfile`)
- `cargo-deny` for licences and advisories (config already in `deny.toml`)

**Done when:** green on a PR; fails on an introduced warning. Depends on `M0-1 · Workspace
scaffold`.

### M0-3 · Config loading (§10, done)

`figment` layering in `mate-cli/src/config.rs`: flags → env (`MATE_*`) → `./.mate.toml` (or
`--config`) → `~/.config/mate/config.toml` → defaults. `Config`, `ToolsConfig`, `PanelConfig`,
`PricingEntry` live there; `DelegationPolicy`, `HttpPolicy`, `HttpAccessPolicy`, `AgentSpec`,
`SessionSpec` live in `mate-core/src/config.rs` (shared shape, per §4/§5.1/§7.4).

**`API_TOKEN`** (the API-provider token — HuggingFace or otherwise) is env-only: read via
`config::api_token()`, never a `Config` field, never round-trips through a config file even if
one sets the key. Don't add it to `Config` — that reopens the leak this ticket closed.

`figment::Jail` (dev-dependency, `test` feature) drives the precedence tests, one per layer,
in `mate-cli/src/config.rs`. `Jail::create_file` does **not** create parent directories —
`std::fs::create_dir_all` first for any nested path (e.g. `.config/mate/config.toml`).

### Testing conventions (§12)

- Tools tested over `tempfile::TempDir` (`mate-tool-fs`) and `wiremock` (`mate-tool-http`).
- Subagent runtime tested headless against Rig's mock models.
- Agent behavior: Rig mock models + VCR cassette tests, replayed in CI.
- TUI: `TestBackend` + `insta` snapshots.
- One `#[ignore]`d integration test against the live HF router, run manually only — must not
  run in CI.

### Existing infra (do not duplicate)

- `Justfile` — `fmt`, `test`, `deny` recipes already defined.
- `deny.toml` — licence allowlist and advisory ignores already configured.
- `rust-toolchain.toml` — pins toolchain + components (`rustfmt`, `clippy`).
- `mate-cli/src/config.rs` + `mate-core/src/config.rs` — config loading (M0-3, above). Extend,
  don't reimplement.
