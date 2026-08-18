# clap practices in `mate-cli`

Read this before touching `crates/mate-cli/src/cli.rs` or adding a new flag.
The whole CLI surface is one `#[derive(Parser)]` struct — there is no
subcommand tree, and nothing here should introduce one without a real reason
(`mate` is "one binary, a handful of flags," not a multi-command CLI).

## One `Cli` struct, derive only

```rust
#[derive(Parser, Debug, Clone)]
#[command(name = "mate", version, about = "A terminal coding agent")]
pub struct Cli { /* fields */ }
```

Every flag is a field with a `///` doc comment (becomes the `--help` text)
and an `#[arg(...)]` attribute where the default derivation isn't right —
`short`/`long` renames, `value_name`, `requires`. Keep using the derive API;
don't drop to the builder API (`Command::new(...)`) for a one-off flag —
consistency here matters more than saving a line.

`Cli` derives `Clone` — it gets threaded into both `config::load` and (for
`-C`/`--dir`) workspace-root resolution, so cheap cloning matters more than
it would for a typical one-shot arg struct.

## `Vec<PathBuf>` for a repeatable flag

```rust
#[arg(short = 'C', long = "dir", value_name = "PATH")]
pub dir: Vec<PathBuf>,
```

`-C`/`--dir` repeats to open one tab per workspace root (`mate -C a -C b`).
A bare `Vec<T>` field is clap's idiom for "flag may repeat, collect every
occurrence" — no `#[arg(action = ArgAction::Append)]` needed, that's the
default for a `Vec` field.

## Cross-field validation: `requires`, not manual checks

```rust
#[arg(short = 'p', long, requires = "prompt")]
pub print: bool,
```

`--print` only makes sense with a prompt supplied; `requires = "prompt"`
(referring to the positional `prompt: Option<String>` field's name) makes
clap itself reject `mate --print` with no prompt at parse time
(`ErrorKind::MissingRequiredArgument`), rather than the app checking `if
cli.print && cli.prompt.is_none()` after the fact. Prefer a `requires`/
`conflicts_with` attribute over a manual post-parse check whenever the
relationship is expressible declaratively — it shows up in `--help`-adjacent
tooling and is one less thing a future flag addition can silently break.

## Testing: `try_parse_from`, not manual `std::env::args` mocking

```rust
#[test]
fn print_without_a_prompt_is_a_usage_error() {
    let err = Cli::try_parse_from(["mate", "--print"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}
```

`Cli::try_parse_from([...])` (fallible) or `Cli::parse_from([...])`
(panics on error — use when the test's whole point is that parsing
*succeeds*) take a plain `&[&str]`-like argv, first element the program
name. Assert on `err.kind()` for a usage-error test, not on the error's
rendered string — the string is for humans, `ErrorKind` is the stable
contract. This is also the pattern `mate-cli/src/config.rs`'s precedence
tests use to build a `Cli` fixture inline (`fn cli(args: &[&str]) -> Cli`)
before feeding it into `config::load`.

## Env var prefix stays in `figment`, not clap

`clap` here only parses flags — `MATE_*` env-var precedence and `.mate.toml`
layering are `figment`'s job in `mate-cli/src/config.rs`, applied *after*
`Cli::parse()`. Don't add `#[arg(env = "...")]` to a `Cli` field to pull in
an env var; that would create a second, competing precedence path outside
`config.rs`'s documented `flags → env → project file → user file → defaults`
chain. See `.agents/docs/config.md`.
