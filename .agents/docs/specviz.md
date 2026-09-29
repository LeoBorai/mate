# specviz (spec viewer)

A local, read-only, live-reloading web view of a workspace's Markdown specs.
Two crates, both linked into the `mate` binary — there is no separate
`specviz` executable:

- `crates/mate-specviz-server` — library. Axum HTTP server, CQRS application
  layer, embedded UI bundle (`rust-embed`).
- `crates/mate-specviz-client` — Leptos CSR app compiled to
  `wasm32-unknown-unknown` by Trunk into `crates/mate-specviz-client/dist/`
  (gitignored), which the server embeds.

## Entry points

| How | Where | Behavior |
|---|---|---|
| `mate specviz [-C <dir>] [--port <port>]` | `mate-cli/src/specviz.rs` | Foreground. Branches in `main.rs` right after arg parsing — before config loading, the first-run notice, backend and MCP setup — so it needs no API token. Prints one `specviz serving <root> at <url>` line, serves until Ctrl+C. Default port `7732`. |
| `/specviz` in the TUI | `mate-tui/src/specviz.rs`, `App::start_specviz` | Background. Binds `127.0.0.1:0` inline, prints the URL as an `Entry::System` line, then builds and serves in a spawned task. One viewer per canonical workspace root (a repeat re-prints the URL). A viewer that fails later reports a `ViewerExit` through a channel `run_loop` selects on; it becomes a `SystemError` in the tab that started it, and the next `/specviz` for that root starts fresh. Never opens a browser. |

The library API (`mate_specviz_server::viewer`): `bind(addr)` → listener,
`url_of(&listener)`, `run(listener, root, shutdown)` (foreground), and
`spawn(listener, root)` → `(Viewer, JoinHandle)` (background; dropping the
`Viewer` stops it). Binding is separate from serving so the URL is known
before the potentially slow initial index walk. Shutdown is not graceful:
open SSE streams never end, so waiting on them would never return.

The library never writes to stdout/stderr — everything goes through
`tracing`, since the TUI owns the terminal. `mate-cli` prints the one
foreground startup line itself.

## Sources

`SandboxedRoot` (`infra/sandbox.rs`) canonicalizes the served root and
records which sources exist **at startup**: `specs/` (`DocKind::Plain`) and
`openspec/` (`DocKind::OpenSpec`). A source created later is only picked up
on the next start. `SpecId` is the slash path relative to the served root
(`specs/a.md`, `openspec/changes/x/tasks.md`). `resolve` accepts only `.md`
files whose canonical path is inside a source's canonical directory —
`..`, absolute paths, and symlink escapes all come back as not found.

The tree (`GET /api/specs`) is a list of sections:

- `plain` — `specs/`, mirroring its directory layout.
- `capabilities` — each `openspec/specs/<path>/spec.md`, with its
  `### Requirement:` count.
- `changes` — each directory under `openspec/changes/` except `archive`,
  with task progress (`- [x]` vs. total task items in `tasks.md`) and its
  Markdown files ordered proposal, delta specs, design, tasks, then the rest.
- `archive` — `openspec/changes/archive/*`, newest first.

The three OpenSpec sections are always present when `openspec/` is. The
parsing is pure code in `domain/openspec.rs` — never shells out to the
`openspec` CLI.

## Rendering

`infra/markdown.rs`: pulldown-cmark events → (OpenSpec docs only) an event
pass adding `sv-*` classed markup → syntect highlighting of fenced code →
`push_html` → ammonia. The OpenSpec pass turns delta headings into
`h2.sv-delta.sv-delta-{added,modified,removed,renamed}` with a
`span.sv-badge`, requirement headings into `h3.sv-req`, wraps each
`#### Scenario:` block (up to the next heading of level 1–4) in
`section.sv-scenario`, and emphasizes a scenario bullet's leading
`WHEN`/`THEN`/`AND` as `strong.sv-kw`. Ammonia allows `class` only with
exactly those names (`SV_CLASSES`); anything authored is stripped.

The `sv-*` styles live in `mate-specviz-client/style/input.css` under
`@layer components` with `@apply`. That is the one exception to the
utilities-inline rule below: the server emits these classes, so Tailwind's
content scan of `src/` never sees them and would drop plain utilities from
the release build.

## Live reload

One `notify` watcher over every source directory. `bootstrap::classify_fs_event`
maps a content-only edit of one existing file to `InvalidateSpec` (SSE
`{"kind":"spec","id":..}`) — except OpenSpec `tasks.md`/`spec.md`, which
feed tree counts and so trigger `RefreshSpecIndex` (SSE `{"kind":"index"}`),
as does anything that can reshape the tree.

## Server layering (CQRS)

```
domain/        pure types + OpenSpec parsing, no I/O, no async
application/   commands (mutate the index), queries (read-only), the two buses
infra/         filesystem, sandbox, markdown rendering, watcher
web/           axum routes/state/SSE/assets — the only caller of the buses
viewer.rs      public start/serve API; bootstrap.rs wires the layers
```

Dependencies point one way: `web` → `application` → `domain`; `infra`
implements what `application` needs. New pure shape → `domain/`; new read →
`application/queries/`; new mutation → `application/commands/`; new I/O →
`infra/`; new route → `web/routes.rs` via `AppState`, never touching `infra/`
directly. Errors are `thiserror` enums at each boundary.

## Client (Leptos CSR)

CSR only — no SSR, no server functions. All HTTP goes through `api.rs`
(`gloo-net`), whose types mirror the server's JSON exactly. Markdown is
never parsed client-side; the server sends ready HTML set via `inner_html`.

Atomic design under `src/components/`, one `#[component]` per file:
`atoms` (Icon, Badge) → `molecules` (TreeItem, Breadcrumbs) → `organisms`
(Sidebar, SpecContent — may own their fetch) → `templates` (AppShell, no
data) → `pages` (SpecPage, routed). A component only imports from its own
tier or below. Use `#[prop(into)]` for string props; `LocalResource` +
`<Suspense>` for fetches, rendering loading/error/success explicitly.

Tailwind: utility classes inline in `view!`, written literally (never
`format!("text-{c}-500")`), default `slate` scale, ordered layout → spacing
→ border/background → typography → color → state variants.

Comments in both crates are doc comments only (`//!`, `///`) — no inline
`//` explanations; extract a function or document the item instead.

## Host builds vs. the wasm build

The client's dependencies sit under
`[target.'cfg(target_arch = "wasm32")'.dependencies]` and its `lib.rs` is
`#![cfg(target_arch = "wasm32")]`, so host `cargo clippy --workspace` /
`nextest --workspace` compile it as an empty crate. It's linted separately
with `cargo clippy -p mate-specviz-client --target wasm32-unknown-unknown`.

The server's embed uses `#[allow_missing = true]`: without a Trunk build,
`dist/` doesn't exist and the static handler serves a "UI not built" page
(the JSON API still works). The release workflow runs `trunk build
--release` first and fails if `dist/index.html` is missing.

## Local development

```
cd crates/mate-specviz-client && trunk watch   # rebuilds dist/ on change
mate specviz                                   # debug builds read dist/ live
```
