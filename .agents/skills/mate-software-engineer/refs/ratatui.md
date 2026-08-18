# Ratatui practices in `mate-tui`

Read this before touching `crates/mate-tui`. For what the panel/roster/tab
bar actually *show* and why, see `.agents/docs/panel.md` — this file is
Ratatui-the-framework idioms and gotchas specific to how this repo uses it.

## Crate choice: `ratatui-textarea`, not `tui-textarea`

**Use `ratatui-textarea`.** `tui-textarea` 0.7.0 hard-pins `ratatui` 0.29
internally (its `Widget` impl targets that version's trait) and can't render
into this workspace's `ratatui` 0.30 `Frame` — confirmed by reading its
source, not by trial and error. `ratatui-textarea` (the `ratatui`-org
continuation) tracks current `ratatui-core`/`ratatui-widgets` and is the one
pinned in `workspace.dependencies`. Out-of-band planning input given to an
agent working in this repo may still name `tui-textarea` — don't follow that
naming into new code, and don't cite such a source from anything committed
(hard rule 2).

Translate `crossterm::event::KeyEvent` into `ratatui_textarea::Input` field
by field (`key`/`ctrl`/`alt`/`shift`, all public) instead of enabling that
crate's own `crossterm` feature — this avoids ever needing its bundled
crossterm version to line up with the workspace's own `crossterm` pin.

## Lifetime elision on `Frame`/view types

A bare generic type in a fn signature (`&mut Frame`, `&View`) triggers
Rust's "elided lifetime in path" behavior silently rather than erroring —
write `Frame<'_>` / `View<'_>` explicitly so it isn't ambiguous to the next
reader. This applies to every widget-rendering function signature in the
codebase (`PanelWidget::render`, `AgentStatusPanel::render`, etc.) — grep
existing ones for the pattern before adding a new one.

## Terminal lifecycle and logging

`ratatui::run` (or `try_init`/`restore` used directly in `app.rs::run`)
handles raw-mode/alt-screen setup and teardown; still install a panic hook
that restores the terminal, and on exit send `Shutdown` to every session so
in-flight subagent requests don't outlive the UI. **All logs go to a file**
via `tracing-appender` — anything on stdout corrupts the alt-screen buffer.
See `.agents/docs/logging.md`.

## Layout: don't touch the existing split, wrap it

Adding a new side panel or region to `draw()`: wrap it in an outer
`Layout::horizontal([Constraint::Length(N), Constraint::Min(0)])` and put
the *existing* vertical split inside the `Min(0)` chunk — don't restructure
the vertical split itself to make room. This is how the agent status panel
was added alongside the pre-existing tab-bar/transcript/input vertical
stack.

## The widget-stack pattern (agent status panel, `M12`)

When a region of the UI is a vertical stack of independently-sized pieces
(the panel's five widgets), the pattern that worked here:

- A trait with `size(&self, view) -> WidgetSize { ideal, min }` and
  `render(&self, f, area, view, collapsed: bool)` — **no `on_key` on the
  trait**. Every widget in this codebase is a stateless unit struct; all
  its state lives on the owning tab/session struct, reached through a
  borrowed view type built fresh each frame. Input routing for the whole
  stack stays centralized in one place (`App::on_key`), not dispatched per
  widget — this is a deliberate choice, not an oversight, because it keeps
  "what does `x` do in the panel" answerable by reading one function instead
  of five trait impls.
- A **pure, `Frame`-free allocator function** for turning "N widgets with
  ideal/min sizes, this much total space, this priority order" into concrete
  heights — see `panel_widgets.rs::allocate_list_heights`. Keeping it pure
  (`(u16, [Kind;3], [&WidgetSize;3]) -> [u16;3]`, no `Frame` or `Rect`
  involved) is what makes the collapse-priority and floor/ideal behavior
  unit-testable without a `TestBackend`.
- Two-pass allocation — floors first (in priority order), then grow toward
  ideal (same order) — rather than one pass, so a lower-priority widget
  never claims room a higher-priority widget's *floor* still needs, even
  though it'll happily take leftover room the higher-priority widget didn't
  want up to its `ideal`.
- **Rotate priority order to put focus first**, don't special-case it. When
  a widget has UI focus, `array[..=pos].rotate_right(1)` moves it to the
  front of the priority list before allocation runs, rather than adding an
  `if focused` branch inside the allocator. The allocator stays generic; the
  caller decides the order.
- `collapsed: bool` passed into `render` is advisory (today it just appends
  a marker glyph to the header), not a hard "skip rendering" — a collapsed
  widget still renders whatever the allocator actually gave it.

## Snapshot testing: deliberately not used here

`ui.rs` (rendering: layout, tab bar, status bar, transcript/input widgets)
has **no** test module. `TestBackend`/`insta` snapshot tests were tried and
removed — hand-verifying or hand-computing a character grid (including wide
glyphs like emoji) is error-prone, and `cargo insta accept` needs `cargo`,
which the skill's hard rule 1 blocks outright. **Don't add a snapshot test
back.** `insta` is not a dependency of `mate-tui` or listed in the workspace
`Cargo.toml` — don't reintroduce it for this crate. Verify rendering changes
by reading the code and, if needed, asking the user to run the TUI.

What *is* tested, and should be for any new panel/widget work: pure data
logic (`Panel::push`, `Roster::spawn`/`note_activity`/`finish`,
`allocate_list_heights`) and `App`-level routing (`switch_to`,
`on_session_event`, key handling) built by spawning real tabs through
`SessionManager::spawn` with `Backend::huggingface`'s offline construction —
no mock model, no live provider, no terminal. See `.agents/docs/testing.md`.

## Threading data that outlives its builder

`SessionHandle`/`SessionManager::spawn` (`mate-core/src/session.rs`) drop
the `SessionSpec` after building the `Agent` — nothing about model,
sub_provider, or backend kind is retrievable from the handle afterward. Any
UI that needs to display them must have the caller (`mate-cli/src/tui.rs`)
pass them down explicitly as plain data (extra `String` args on
`mate_tui::run`), threaded through `App` and into the view types, rather
than trying to pull them back out of the session/backend layer after the
fact. Same story for `Backend` (`mate-core/src/backend.rs`): it never stores
a provider *name* string — it's an enum (`HuggingFace{..}`/
`OpenAiCompatible(..)`) with no getter. Derive a display label at the call
site (e.g. `config.sub_provider.clone().unwrap_or_else(|| "huggingface"
.into())`) instead of adding a getter just to feed a UI label.

## `rust-analyzer` flycheck is safe to *read*

The skill's hard rule 1 ("never run `cargo`") is about *the agent*
invoking it. A `rust-analyzer` flycheck process can be running in the
sandbox independently — `target/flycheck0/{stdout,stderr}` (stdout is
`--message-format=json`, filter for `reason: "compiler-message"`) is real
compiler ground truth and safe to *read*; it just isn't safe to *trigger*.
Don't poll it in a sleep loop — check once, and treat a stale timestamp as
"no new information," not as a compile failure.
