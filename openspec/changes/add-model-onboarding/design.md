# Design

## Context

See `proposal.md - Why` for motivation. Relevant current state:

- `crates/mate-cli/src/tui.rs::run` and `crates/mate-cli/src/plain.rs::run` both do, eagerly, in this order: `api_token()` (env-only, hard-fails if `None`) → `build_backend` (mate-cli-owned, matches `config.backend: BackendKind` to `Backend::huggingface`/`Backend::gemini`) → `backend.verify().await` → build `SessionManager`/`HttpShared`/`McpServers` → spawn one session per `-C` root → hand off to `mate_tui::run` (TUI) or the plain loop.
- `BackendKind` and `build_backend` live in `mate-cli` (`config.rs`, `plain.rs`), not `mate-core`, because `BackendKind` derives `clap::ValueEnum` and `mate-core` takes no `clap` dependency.
- `mate-tui`'s real (non-dev) dependencies are `mate-core`, `mate-tool-api`, `mate-tool-skills`, plus UI crates — notably *not* `mate-tool-http` or `mate-tool-mcp` (those are dev-dependencies only, used in its own tests). Building a `Backend`/`SessionManager`/`HttpShared`/`McpServers` is mate-cli's job today; `mate-tui::run` only ever receives already-built sessions via `InitialSession`.
- `mate-tui::app` already has a working modal pattern to extend: `SpawnForm` (Ctrl+T: text fields + a toggle, `SpawnField` focus-cycling, char-level `push_char`/`backspace`) and `DetailModal`/`ApprovalModal` (read-only popups). No fuzzy search or autocomplete exists anywhere in the TUI today.
- `rig`'s Gemini provider has a `ModelLister` (`GET /v1beta/models`, needs an API key already on the client); its HuggingFace provider has no model-listing capability at all. Both facts rule out fetching the catalog live during onboarding (`proposal.md`).
- The models.dev repo (`anomalyco/models.dev`, `dev` branch) lays out data as `providers/<provider>/provider.toml` (name, env var, api base url) + `providers/<provider>/models/<org>/<model>.toml` (per-provider `[cost]` block: `input`/`output`, exact shape of `PricingEntry`) + `models/<org>/<model>.toml` (shared metadata: context/output limits, display name, description). Its root `models.json` is unrelated OpenRouter-schema sync data and is not used.

## Goals / Non-Goals

**Goals:**
- Let the TUI render and reach an onboarding flow with zero `API_TOKEN` in place, using a static catalog to drive the model picker.
- Keep the "already have a token" startup path byte-for-byte the same as today — same functions, same order, same behavior — so this change adds a new path rather than modifying the existing one.
- Keep `mate-tui` free of new production dependencies on `mate-tool-http`/`mate-tool-mcp`; backend/session construction stays mate-cli's responsibility.

**Non-Goals:**
- No fuzzy search, autocomplete, or filtering-as-you-type in the model picker — a plain scrollable list, consistent with the complexity level of every existing modal (`SpawnForm`, `DetailModal`).
- No attempt to make the HuggingFace catalog exhaustive. models.dev's HF coverage (a curated set of orgs/models, not HF's full router catalog) is accepted as-is; the existing free-text `--model`/`Config.model` override remains the escape hatch for anything not listed.
- No CI automation to keep the vendored catalog fresh. Regeneration is a manual, on-demand script run by a maintainer.
- No mid-session `/model`/`/login` slash command. Onboarding is a startup-only gate (see proposal's "Modified Capabilities"/"Startup gate only" decision).

## Decisions

### 1. Catalog lives in `mate-core`, as a small hand-rolled enum + plain data, not JSON parsed at runtime
A new `mate-core::model_catalog` module holds:
- `CatalogBackend { Huggingface, Gemini }` — a minimal 2-variant enum, deliberately *not* the same type as `mate-cli::config::BackendKind` (which derives `clap::ValueEnum` and would pull `clap` into `mate-core`). The one conversion point (mate-cli's onboarding-completion closure, see Decision 2) does a trivial 2-arm match between them.
- `ModelEntry { id: &'static str, display_name: &'static str, backend: CatalogBackend, pricing: Option<PricingEntry> }` (or equivalent), and a lookup function filtering by `CatalogBackend`.
- The actual entries live in a *generated* Rust source file (e.g. `model_catalog/generated.rs`, a `const CATALOG: &[ModelEntry] = &[...]`), committed to the repo and re-emitted by the regeneration script — not `include_str!`'d JSON/TOML parsed at startup. This keeps runtime cost at zero and needs no new runtime parsing dependency in `mate-core`.

**Alternative considered**: embed the vendored TOML/JSON as a string asset and parse it with `serde`/`toml` at first use. Rejected — `mate-core` already depends on `serde`, so it's not a new dependency, but a generated `.rs` file is simpler to diff in review, needs no `OnceLock`/lazy-parse machinery, and can't fail to parse at runtime.

**Alternative considered**: move `BackendKind` itself into `mate-core` and have `mate-cli` re-export it, so there's only one enum. Rejected — it would force a `clap` dependency onto `mate-core`, a library crate the whole workspace (including `mate-tui`) depends on, for the sake of one enum's derive.

### 2. Onboarding orchestration lives in `mate-tui`; backend/session construction stays a mate-cli-owned closure
`mate_tui::run` (or a new sibling entry point) is restructured to accept either already-built sessions (today's path, unchanged) or a "needs onboarding" input carrying: workspace roots, a `SessionDefaults` template, the catalog, and a boxed async closure supplied by `mate-cli`:

```
FnOnce(CatalogBackend, model: String, token: String)
    -> BoxFuture<Result<(SessionManager, mpsc::Receiver<SessionEvent>, Vec<InitialSession>, HashMap<String, ModelRate>), MateError>>
```

`mate-cli`'s `tui::run` keeps owning `build_backend`, `backend.verify()`, `HttpShared`/`McpServers` construction, and the per-root session-spawn loop — it just moves that logic into a closure instead of running it unconditionally up front. When `API_TOKEN` is already set, `tui::run` calls the closure itself immediately (today's exact code path, unchanged) and calls `mate_tui::run` with sessions already built, exactly as today. When it's unset, `tui::run` hands the closure to `mate_tui::run` uncalled; `mate-tui`'s onboarding flow calls it once the user finishes the backend → model → token steps and the token verifies.

**As built** (refinements found while implementing, same shape as above): the closure is `Fn`, not `FnOnce`, because a failed token can be corrected and retried; it returns `Result<StartedSessions, String>` (`MateError` is a `mate-cli` type `mate-tui` can't name) and its future is a `LocalBoxFuture`. `StartedSessions` also carries the `SessionDefaults`, since the chosen backend/model (and the subagent-model default that follows the backend) must reach the `Ctrl+T` spawn form too — so the `SessionDefaults` template is built inside the closure rather than passed in, and the workspace roots are captured by it. Onboarding runs as its own screen before `App` exists (`mate_tui::run_with_onboarding`), so `App` gains no "backend not yet built" state and `mate_tui::run` is unchanged.

**Alternative considered**: move `build_backend`/`HttpShared`/`McpServers` wiring down into `mate-core` so `mate-tui` could call it directly with no closure. Rejected — `HttpShared`/`McpServers` construction from `Config`/`Cli` is mate-cli-specific glue (per `mcp_server_specs`'s own doc comment, `mate-tool-mcp` can never depend on `mate-core`), and pulling it into `mate-core` or making it a real (non-dev) `mate-tui` dependency contradicts this change's own Goal of keeping `mate-tui` decoupled from those crates.

### 3. Token representation: plain `String`, in-memory only, dropped after use
The onboarding-entered token is held as an ordinary `String` in the onboarding modal's state (same as `SpawnForm.model` today), passed by value into the completion closure, and never touches `Config`, figment, or any file write. This is the same invariant `config.rs`'s `api_token_is_env_only_and_not_a_config_field` test already proves for the env-var path; tasks.md should add an equivalent test (or an assertion in the onboarding module) proving no serialization path exists for the onboarding-entered token either.

### 4. Onboarding UI: three sequential steps, reusing `SpawnForm`'s interaction style
One new modal type, structurally similar to `SpawnForm`: a `focus`-cycling step index (`Backend`, `Model`, `Token`) rather than free navigation between them, since the model list depends on the backend choice and the token step only makes sense once a model is chosen. Backend and model steps are selectable lists (`↑`/`↓` + `Enter`), the token step is a masked text input (existing `push_char`/`backspace` pattern, rendered as `*`s). A failed `verify()` shows an inline error (matching `SpawnForm.error`) and returns focus to the token step without discarding the chosen backend/model.

### 5. Regeneration script: a small workspace member, invoked via `just`
A new binary (e.g. `crates/xtask-model-catalog` or a top-level `xtask/`) fetches `providers/huggingface/**`, `providers/google/**`, and the referenced `models/**` files from a **pinned commit/tag** of `anomalyco/models.dev` (not the `dev` branch's moving HEAD, so reruns are deterministic and reviewable as a diff) and overwrites `model_catalog/generated.rs`. A `just gen-model-catalog` recipe is added to the existing `Justfile` alongside `fmt`/`test`/`deny`, matching this repo's established task-runner convention. This binary is a dev-time tool only — it is not part of the default `cargo build` graph beyond being a normal workspace member (compiled in `cargo build --workspace`, never invoked by it).

## Risks / Trade-offs

- **[Risk]** Static catalog goes stale as new HF/Gemini models ship → **[Mitigation]** regeneration is a one-command, on-demand script (Decision 5); staying current is a manual maintenance task, not automated in this change.
- **[Risk]** `anomalyco/models.dev` is a third-party (possibly fork of the canonical `sst`/`models.dev`) community repo that could rename, go private, or diverge → **[Mitigation]** the script pins a specific commit/tag rather than tracking `dev` HEAD, so a broken or vanished upstream only affects the *next* regeneration, never an existing vendored catalog or a build.
- **[Risk]** models.dev's HF coverage is a curated subset, not HF's full router catalog → **[Mitigation]** explicitly a Non-Goal; the existing free-text model override (`--model`, `Config.model`) stays available for anything not listed.
- **[Trade-off]** The closure-based inversion of control (Decision 2) is more indirection than a direct function call → accepted, since the alternative (collapsing crate layering) contradicts an explicit Goal and would ripple into `mate-tool-mcp`'s own documented constraints.

## Migration Plan

No data migration — this is a purely additive feature (new module, new modal, new script) plus one behavior change (TUI no longer exits on missing `API_TOKEN`). Ship as a normal PR; no feature flag, no rollback beyond a normal revert.
