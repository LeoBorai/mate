# AGENTS.md

Before making any code, doc, or config change in this repo, read
`.agents/skills/mate-software-engineer/SKILL.md`. It has the full engineering
practices for this workspace and points to topic docs under `.agents/docs/`
for architecture, config, logging, error handling, providers/agent
construction, streaming, and testing. This applies regardless of which agent
or tool is reading this file.

## Hard rules

These apply even if nothing else in this file or the skill gets read.

**Never run `cargo` commands** (`cargo build`, `cargo test`, `cargo clippy`,
`cargo fmt`, `cargo deny`, `cargo nextest`, etc.) directly in this
environment. Use the `Justfile` recipes as reference for what CI runs, but do
not execute them yourself. If verification is needed, ask the user to run it
or rely on CI.

**Never reference `plan.md`, or any other planning doc supplied out-of-band,
from code, doc comments, commit messages, or committed docs (`AGENTS.md`,
`CONTRIBUTING.md`, `.agents/**`, etc.).** Such files are working input, not
part of the repo (`plan.md` is `.gitignore`d) — anyone without the original
loses the referent, and the pointer rots the moment a section renumbers.
Milestone tags (`M0-5`) and `§N` section numbers are fine as an internal,
self-contained convention, since the doc using them defines them itself;
"per the plan", "`plan.md`", or any wording that implies a reader needs an
external doc to make sense of a comment is not.

## Project

`mate` — Rust CLI coding agent built on Rig + HuggingFace. See
`.agents/docs/architecture.md` for the workspace layout and dependency graph.
