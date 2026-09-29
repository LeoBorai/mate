# Proposal

## Why

`mate`'s planning work lives in `openspec/` (capability specs and in-flight
changes), but the only way to read it today is raw Markdown in an editor.
`specviz` — a local Markdown spec viewer (Axum server + embedded Leptos/WASM
UI) — already exists as a separate, untracked workspace under `specviz/`,
reads only a `specs/` directory, and ships in no release. Folding it into
`mate` gives every `mate` user a browsable, live-reloading view of a
workspace's specs and OpenSpec changes, reachable from inside a running
session without installing a second tool.

## What Changes

- Migrate `specviz/crates/server` to `crates/mate-specviz-server` and
  `specviz/crates/ui` to `crates/mate-specviz-client`, adopting this
  workspace's conventions (`workspace = true` dependencies, `tracing` instead
  of `println!`/`eprintln!`, `deny.toml`). Delete `specviz/` entirely; no git
  history is carried over. Its design notes move under `.agents/docs/`.
- `mate-specviz-server` becomes a library exposing a start/serve API that
  returns a handle carrying the bound URL and a shutdown path. The UI bundle
  stays embedded in it via `rust-embed`, so the `mate` binary is the only
  release artifact.
- New `mate specviz [-C <dir>] [--port <port>]` subcommand: runs the viewer in
  the foreground (today's standalone `specviz` behavior), without first-run
  notice, provider backend, or MCP server setup.
- New `/specviz` TUI slash command: starts the viewer for the active tab's
  workspace root as a background task on an OS-assigned loopback port, and
  writes the URL into the tab's transcript as a system line. Never opens a
  browser. Re-running it for the same root re-prints the existing URL instead
  of starting a second server.
- The viewer discovers two source layouts under the served root and shows
  both when present: plain `specs/**/*.md`, and OpenSpec's `openspec/`
  (capability specs, active changes, archived changes).
- OpenSpec-aware navigation: sidebar sections for Specs / Capabilities /
  Changes / Archive, a task-progress badge per change (from `tasks.md`
  checkboxes), and a requirement count per capability spec.
- OpenSpec-aware rendering for OpenSpec documents only: delta section
  headings (`ADDED` / `MODIFIED` / `REMOVED` / `RENAMED Requirements`) as
  labeled badges, requirement headings styled distinctly, `#### Scenario:`
  blocks rendered as cards, and `WHEN` / `THEN` / `AND` keywords emphasized.
- CI and release workflows gain the `wasm32-unknown-unknown` target and a
  Trunk build of the client before the `mate` build.
- **BREAKING** (minor): `mate specviz` with no other arguments now runs the
  subcommand instead of sending the one-shot prompt `"specviz"`. Any prompt
  that is not exactly the bare word `specviz` is unaffected.

## Capabilities

### New Capabilities
- `specviz-viewer`: serving the spec viewer — source discovery (`specs/`,
  `openspec/`), sandboxed file access, the spec tree and rendered-spec API,
  and live reload on filesystem change.
- `specviz-openspec`: OpenSpec-aware navigation (sections, task progress,
  requirement counts) and OpenSpec-aware rendering (delta badges,
  requirement headings, scenario cards, keyword emphasis).
- `specviz-launch`: starting the viewer from `mate` — the `mate specviz`
  foreground subcommand and the `/specviz` background slash command,
  including URL reporting, per-root reuse, failure reporting, and shutdown
  with `mate`.

### Modified Capabilities
<!-- None: no capability specs exist under openspec/specs/ yet. -->

## Impact

- **New crates**: `crates/mate-specviz-server` (host, lib + embedded UI),
  `crates/mate-specviz-client` (`wasm32-unknown-unknown`, built by Trunk).
- **Removed**: `specviz/` (its own `Cargo.toml`, `Cargo.lock`, `Justfile`,
  `.github/`, docs).
- **`mate-cli`**: `Cli` gains a subcommand; `main.rs` branches to the viewer
  before first-run/config-dependent setup.
- **`mate-tui`**: `SlashCommand::Specviz`, per-root server handles owned by
  `App`, a new event-loop arm for background startup failures.
- **Dependencies**: `axum`, `notify`, `pulldown-cmark`, `syntect`, `ammonia`,
  `rust-embed`, `mime_guess` on the host; `leptos`, `gloo-net`,
  `wasm-bindgen` on the client. Licences must pass `cargo deny`.
- **Build/CI**: Trunk + wasm32 target required in CI and release jobs; the
  host `clippy`/`nextest` runs must not try to build the client for the host.
- **Binary size**: the `mate` binary grows by the server dependencies plus the
  embedded WASM bundle.
