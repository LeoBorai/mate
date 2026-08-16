---
name: mate-software-engineer
description: Engineering practices, hard rules, and an architecture map for working in the mate Rust workspace (a CLI coding agent built on Rig + HuggingFace). Load before making code, doc, or config changes anywhere in this repo.
---

# mate software engineer

`mate` is a Rust CLI coding agent built on Rig, talking to HuggingFace
Inference Providers. This skill is the entry point for working in this
workspace — read it before editing code, docs, or config here.

## Hard rules (non-negotiable)

1. **Never run `cargo` commands** — `cargo build`, `cargo test`,
   `cargo clippy`, `cargo fmt`, `cargo deny`, `cargo nextest`, etc. Not
   directly, not via a script. Use the `Justfile` recipes as reference for
   what CI runs, but do not execute them. If verification is needed, ask the
   user to run it, or rely on CI once it exists. Verify changes by reading the
   code and cross-checking against dependency sources
   (`~/.cargo/registry/src/...`) instead of compiling.

2. **Never reference `plan.md`, or any other planning doc supplied
   out-of-band, from anything committed** — code, doc comments, commit
   messages, `AGENTS.md`, `CONTRIBUTING.md`, files under `.agents/`. Such
   files are working input, not part of the repo (`plan.md` is
   `.gitignore`d). Anyone without the original file loses the referent, and
   the pointer rots the moment a section renumbers. Milestone tags (`M0-5`)
   and `§N` section numbers are fine as an internal, self-contained
   convention as long as the doc using them defines them itself; "per the
   plan", "`plan.md`", or any wording implying a reader needs an external doc
   is not fine.

## Where things are

See `.agents/docs/architecture.md` for the full workspace layout and
dependency graph. Short version:

```
cli ──► tui ──► core ──► rig
                  ├────► mate-tool-fs
                  ├────► mate-tool-http
                  └────► mate-tool-agent ──► mate-tool-api ──► rig
```

`mate-tool-api` must never depend on `mate-core` — capabilities `mate-core`
needs inside a tool crate are declared as traits there and implemented in
`mate-core`, never the reverse.

Toolchain: Rust `1.97.1` (`rust-toolchain.toml`), edition 2024, workspace
resolver `3`.

## Topic docs — read the one that matches the task

| Doc | Read it when touching... |
|---|---|
| `.agents/docs/architecture.md` | any new crate, or unsure where a change belongs |
| `.agents/docs/config.md` | `Config`, `AgentSpec`, `SessionSpec`, or anything env/TOML-loaded |
| `.agents/docs/logging.md` | `tracing` setup, log output, span fields |
| `.agents/docs/error-handling.md` | a new error variant or error-propagation path (full detail in `CONTRIBUTING.md`) |
| `.agents/docs/providers.md` | `Backend`, `build_agent`, preambles, provider error classification/retry |
| `.agents/docs/streaming.md` | `AgentEvent`, the Rig stream-to-event mapping, cancellation, usage |
| `.agents/docs/testing.md` | writing or reasoning about tests, or CI expectations |

## General practices

- Libraries return `thiserror` enums; `mate-cli` plumbing returns
  `anyhow::Result`; the CLI boundary (`MateError`) is the only place errors
  get pinned to an exit code. Full detail: `.agents/docs/error-handling.md`
  and `CONTRIBUTING.md`.
- Define a new enum variant (error, event, outcome) at the point its *shape*
  is known, even before anything produces it — that's cheaper than a breaking
  enum change once the producer lands. Don't do the same for whole new
  abstractions that don't exist yet; that's scope creep, not future-proofing.
- Add tests offline (`tempfile`, `wiremock`, mock models, synthetic streams).
  Exactly one live-network integration test is allowed
  (`crates/mate-core/tests/hf_backend.rs`), and it's `#[ignore]`d — never let
  a live-network test run unattended or in CI.
- Extend existing config/logging/error modules rather than adding a second
  mechanism that does the same job.

## Ratatui / TUI notes (`mate-tui`, `M7`+)

- **Use `ratatui-textarea`, not `tui-textarea`.** `tui-textarea` 0.7.0 hard-pins `ratatui` 0.29
  internally (its `Widget` impl targets that version's trait) and can't render into this
  workspace's `ratatui` 0.30 `Frame` — confirmed by reading its source, not by trial and error.
  `ratatui-textarea` (the `ratatui`-org continuation) tracks current `ratatui-core`/
  `ratatui-widgets` and is the one in `workspace.dependencies`.
- Translate `crossterm::event::KeyEvent` into `ratatui_textarea::Input` field by field
  (`key`/`ctrl`/`alt`/`shift`, all public) instead of enabling that crate's own `crossterm`
  feature — avoids ever needing its bundled crossterm version to line up with the workspace's.
- A bare generic type in a fn signature (`&mut Frame`, `&View`) triggers Rust's "elided
  lifetime in path" behavior silently rather than erroring — write `Frame<'_>` / `View<'_>`
  explicitly so it isn't ambiguous to the next reader.
- Rule 1 ("never run `cargo`") is about *me* invoking it. A `rust-analyzer` flycheck process
  can be running in the sandbox independently — `target/flycheck0/{stdout,stderr}` (stdout is
  `--message-format=json`, filter for `reason: "compiler-message"`) is real compiler ground
  truth and safe to *read*; it just isn't safe to *trigger*. Don't poll it in a sleep loop —
  check once, and treat a stale timestamp as "no new information," not as a compile failure.
- Hand-writing an `insta` `.snap` file (since `cargo insta accept` also needs `cargo`): front
  matter is `source: <path relative to the workspace root>` + `expression: <macro arg as
  typed>`, `---`, then the raw `Display` output. `assertion_line` appears in a *pending*
  snapshot's terminal summary but is stripped before persistence — don't include it.
  `TestBackend`'s `Display` wraps every row in literal `"..."` and pads to the buffer width;
  copy failing-test output verbatim rather than retyping it, and verify row length
  programmatically (`len()` per row) since trailing spaces are significant and easy to drop by
  hand.
- Adding a new side panel to `draw()`: wrap it in an outer `Layout::horizontal([Constraint::
  Length(N), Constraint::Min(0)])` and put the existing vertical split inside the `Min(0)` chunk
  — don't touch the vertical split itself. If a snapshot test exists, widen `TestBackend` by
  exactly `N` instead of resizing the whole thing: the pre-existing (right-hand) columns then
  stay byte-for-byte identical to the old snapshot, and only the new `N`-wide left columns need
  hand-computing per row (title/border math, then paste the untouched original row content after
  it). Verify with a quick Python snippet (border length, row length == N) rather than
  hand-counting dashes.
- `SessionHandle`/`SessionManager::spawn` (`mate-core/src/session.rs`) drop the `SessionSpec`
  after building the `Agent` — nothing about model, sub_provider, or backend kind is retrievable
  from the handle afterward. Any UI that needs to display them must have the caller
  (`mate-cli/src/tui.rs`) pass them down explicitly as plain data (e.g. extra `String` args on
  `mate_tui::run`), threaded through `App` and into `View`, rather than trying to pull them back
  out of the session/backend layer.
- `Backend` (`mate-core/src/backend.rs`) never stores a provider *name* string — it's an enum
  (`HuggingFace { .. }` / `OpenAiCompatible(..)`) with no getter. For display purposes derive the
  provider label at the call site instead (e.g. `config.sub_provider.clone().unwrap_or_else(||
  "huggingface".into())`); don't add a getter to `Backend` just to feed a UI label.
