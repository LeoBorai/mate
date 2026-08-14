# AGENTS.md

## Hard rule

**Never run `cargo` commands** (`cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`,
`cargo deny`, `cargo nextest`, etc.) directly in this environment. Use the `Justfile` recipes
as reference for what CI runs, but do not execute them yourself. If verification is needed,
ask the user to run it or rely on CI.

**Never reference `plan.md`, or any other planning doc supplied out-of-band, from code, doc
comments, commit messages, or committed docs (`AGENTS.md`, `CONTRIBUTING.md`, etc.).** Such
files are working input, not part of the repo (`plan.md` is `.gitignore`d) — anyone without
the original loses the referent, and the pointer rots the moment a section renumbers. Milestone
tags (`M0-5`) and `§N` section numbers are fine as an internal, self-contained convention, since
this doc defines them itself; "per the plan", "`plan.md`", or any wording that implies a reader
needs an external doc to make sense of a comment is not.

## Project context

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

### M0-4 · Tracing setup (§11, done)

`tracing-subscriber` (`env-filter` feature) + `tracing-appender` in `mate-cli/src/logging.rs`.
`logging::init()` is called first thing in `main()`, before `Cli::parse()` / `config::load()`,
and returns a `WorkerGuard` that must stay alive for the process lifetime (the writer is
non-blocking; dropping the guard early loses buffered lines).

Log file: `$XDG_STATE_HOME/mate/mate.log`, falling back to `~/.local/state/mate/mate.log`
(mirrors the `XDG_CONFIG_HOME` pattern already used in `mate-cli/src/config.rs`). Level from
`RUST_LOG`, default `info`. Writer is the file only — never stdout/stderr, in either `--plain`
or TUI mode.

`SessionId` / `AgentId` don't exist until the session manager (§5, M6); once they do, spans
opened anywhere in `mate-core` should carry them as fields — add enum/span keys early rather
than retrofitting later (§11).

Covered by `crates/mate-cli/tests/tracing_stdout.rs` (spawns the built binary, asserts empty
stdout and a populated log file) and unit tests in `logging.rs` for state-dir resolution.

### M0-5 · Error strategy (§16, done)

Three layers, documented in full in `CONTRIBUTING.md`:

- Libraries (`mate-core`, `mate-tool-*`, `mate-tui`) return `thiserror` enums, never
  `anyhow::Error` — callers need a matchable type. Tool crates converge on `ToolFailure`
  (`M3-1`, not yet landed) which is deliberately non-fatal: fed back to the model as a tool
  result, not a turn abort.
- `mate-cli` plumbing (`config.rs`, `logging.rs`) stays `anyhow::Result` with
  `.context(...)` — the binary edge, human-readable trail over a matchable type.
- `mate-cli/src/error.rs` has `MateError`, the one error type at the CLI boundary.
  `main.rs`'s `run()` maps each fallible call to a variant (`logging::init().map_err(MateError::Io)?`,
  `config::load(&args).map_err(MateError::Config)?`); `main()` prints the error plus its
  `source()` chain and converts `MateError::exit_code()` to the process `ExitCode`. Codes:
  `Config` = 2, `Io` = 3, `Other` = 1, success = 0. `Auth`/`Provider` (codes 4/5, kept free) get
  added as variants once `M1-5`/`M5-4` give them a real producer, not before — an unconstructed
  variant is dead code under `clippy -D warnings`.

### M1-1 · Backend and HF client (§4, done)

`mate-core/src/backend.rs`: `Backend` wraps Rig's HuggingFace `Client` (`rig::providers::huggingface`,
itself a `client::Client<Ext, H>` instantiation — the generic client machinery Rig ships, of
which HuggingFace is one `Ext`). `Backend::new(api_key, sub_provider)` takes the token as a
value, never reads the environment itself; `mate-cli::config::api_token()` stays the one place
`API_TOKEN` is read. `sub_provider` is a free-text `Option<&str>` mapped to Rig's `SubProvider`
enum (`together`, `fireworks`, `sambanova`, `hyperbolic`, `nebius`, `novita`, else
`Custom`/`hf-inference`) — an unrecognized name falls back to `hf-inference` rather than
erroring, since the partner list moves independently of `mate`.

`Backend` is named and shaped for `base_url` override + the OpenAI-compatible fallback
(`M1-3`) to slot in later without a rename; that generic provider-selection path is that
ticket's job, not this one's.

**✓** `crates/mate-core/tests/hf_backend.rs` — `#[ignore]`d, needs a real `API_TOKEN`; calls
Rig's `VerifyClient::verify()` against the live router. Offline unit tests in `backend.rs`
cover `sub_provider` name mapping and client construction (construction alone never hits the
network — only `.verify()` does).

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
