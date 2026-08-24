# Config loading

Layered with `figment`, in `mate-cli/src/config.rs`:

```
flags → env (MATE_*) → ./.mate.toml (or --config) → ~/.config/mate/config.toml → defaults
```

- `Config`, `ToolsConfig`, `PanelConfig`, `PricingEntry`, `BackendKind` live in
  `mate-cli/src/config.rs` — the CLI-facing shape. `BackendKind`
  (`huggingface`/`gemini`, default `huggingface`) picks which
  `mate_core::backend::Backend` path the whole process talks through —
  `--backend`/`Config::backend`, distinct from `--provider`/`sub_provider`
  (an HF-only partner choice). `plain::build_backend` is the one place that
  turns `Config::backend` into a real `Backend`; both frontends call it.

- `apply_flags` also backend-switches the subagent default: if
  `--subagent-model` wasn't passed and `delegation.subagent_model` is still
  exactly `mate_core::config::DEFAULT_SUBAGENT_MODEL` (the HuggingFace-path
  default) after the figment merge, and `backend == Gemini`, it's replaced
  with a Gemini model (`gemini-3.5-flash-lite`) — so a `--backend gemini`
  run doesn't spawn subagents against a Qwen model that backend can't serve.
  This is a string-equality heuristic, not a "was this explicitly set"
  check — figment doesn't preserve that distinction post-merge — so it
  won't fire if a config file/env var explicitly pins `subagent_model` to
  that same HuggingFace default string while also selecting `gemini`; that
  combination is assumed deliberate.
- `DelegationPolicy`, `HttpPolicy`, `HttpAccessPolicy`, `AgentSpec`,
  `SessionSpec` live in `mate-core/src/config.rs` — the shared, provider-facing
  shape that both the CLI and (eventually) the session manager build from.

## `API_TOKEN` is env-only

Read via `config::api_token()`. It is **never** a `Config` field and never
round-trips through a config file, even if a config file happens to set the
key. Don't add it to `Config` — that reopens a credential leak this rule
exists to prevent.

## Testing config precedence

`figment::Jail` (dev-dependency, `test` feature) drives one test per
precedence layer in `mate-cli/src/config.rs`. `Jail::create_file` does **not**
create parent directories — call `std::fs::create_dir_all` first for any
nested path (e.g. `.config/mate/config.toml`).

## Extend, don't reimplement

Both `config.rs` files already exist and are the right place to add new
config surface — don't invent a second config-loading path.
