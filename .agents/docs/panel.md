# The agent status panel (`mate-tui`, `M12`)

Read this when touching `panel.rs`, `panel_widgets.rs`, `roster.rs`, or the
telemetry routing that feeds them (`session.rs`'s `Activity` forwarding,
`cost.rs`). For Ratatui idioms in general, see
`.agents/skills/mate-software-engineer/refs/ratatui.md`; this doc is the
panel's own data flow and layout logic specifically.

## Where the state lives

Everything the panel shows is **per-tab, on `SessionTab`, never on `App`**.
A tab rooted at `~/work/api` showing another tab's file reads would be
actively misleading, so there is exactly one panel per tab and switching
tabs swaps its entire backing state. Concretely, the state is split across
three places, not one:

- **`crate::panel::Panel`** — the ring-buffered network/documents logs
  (`VecDeque<NetRow>`, `VecDeque<DocRow>`, both capped at `RING_CAPACITY =
  50`) plus `turn_requests: u32`, the network widget's per-turn header
  count. Fed by `Panel::push(agent, activity)` from `ToolActivity` records.
- **`crate::roster::Roster`** — the subagent roster (`VecDeque<SubagentRow>`,
  capped at `ROSTER_CAPACITY = 32`, of which `ROSTER_SHOWN = 8` render).
- **`crate::app::SessionTab`** — everything else the widgets read: model,
  provider, `UsageRollup`, `CostEstimate`, panel visibility, `PanelFocus`.

`crate::panel_widgets::PanelView<'a>` is a **borrowed snapshot** of all three,
built fresh every frame in `App::view` — nothing in `panel_widgets.rs` owns
state; every `PanelWidget` impl is a stateless unit struct. This split keeps
rendering logic (which can freely borrow) separate from the mutation logic
(which needs `&mut` and happens on event/key handling), and it's why adding
a new widget never means adding an `on_key` method per widget — see below.

## How telemetry gets here

A tool emits `ToolActivity` into `ctx.activity` (an `mpsc::Sender<(AgentId,
ToolActivity)>`, `tools.md`). `mate-core::session::SessionManager::spawn`
drains that sink onto the session's one shared `SessionEvent` channel,
wrapped as `AgentEvent::Activity(activity)` and tagged with whichever
`AgentId` produced it — root or subagent, both share the one sink. `mate-tui`
routes that event to `Panel::push` **and** (if `agent != AgentId::ROOT`)
`Roster::note_activity`, so one tool call updates both the network/documents
log and the subagent's one-line activity string from a single record. Ditto
`AgentEvent::Usage` tagged with a non-root id, which drives
`Roster::record_turn` and folds into `UsageRollup::subagents` via
`record_subagent_turn` (`streaming.md`).

**Never derive the roster's activity text from the model.** `roster::
derive_activity` maps `ToolActivity` variants to short present-tense strings
(`FileTouched{Read} → "reading build.rs"`, `NetRequest{reason:None} → "GET
docs.rs"`, `NetRequest{reason:Some(_)} → "blocked <host>"`) mechanically.
Asking the subagent to narrate itself would cost tokens, add latency, and
produce a confidently wrong description of something it didn't actually do;
deriving from the tool call it just made is free and can't lie.

## The widget framework (`panel_widgets.rs`)

```rust
pub(crate) trait PanelWidget {
    fn title(&self) -> &str;
    fn size(&self, view: &PanelView<'_>) -> WidgetSize;   // { ideal: u16, min: u16 }
    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, collapsed: bool);
}
```

No `on_key` on the trait. Every panel key (`Tab`, arrows, `Enter`, `x`) is
routed centrally through `App::on_key`, the same place every other app-level
key already is — a widget never dispatches its own input. If you're adding
panel interactivity, it goes in `App`, not in a new trait method.

`AgentStatusPanel::new()` registers the five widgets in a fixed order
(`ModelWidget`, `ContextWidget`, `SubagentRosterWidget`, `NetworkLogWidget`,
`DocumentsLogWidget`) — this order **is** the priority order used below.

### The vertical-budget allocator (`allocate_list_heights`, `M12-3`)

`Model` (3 rows) and `Context` (4 rows) are fixed and always render in full
(clamped only if the *whole panel* is shorter than 7 rows). Whatever's left
goes to the three list widgets via a function deliberately kept pure and
`Frame`-free so it's unit-testable in isolation:

```rust
fn allocate_list_heights(
    list_budget: u16,
    order: [PanelWidgetKind; 3],   // priority order, highest first
    sizes: [&WidgetSize; 3],       // [subagents, network, documents], regardless of `order`
) -> [u16; 3]                      // returned in that same fixed shape
```

Two passes over `order`, both in priority order:

1. **Floors first** — walk `order`, give each widget `min(min_rows,
   remaining)`. A higher-priority widget's floor is guaranteed before a
   lower-priority one gets anything at all.
2. **Grow toward ideal** — walk `order` again, give each widget whatever's
   left, up to its `ideal`.

Default priority is `[Subagents, Network, Documents]` — i.e. **documents
collapse first, then network, then subagents** — because the subagent roster
is the liveness signal during a delegation fan-out; without it a 90-second
`spawn_agent` burst reads as a hang. **A focused widget is rotated to the
front of `order` before allocation** (`order[..=pos].rotate_right(1)` in
`AgentStatusPanel::render`), so `Ctrl+P`-focusing Documents and starving the
panel for space makes Documents win the scarce rows instead of Subagents —
"expands on focus" beats the static priority.

A widget that got less than its `ideal` height renders `collapsed: true`,
which today just appends `▸` to its header row
(`panel_header_marker`) — the widget itself still decides how to spend
whatever rows it got; `collapsed` is advisory, not a hard "render nothing."

### Sizing per widget

| Widget | `ideal` | `min` |
|---|---|---|
| Model | 3 (fixed) | 3 |
| Context | 4 (fixed) | 4 |
| Subagents | `1 + min(len, ROSTER_SHOWN)` | `2` if any rows, else `1` |
| Network | `1 + min(len, NETWORK_SHOWN=6)` | `1` |
| Documents | `1 + min(len, DOCUMENTS_SHOWN=6)` | `1` |

## Per-widget notes

- **`ModelWidget`** — always exactly 3 lines regardless of content, so it
  never fights the allocator for variable space. Model id truncated from the
  *left* (`truncate_left`) since the distinguishing part of a model id is
  usually its suffix. Shows `sub: <model>` only when the subagent model
  differs from the root's.
- **`ContextWidget`** — `Enter` toggles `context_split` (root vs. subagent
  breakdown), handled in `App`, not as a row focus (§9.5 has no rows to
  focus here — it's a whole-widget toggle). `running_turn` adds a `+…`
  suffix instead of a fake live token estimate; counts only update on
  `FinalUsage`. `format_cost` prints `~$? · unpriced model, see [pricing]`
  when `CostEstimate::known` is `false` — never a number that merely *looks*
  real (see `cost.rs`, below).
- **`SubagentRosterWidget`** — **one line per subagent is a hard constraint**
  enforced by `SubagentRow` having no second-line field to grow into, not by
  convention. `subagent_row_line` computes the activity-text budget from the
  row's *actual* rendered width (`width - glyph - label(10) - elapsed -
  fixed spacing`), so a 60-char path and a 40-char label still fit on one
  line via `truncate_end`. Header shows `running/total` plus `+n more` when
  the roster holds more than what's shown. `AwaitingApproval` rows render in
  yellow; a focused row is reverse-video.
- **`NetworkLogWidget`** — header count is `network_turn_requests`
  (`Panel::turn_requests`, reset once per prompt sent — "is this thing
  hammering something *right now*"). Status-coded: 2xx dim green, 3xx dim,
  4xx/5xx and blocked (`BLK`, no status) red. A subagent's row is prefixed
  `[<id>] ` (`subagent_prefix`) so the log stays legible when root and
  subagent traffic interleave.
- **`DocumentsLogWidget`** — `R`/`W`/`+`/`−` per `FileOp`; path is rendered
  relative to the session root and middle-truncated (`middle_truncate`) so
  the filename — the identifying part — always survives even when the
  directory prefix doesn't fit.

## Cost estimation (`mate-core/src/cost.rs`, `M11-6`)

HuggingFace's completion response carries only `usage: Usage` — no billing
metadata on the response body or headers, for any provider partner Rig
0.41.0 supports — so a user-maintained `[pricing]` table (`ModelRate {
input_per_million, output_per_million }`) is the *only* path, not a fallback
for one that doesn't exist yet. `estimate_cost(rollup, root_model,
subagent_model, pricing)`:

- Looks up `root_model` in `pricing` always; looks up `subagent_model` **only
  if** `rollup.subagents.total_tokens > 0` — a session that never delegated
  must not report `known: false` just because its (unused) configured
  subagent model happens to have no price entry.
- `known` is `false` the instant either lookup that's actually needed comes
  back empty — and `known: false` always means `total_usd: 0.0`, `known:
  false`, never a silently-real-looking zero.
- `per_turn_avg = total / rollup.turns`, guarded against `turns == 0`. This
  is "cost per completed *root* turn over the whole session" — the number
  that predicts what the *next* question costs — not a lifetime total.

`ModelRate` is deliberately not `mate-cli::config::PricingEntry` — `mate-core`
can't depend on the CLI-facing config shape (`config.md`), so it defines the
minimal pair of numbers the math needs and leaves TOML loading to `mate-cli`.

## Keys (implemented so far)

| Key | Effect |
|---|---|
| `Ctrl+B` | Toggle the panel for the active tab |
| `Ctrl+P` | Toggle panel focus — focuses `ModelWidget` if nothing was focused, releases back to the input otherwise |
| `Tab` / `Shift+Tab` (panel focused) | Cycle `PanelWidgetKind` via `.next()`/`.prev()` |
| `↑`/`↓` (a list widget focused) | Move the focused row within that widget |
| `Enter` (panel focused) | Widget-dependent: toggles `ContextWidget`'s split; other widgets' detail modals are not yet implemented |
| `x` (Subagents focused) | Cancels the focused subagent via `SubagentRunner::cancel` (`delegation.md`) |
| Any printable char (panel focused) | Returns focus to the input and inserts the character there — there is no code path that routes typed text into a subagent |

## Testing patterns

- `Panel`/`Roster` mutation logic is tested directly with hand-built
  `ToolActivity` values — no `Frame`, no terminal.
- `allocate_list_heights` is tested directly as a pure function: generous
  budget → everyone gets `ideal`; tight budget → floors are respected in
  priority order; a focused widget jumps the priority queue.
- `ui.rs` (actual `Frame` rendering: layout, tab bar, transcript/input
  widgets) has **no** test module — see `testing.md` and
  `refs/ratatui.md` for why, and don't add one back.
