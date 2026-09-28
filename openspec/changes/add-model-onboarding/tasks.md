# Tasks

## 1. Model catalog data pipeline

- [ ] 1.1 Add a new workspace member (e.g. `crates/xtask-model-catalog`) that fetches `providers/huggingface/**`, `providers/google/**`, and their referenced `models/<org>/<model>.toml` files from a pinned commit/tag of `anomalyco/models.dev`, and verify it runs via `cargo run -p xtask-model-catalog` (or equivalent) against the pinned ref with no other input.
- [ ] 1.2 Add a `just gen-model-catalog` recipe wired to that binary, and verify `just gen-model-catalog` regenerates the file with no manual steps.
- [ ] 1.3 Implement generation of `model_catalog/generated.rs` (a `const CATALOG: &[ModelEntry] = &[...]`) from the fetched TOML data, mapping each provider's `[cost]` block to a `PricingEntry`-shaped pair, and verify the generated file compiles as part of `cargo build --workspace`.
- [ ] 1.4 Run the generator once against the pinned ref and commit the resulting `model_catalog/generated.rs`, and verify `cargo build --workspace` succeeds with network access disabled.

## 2. `mate-core` catalog module

- [x] 2.1 Add `mate-core::model_catalog` with `CatalogBackend { Huggingface, Gemini }` and `ModelEntry { id, display_name, backend, pricing }`, and verify it compiles with no new runtime dependency added to `mate-core`.
- [x] 2.2 Add a lookup function filtering `CATALOG` by `CatalogBackend`, and verify a unit test asserts every returned entry's `backend` matches the requested filter.
- [x] 2.3 Add a unit test asserting every catalog entry's `id` is non-empty and every `Huggingface` entry's `id` contains a `/` (the `Org/Model` shape `Backend::huggingface`/`Config.model` already expect).

## 3. Onboarding-completion closure in `mate-cli`

- [x] 3.1 Extract the existing token-check → `build_backend` → `verify()` → `HttpShared`/`McpServers` → per-root session-spawn sequence in `crates/mate-cli/src/tui.rs::run` into a closure matching design.md's Decision 2 signature, and verify the existing token-already-set path still produces identical `InitialSession`s (existing tests continue to pass unmodified).
- [x] 3.2 Add a 2-arm mapping from `mate_core::model_catalog::CatalogBackend` to `mate_cli::config::BackendKind`, and verify a unit test covers both variants.
- [x] 3.3 Wire `tui::run` to call the closure immediately (today's behavior, unchanged) when `api_token()` returns `Some`, and verify `cargo test -p mate-cli` still passes with no behavior change on this path.

## 4. Onboarding modal in `mate-tui`

- [x] 4.1 Add an onboarding modal type (backend step, model step, token step) following `SpawnForm`'s focus-cycling/char-input pattern, and verify a unit test drives all three steps via simulated key events and reaches a completed state with a chosen backend, model id, and token string.
- [x] 4.2 Render the model step's list filtered to the chosen backend using `mate_core::model_catalog`'s lookup, and verify a unit test asserts the rendered list changes when the backend step's selection changes.
- [x] 4.3 Restructure `mate_tui::run`'s entry to accept either already-built sessions (unchanged) or a pending-onboarding input (roots, `SessionDefaults` template, catalog, completion closure), and verify existing `mate-tui` tests exercising the already-built-sessions path pass unmodified.
- [x] 4.4 On onboarding completion, `await` the completion closure and show its `Err` inline on the token step (matching `SpawnForm.error`) without discarding the chosen backend/model, and verify a test simulates a closure returning `Err` and asserts the backend/model selection is preserved and the token step regains focus.
- [x] 4.5 On success, transition into the normal tabbed session view using the closure's returned sessions, and verify a test simulates a closure returning `Ok` and asserts the resulting tab count matches the workspace roots passed in.

## 5. Wiring `mate-cli`'s startup for the no-token path

- [ ] 5.1 In `tui::run`, when `api_token()` returns `None`, resolve workspace roots and build the `SessionDefaults` template as today but skip the token-check/backend-build/verify steps, and hand the pending-onboarding input (roots, template, catalog, unclaimed closure) to `mate_tui::run`, and verify an integration test (or manual run with `API_TOKEN` unset) shows the TUI render the onboarding modal instead of the process exiting.
- [x] 5.2 Verify `crates/mate-cli/src/plain.rs` is untouched by this change (no diff), matching the proposal's "Plain/print mode is unaffected" requirement.

## 6. Token non-persistence proof

- [x] 6.1 Add a test (alongside `config.rs`'s existing `api_token_is_env_only_and_not_a_config_field`) asserting the onboarding modal's token field is never passed through `figment`, `Config`, or any serialization path, and verify it fails if such a path were introduced.
- [ ] 6.2 Manually verify: complete onboarding, exit mate, relaunch with `API_TOKEN` still unset, and confirm onboarding is shown again with no memory of the previous token.

## 7. End-to-end verification

- [ ] 7.1 Run the full workspace test suite (`cargo nextest run` per the project `Justfile`) and verify it passes.
- [ ] 7.2 Manually verify each spec scenario in `specs/interactive-onboarding/spec.md` and `specs/model-catalog/spec.md` against a locally built binary: no-token startup reaches onboarding, step order is enforced, invalid token retries in place, valid token starts sessions, already-set token skips onboarding entirely, and `--plain`/`--print` still fails fast with no token.
