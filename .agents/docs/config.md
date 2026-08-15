# Config loading

Layered with `figment`, in `mate-cli/src/config.rs`:

```
flags → env (MATE_*) → ./.mate.toml (or --config) → ~/.config/mate/config.toml → defaults
```

- `Config`, `ToolsConfig`, `PanelConfig`, `PricingEntry` live in
  `mate-cli/src/config.rs` — the CLI-facing shape.
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
