# Contributing

## Error strategy (M0-5)

Three layers, each with one job.

### Libraries — `thiserror`

Every `mate-*` library crate (`mate-core`, `mate-tool-api`, `mate-tool-fs`, `mate-tool-http`,
`mate-tool-agent`, `mate-tui`) defines its own `#[derive(thiserror::Error)]` enum for errors
callers might match on. Don't return `anyhow::Error` from a library function — it erases the
type and forces every caller to string-match or downcast.

Tool crates in particular converge on `ToolFailure` (`mate-tool-api`, landing with `M3-1`):
a typed, non-fatal enum. A tool error is fed back to the model as a tool result, not raised as
a turn abort — the `Display` string is written for a model to read and act on, not for a human
staring at a stack trace. `mate-core`'s own errors (session/agent construction, provider
mapping) stay `thiserror` too, for the same reason: `mate-cli` needs to distinguish them.

### Binary edge — `anyhow`

Inside `mate-cli`, plumbing code (`config.rs`, `logging.rs`, and anything else that's mostly
"call a few fallible things and add context") returns `anyhow::Result`. Use
`.context("...")` / `.with_context(|| ...)` liberally — this is the layer where a
human-readable trail matters more than a matchable type. Don't invent a `thiserror` enum here;
that's what the next layer is for.

### CLI boundary — `MateError`

`main.rs` is the one place `anyhow::Error` gets pinned to a category. `run()` (the real
`main`, returning `Result<(), MateError>`) maps each fallible call site to a `MateError`
variant right where it knows what failed:

```rust
let _log_guard = logging::init().map_err(MateError::Io)?;
let config = config::load(&args).map_err(MateError::Config)?;
```

`main()` itself never returns `Result` — it calls `run()`, and on failure prints the error plus
its full `source()` chain (each `context(...)` layer anyhow added, one "caused by:" line apiece)
before turning `MateError::exit_code()` into the process's `ExitCode`. Every category gets a
distinct, stable exit code (`Config` = 2, `Io` = 3, anything uncategorized = 1; `0` is success
and nothing else).

Add a `MateError` variant only once a failure mode has a real producer, not ahead of it — an
unconstructed enum variant is dead code, and CI runs `clippy -D warnings`. `Auth` and
`Provider` will land as variants (exit codes 4 and 5, keep them free) when `M1-5`/`M5-4`
(provider error mapping) give them something to build from.

### Rules of thumb

- No `unwrap()` / `expect()` outside tests and truly-impossible states (and even then, prefer
  `expect("why this can't happen")` over a bare `unwrap()`).
- Add context at the point you have information the caller doesn't (a path, a URL, an ID) —
  don't just propagate with `?` and hope the leaf error is self-explanatory.
- A tool's error is not the same thing as the CLI's error. `ToolFailure` flows back into the
  model's context; `MateError` flows to a human's terminal and a process exit code. Don't
  conflate the two — a tool crate should never construct or depend on `MateError`.
