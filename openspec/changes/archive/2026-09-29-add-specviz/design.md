# Design

## Context

See `proposal.md` for motivation and `specs/` for the behavior contract.

Current state that shapes the approach:

- `specviz/` is a separate two-crate workspace (resolver 2, its own
  `Cargo.lock`, `Justfile`, `.github/`), untracked in this repo. The server
  crate is already library-shaped: `bootstrap::build(dir)` returns an `App`
  holding the axum `Router`, the `CommandBus`, and the `notify` watcher guard.
  `main.rs` only parses args, binds, prints, and serves.
- The server sandboxes a single directory (`SandboxedRoot` →
  `<root>/specs`), `SpecId` is relative to that directory, and the watcher
  watches only it. Rendering is `pulldown-cmark` events → syntect highlight
  pass → `html::push_html` → `ammonia` (default builder plus `span[style]`).
- The UI is a Leptos 0.8 CSR app built by Trunk (with Trunk's Tailwind
  integration, content globs `./index.html`, `./src/**/*.rs`) into
  `crates/ui/dist/`, embedded by the server via
  `#[derive(RustEmbed)] #[folder = "../ui/dist/"]`.
- `mate`'s workspace globs `members = ["crates/*"]`, uses resolver 3 and
  `workspace = true` dependencies, and CI runs
  `cargo clippy --workspace --all-targets`, `cargo nextest run --workspace`,
  and `cargo fmt --all --check`. Release builds only `-p mate-cli` for three
  host targets.
- `mate-cli`'s `Cli` is a flat `Parser` with an optional positional `prompt`.
  `main.rs::run` initializes logging, loads config, shows the first-run
  notice, then picks the plain or TUI frontend.
- `mate-tui`'s `App` runs one `select!` loop over terminal input and a shared
  `mpsc::Receiver<SessionEvent>`; `handle_slash_command` is `async` and awaited
  inline, so anything slow inside it stalls rendering. Transcript already has
  `Entry::System` / `Entry::SystemError`, never sent to the model.

## Goals / Non-Goals

**Goals:**
- One release artifact (`mate`), with the viewer and its UI linked in.
- The migrated crates follow this workspace's conventions from day one, so no
  "specviz-flavored" corner persists.
- Host-side CI (`clippy`, `nextest`) stays fast and never needs the wasm
  toolchain; the client still gets linted, on its own target.

**Non-Goals:**
- Tier 3 OpenSpec features: linking a change's delta to its base capability,
  rendered diffs, cross-spec graphs.
- Editing specs from the browser, auth, non-loopback serving.
- Shelling out to the `openspec` CLI for anything.
- Surfacing viewer state in the TUI's agent status panel.
- Stopping a background viewer before `mate` exits (no `/specviz stop`).

## Decisions

### D1. In-process library, not a sidecar process

`mate-specviz-server` is a library; both entry points (`mate specviz`,
`/specviz`) run it inside the `mate` process on the existing Tokio runtime.

- *Alternative: sidecar binary spawned by `mate`.* Gives crash isolation and
  a smaller `mate`, but ships two binaries, needs binary discovery, and needs
  explicit child cleanup on every exit path.
- *Alternative: embed the sidecar's bytes in `mate` and extract at runtime.*
  Adds per-target nested builds, Gatekeeper/AV friction, and cache
  versioning for no user-visible gain.
- In-process makes "stops with `mate`" (spec: *Background viewers stop with
  mate*) structural: no process can outlive its parent.

The crate drops specviz's `[[bin]]` target: the standalone behavior is
`mate specviz`, so no second binary name exists.

### D2. Server public API: bind separately from serve

```rust
pub struct Viewer { url: String, root: PathBuf, _stop: DropGuard }   // drop = stop

pub async fn bind(addr: SocketAddr) -> Result<TcpListener, ViewerError>;
pub fn url_of(listener: &TcpListener) -> Result<String, ViewerError>;
pub fn spawn(listener: TcpListener, root: PathBuf)
    -> Result<(Viewer, JoinHandle<Result<(), ViewerError>>), ViewerError>;   // build + serve in a task
pub async fn run(listener: TcpListener, root: &Path, shutdown: impl Future<Output = ()>)
    -> Result<(), ViewerError>;
```

`bind` is fast and is awaited inline, so the URL is known (and printed)
before indexing starts. `spawn` moves the listener into a task that runs
`bootstrap::build(root)` and then `select!`s `axum::serve(...)` against the
shutdown future. The shutdown is deliberately not graceful: open SSE streams
never finish, so a graceful shutdown would never return. Connections made
during indexing wait in the accept backlog. `run` is the foreground form used
by `mate specviz`, with Ctrl+C as the shutdown future. `Viewer` holds a
`DropGuard` over the task's cancellation token, so dropping it stops the
server; the returned `JoinHandle` lets the TUI learn when a viewer failed.

`ViewerError` is a `thiserror` enum (`Bind { addr, source }`,
`Root { path, source }`, `Watch(notify::Error)`, `Serve(io::Error)`), per
this repo's error-handling rules; `mate-cli` maps it into `MateError`.

- *Alternative: `start(root) -> Result<Viewer>` doing bind+build together.*
  Simpler signature, but awaiting it inside `handle_slash_command` would
  block the TUI for the whole index walk.

### D3. Sources replace the single sandbox root

`SandboxedRoot` becomes a canonicalized served root plus the list of present
source directories (`specs/`, `openspec/`). `SpecId` becomes root-relative
(`specs/a.md`, `openspec/changes/x/tasks.md`). `resolve` canonicalizes
`root.join(id)` and accepts it only if it `starts_with` one of the
canonicalized source dirs and has an `.md` extension.

`FsSpecRepository::walk_specs` dispatches per present source on `DocKind`
(a plain recursive walk, or the OpenSpec walk yielding three sections) and
concatenates the results. An enum match was chosen over a `SpecSource`
trait: there are exactly two closed-set sources, and `resolve` already answers
"which source owns this ID". The existing CQRS split (commands mutate the
index, queries read it) is untouched.

Sources are detected at startup only. A `specs/` or `openspec/` directory
created while the viewer runs is picked up on the next `mate specviz` /
`/specviz` start — consistent with today's watcher, which also only watches a
directory that existed at startup.

### D4. Tree payload grows; routes do not

`/api/specs` and `/api/specs/{*id}` keep their paths. `SpecTreeNode` gains
variants, serialized with the existing `type` tag:

```
section    { kind: "plain" | "capabilities" | "changes" | "archive", children }
dir        { name, children }                       (unchanged)
file       { id, title }                            (unchanged)
capability { path, id, title, requirements }
change     { name, progress: { done, total } | null, children: [file] }
```

The client's `api.rs` mirrors these; the sidebar renders section headers,
change rows with a `done/total` badge, and the Archive section collapsed by
default.

### D5. OpenSpec parsing is pure domain code

`domain/openspec.rs` holds pure functions over `&str`:
`task_progress`, `requirement_count`, `delta_op(heading) -> Option<DeltaOp>`,
`artifact_rank(relative_path)`. No I/O, so each spec scenario in
`specviz-openspec` maps to a plain unit test. The repository calls them while
walking; `RefreshSpecIndex` recomputes them, and because a `tasks.md` edit
is a content `Modify`, D6's invalidation must also refresh the owning change's
progress (the cheap `InvalidateSpec` path alone would leave the badge stale).
Rule: a content edit to a file named `tasks.md` or `spec.md` under
`openspec/` dispatches `RefreshSpecIndex`, not `InvalidateSpec`.

- *Alternative: `openspec list --json`.* Needs Node at runtime and doesn't fit
  a watcher-driven in-memory index.

### D6. Rendering takes a `DocKind`

`MarkdownRenderer::render(markdown, kind)` with `DocKind::{Plain, OpenSpec}`,
derived in `RenderSpec` from the ID prefix. For `OpenSpec`, a new event pass
runs before the syntax-highlight pass:

- `Start(Heading(H2))` + text matching a delta op → emit
  `<h2 class="sv-delta sv-delta-{added|modified|removed|renamed}">`
  with a `<span class="sv-badge">` label.
- `### Requirement:` → `<h3 class="sv-req">`.
- `#### Scenario:` → open `<section class="sv-scenario">` before the heading;
  close it at the next heading of level ≤ 4 or end of document.
- Inside a scenario, a list item whose first text (or first `Strong` child)
  is `WHEN`/`THEN`/`AND` → wrap that keyword in `<strong class="sv-kw">`.

Emitted markup goes through `Event::Html`, then `ammonia` as today, with
`class` allowed on `h2`, `h3`, `section`, `span`, `strong` via
`allowed_classes`, restricted to the fixed `sv-*` set. Authored classes
outside it are stripped (spec: *Rendering stays sanitized*).

- *Alternative: style in the client by inspecting rendered DOM.* Moves
  Markdown semantics into WASM, which the original design deliberately kept
  server-side.

### D7. `sv-*` styles live in plain CSS, not Tailwind utilities

The server emits the `sv-*` classes, so Tailwind's content scan of the client
never sees them and the release build would drop them. Define them with
`@layer components { .sv-delta { @apply ... } }` in `style/input.css`
(`@apply` output is always emitted), not as utility classes in server
strings.

- *Alternative: `safelist` in `tailwind.config.js`.* Works, but duplicates
  the class list in a second file that silently drifts.

### D8. Client crate compiles to nothing on the host

`crates/mate-specviz-client` stays a workspace member (so it shares
`Cargo.lock` and `workspace = true` dependencies), but all its dependencies
move under `[target.'cfg(target_arch = "wasm32")'.dependencies]` and its
source is gated with `#![cfg(target_arch = "wasm32")]` (`main.rs` keeps an
empty host `fn main`). Host `clippy --workspace` and `nextest --workspace`
therefore build it as an empty crate. CI adds one job:
`trunk build --release` in the client dir, then
`cargo clippy -p mate-specviz-client --target wasm32-unknown-unknown -- -D warnings`.

- *Alternative: `exclude` it from the workspace.* It would need its own
  lockfile and couldn't use `workspace = true` dependencies.
- *Alternative: `default-members`.* CI passes `--workspace`, which ignores
  `default-members`.

### D9. Missing UI bundle does not break host builds

Host CI jobs never run Trunk, so `crates/mate-specviz-client/dist/` does not
exist there. The embed uses rust-embed's `#[allow_missing = true]`; when the
bundle is absent the static handler serves a short "UI not built" HTML page
instead of the SPA. The release workflow runs `trunk build --release` first
and fails the job if `dist/index.html` is missing before `cargo build`.
`dist/` is added to `.gitignore`.

### D10. `mate specviz` branches before any agent setup

`Cli` gains `#[command(subcommand)] command: Option<Command>` with
`Command::Specviz { dir, port }`, plus `args_conflicts_with_subcommands` so
flags like `--model` are rejected with the subcommand rather than ignored.
`main.rs::run` initializes logging, parses, and on `Some(Command::Specviz)`
calls a new `specviz.rs` module before `config::load` and
`firstrun::show_once`. It prints one line to stdout
(`specviz serving <root> at http://127.0.0.1:<port>`) and runs until Ctrl+C
(`tokio::signal`, already enabled in `mate-cli`).

### D11. `/specviz` in the TUI

- `SlashCommand::Specviz` (no argument) in `slash.rs`.
- `mate-tui/src/specviz.rs`'s `Viewers` owns a `HashMap<PathBuf, Viewer>`
  keyed by the tab's canonicalized root, plus an unbounded
  `ViewerExit` channel; `App` holds one `Viewers` and `run_loop` gains a
  `viewers.exits.recv()` arm in its `select!`.
- On `/specviz`: `Viewers::ensure` returns the existing URL for a live root,
  or `bind(127.0.0.1:0)`s inline (fast) and `spawn`s; `App` writes
  `specviz serving <root> at <url>` (URL last, nothing after it) with
  `push_system`, or a `specviz: <error>` line with `push_system_error`. A
  small task awaits the join handle and sends
  `ViewerExit { root, url, tab, error }` only if the viewer ended with an
  error.
- On `ViewerExit`: `Viewers::forget` removes the entry if its URL still
  matches (a stale exit can't evict a newer viewer for the same root), so a
  later `/specviz` retries; the issuing tab, if still open, gets a
  `SystemError` line.
- On quit, dropping `App` drops every `Viewer`, cancelling its token.

`mate-tui` depends on `mate-specviz-server`; the dependency graph gains
`tui ──► mate-specviz-server` and `cli ──► mate-specviz-server`, neither of
which touches `core` or the tool crates.

### D12. Logging

All `println!`/`eprintln!` in the migrated server become `tracing` events
(target `mate_specviz_server`). `mate specviz`'s single startup line is
printed by `mate-cli`, not the library, so the library never writes to
stdout/stderr in either mode.

## Risks / Trade-offs

- [New dependencies fail `cargo deny`'s licence allowlist (syntect,
  leptos, notify transitive crates)] → Run `just deny` right after the
  crates are added; extend `deny.toml`'s `allow` only for OSI licences with a
  comment naming the crate that needs it, and record the outcome in
  `tasks.md`.
- [`#[allow_missing = true]` not supported by the pinned rust-embed version
  or behaving differently in debug builds] → Verify against the rust-embed
  source once the dependency is fetched; fallback is a `build.rs` in the
  server that `cargo:rerun-if-changed`s `dist/` and errors only when
  `PROFILE=release`.
- [Binary size growth from axum, syntect's default syntaxes/themes, and the
  WASM bundle] → Accepted; measure before/after on the release build and
  note it in the PR. syntect's default-fancy features can be trimmed later.
- [Trunk's Tailwind integration downloads a Tailwind binary at build time]
  → Pin Trunk and the Tailwind version in CI; cache between runs.
- [The bare prompt `mate specviz` changes meaning] → Accepted as a minor
  breaking change; documented in README and release notes.
- [Watcher only covers source dirs present at startup] → Documented;
  restarting the viewer picks up new sources.
- [Large repos make the initial walk slow] → The walk happens after the URL
  is shown and off the TUI's event loop (D2); a browser request during
  indexing waits rather than seeing an empty tree.

## Migration Plan

1. Copy `specviz/crates/server` → `crates/mate-specviz-server`,
   `specviz/crates/ui` → `crates/mate-specviz-client`; rename packages;
   switch to `workspace = true` dependencies (adding new ones to the root
   `[workspace.dependencies]`); carry the wasm `opt-level = "z"` profile
   override into the root `Cargo.toml`.
2. Move `specviz/docs/spec.md` and `specviz/specs/000-Initial.md` content into
   `.agents/docs/specviz.md`; fold the specviz skill's rules that still apply
   into it. Delete `specviz/`.
3. Land D3–D7 (sources, OpenSpec parsing, rendering) inside the server and
   client crates.
4. Land D10–D11 (CLI subcommand, slash command).
5. Update CI and release workflows (D8, D9), README, and
   `.agents/docs/architecture.md`.

Rollback: revert the change; nothing persists outside the repo.
