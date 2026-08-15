# Error handling

Full write-up lives in `CONTRIBUTING.md` — read that before touching error
types. Summary:

- **Libraries** (`mate-core`, `mate-tool-*`, `mate-tui`) return `thiserror`
  enums, never `anyhow::Error` — callers need a matchable type. Tool crates
  converge on `ToolFailure` (`mate-tool-api`, not yet landed): non-fatal by
  design, its `Display` string is fed back to the model as a tool result, not
  raised as a turn abort.
- **`mate-cli` plumbing** (`config.rs`, `logging.rs`) stays `anyhow::Result`
  with `.context(...)` — the binary edge, where a human-readable trail matters
  more than a matchable type.
- **CLI boundary** — `mate-cli/src/error.rs`'s `MateError` is the one place
  `anyhow::Error` gets pinned to a category with a stable exit code. `main()`
  prints the error plus its `source()` chain, then converts
  `MateError::exit_code()` to the process `ExitCode`.

## Rules of thumb

- No `unwrap()` / `expect()` outside tests and truly-impossible states — and
  even then, prefer `expect("why this can't happen")` over a bare `unwrap()`.
- Add a `MateError` variant only once a failure mode has a real producer, not
  ahead of it. An unconstructed variant is dead code and CI runs
  `clippy -D warnings`. Exit codes: `Config` = 2, `Io` = 3, `Other` = 1,
  success = 0; `Auth`/`Provider` = 4/5 are reserved for when
  provider-auth/provider-request failures get a real producer.
- A tool's error is not the CLI's error — `ToolFailure` flows back into the
  model's context, `MateError` flows to a human's terminal. A tool crate
  should never construct or depend on `MateError`.
