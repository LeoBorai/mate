# Logging

`tracing-subscriber` (`env-filter` feature) + `tracing-appender`, wired in
`mate-cli/src/logging.rs`.

- **Never stdout/stderr, ever.** The TUI owns the terminal; in `--plain` mode
  stdout is reserved for the conversation. All logging goes to a file only.
- Log file: `$XDG_STATE_HOME/mate/mate.log`, falling back to
  `~/.local/state/mate/mate.log` (same pattern as `XDG_CONFIG_HOME` in
  `mate-cli/src/config.rs`).
- Level from `RUST_LOG`, default `info`.
- `logging::init()` is called first thing in `main()`, before `Cli::parse()` /
  `config::load()`, and returns a `WorkerGuard`. That guard must stay alive for
  the whole process — the writer is non-blocking, and dropping the guard early
  silently loses buffered lines.

## Forward-looking: span fields

`SessionId` / `AgentId` don't exist yet (session manager, not started). Once
they do, every span opened in `mate-core` should carry them as fields from the
start — adding a span key later means finding and updating every span
call site, whereas adding it now (even with only `AgentId::ROOT` ever
appearing) costs nothing.

## Tests

`crates/mate-cli/tests/tracing_stdout.rs` spawns the built binary and asserts
empty stdout plus a populated log file. Unit tests in `logging.rs` cover
state-dir resolution.
