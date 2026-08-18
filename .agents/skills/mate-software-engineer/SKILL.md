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
   directly, not via a script. This includes after making an edit, "just to
   confirm it compiles" — that instinct is exactly what rule 1 forbids; the
   edit itself is the deliverable, not a green build. Use the `Justfile`
   recipes as reference for what CI runs, but do not execute them. If
   verification is needed, ask the user to run it, or rely on CI once it
   exists. Verify changes by reading the code and cross-checking against
   dependency sources (`~/.cargo/registry/src/...`) instead of compiling.

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
| `.agents/docs/tools.md` | `ToolCtx`, `ToolFailure`, `ToolActivity`, the path jail, `mate-tool-fs`, `mate-tool-http`/SSRF guards |
| `.agents/docs/delegation.md` | `spawn_agent`, `SubagentSpawner`/`SubagentRunner`, delegation guardrails, subagent cancellation |
| `.agents/docs/panel.md` | the agent status panel, subagent roster, network/documents logs, cost estimation |
| `.agents/docs/testing.md` | writing or reasoning about tests, or CI expectations |

## Framework notes — read the ref that matches what you're touching

| Ref | Read it when touching... |
|---|---|
| `refs/ratatui.md` | anything in `mate-tui`: widgets, layout, the panel framework, terminal lifecycle |
| `refs/rig.md` | agent construction, tool impls (any `mate-tool-*` crate), streaming, `AgentHook`s, mock-model tests |
| `refs/clap.md` | `mate-cli/src/cli.rs`, adding or changing a flag |

These are gotchas and idioms for the *framework*, not this repo's own
architecture — for what the code does and why, use the topic docs table
above instead. Note also (`refs/ratatui.md`): hard rule 1 above is about
*invoking* `cargo` — a sandboxed `rust-analyzer` flycheck process's output
under `target/flycheck0/` is safe to *read* as compiler ground truth even
though nothing here may trigger it.

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
- Every `assert_eq!`/`assert_ne!` in a test takes a third-argument message
  explaining *what's expected and why*, not just restating the values (the
  values already print on failure). Write it as the fact under test, e.g.
  `assert_eq!(report.turns, 2, "one completion call per mock stream turn")` —
  a future reader debugging a red test should learn the intent from the
  message alone, without reading the surrounding test body first.
- **A tool is the security boundary, not a checkpoint in front of one.** Rig
  executes tool calls automatically; there is no interception point between
  model and `call()`. Any new filesystem/network capability enforces its own
  jail/allowlist inside the tool itself (`ToolCtx::resolve` for paths,
  `ip_guard`/`HttpShared` for network) — never assume a caller already
  checked. See `.agents/docs/tools.md`.
- **Subagent non-addressability (§7.6) is a hard invariant, enforced in the
  types.** No user input reaches a running subagent, ever — its whole input
  is preamble + task, fixed at spawn. Don't add a channel, setter, or field
  that could let anything post-construction reach into a subagent's
  conversation, even for a seemingly reasonable feature ("nudge the
  subagent"). See `.agents/docs/delegation.md`.
