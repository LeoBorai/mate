# Support Skills

Plan for issue #59. Adds Agent Skills to `mate`: filesystem-discovered
`SKILL.md` packages under `.claude/skills/` and `.agents/skills/`, loaded
on demand by a new `skill` tool. Self-contained — no code, doc, or commit
message should point back at this file once the work lands (see the "never
reference an out-of-band planning doc" rule in
`.agents/skills/mate-software-engineer/SKILL.md`). Any tag like `S1`, `S2`
below is defined here and nowhere else.

## Research: how other agents do this

**Claude Code / Claude API (Anthropic).** A Skill is a directory with a
`SKILL.md`: YAML frontmatter (`name`, `description`, both required) plus a
Markdown body. Three-level progressive disclosure:

1. *Metadata* — every Skill's `name`+`description` is loaded at startup and
   put in the system prompt (~100 tokens each). `description` is the only
   thing the model matches your request against, so it must say both what
   the Skill does and when to use it.
2. *Instructions* — the full `SKILL.md` body loads only once the model
   decides (or is told) to use that Skill.
3. *Resources/code* — anything the body references (`FORMS.md`, a
   `scripts/*.py`) loads only when actually touched; scripts run via bash
   and only their output enters context, never their source.

Mechanically, on the Claude API / Claude Code this is not a dedicated
"skill" tool — Claude has a general bash tool and just does
`cat pdf-processing/SKILL.md`, `cat FORMS.md`, `python scripts/fill_form.py`
itself. `name`: ≤64 chars, `[a-z0-9-]+`, no XML tags, can't contain
"claude"/"anthropic". `description`: non-empty, ≤1024 chars, no XML tags.
Discovery paths: `~/.claude/skills/` (personal), `.claude/skills/`
(project). (Sources:
[Agent Skills overview](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview),
[equipping-agents-for-the-real-world-with-agent-skills](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills).)

**OpenCode.** Same `SKILL.md` shape (`name`: `^[a-z0-9]+(-[a-z0-9]+)*$` ≤64
chars, `description` ≤1024 chars, both required; optional `license`,
`compatibility`, `metadata`). Two differences that matter for `mate`:

- OpenCode has **no bash tool it always trusts** the same way Claude Code
  does, so it exposes a dedicated, typed **`skill` tool** —
  `skill({ name: "git-release" })` — that returns the full `SKILL.md`
  content. Discovery is still injected as a lightweight
  `<available_skills>` list in the system prompt (name + description per
  skill); the tool call is what loads level 2.
- Discovery walks **multiple directories with precedence**, project first
  then global: `.opencode/skills/`, `.claude/skills/`, `.agents/skills/`
  (walked from cwd up to the repo root), then the `~/.config/opencode`,
  `~/.claude`, `~/.agents` equivalents. First match for a given skill name
  wins. (Source: [opencode.ai/docs/skills](https://opencode.ai/docs/skills/).)

**Takeaway for `mate`.** `mate` has no shell/exec tool at all (`mate-tool-fs`
is read-only: `read_file`/`list_dir`/`find_files`) and isn't going to grow
one for this. So the OpenCode shape is the closer fit: a dedicated `skill`
tool for level 2 (rather than "the model does `cat` itself"), and level 3
resource files served by the `read_file`/`find_files` tools `mate` already
has, not by a new mechanism. Bundled *scripts* are out of scope — there is
nothing in `mate` that can execute one (see Non-goals).

## Format `mate` supports

A Skill is a directory containing `SKILL.md`:

```markdown
---
name: pdf-processing
description: Extract text and tables from PDF files. Use when the user
  mentions PDFs or document extraction.
---

# PDF Processing

Instructions here. Reference other files in this skill's own directory
with a normal relative-to-workspace-root path, e.g. `FORMS.md` next to
this file — read it with `read_file`.
```

Required frontmatter fields: `name`, `description` — same constraints as
Claude Code's (`name`: ≤64 chars, `[a-z0-9]` + `-`; `description`:
non-empty, ≤1024 chars). Optional fields (`license`, `compatibility`,
`metadata`) are parsed-and-ignored for `S1` — no consumer for them yet, and
inventing one now would be scope creep.

## Where Skills are discovered

Per the ask: **`.claude/skills/<name>/SKILL.md`** and
**`.agents/skills/<name>/SKILL.md`**, both relative to the session's own
workspace root (`ToolCtx::root` / `SessionSpec::root` — the same root
`read_file` is already jailed to). Not global (`~/.claude/skills`, …) —
out of scope, see Non-goals.

Precedence when the same `name` appears in both: `.claude/skills` wins,
`.agents/skills` loses (matches OpenCode's ordering, and matches this
repo's own layout — `.claude/skills/mate-software-engineer` is already a
symlink into `.agents/skills/mate-software-engineer`, so the two trees are
expected to overlap on purpose). A malformed skill (missing/invalid
`name` or `description`, unreadable `SKILL.md`) is skipped with a
`tracing::warn!`, not a hard failure — one bad skill directory shouldn't
break the session.

## Crate: `mate-tool-skills`

New crate, same shape as `mate-tool-fs`/`mate-tool-agent`:
`mate-tool-skills → mate-tool-api` only (never `mate-core`, per the
existing dependency-graph rule).

```
crates/mate-tool-skills/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── discovery.rs   # discover_skills(root: &Path) -> Vec<SkillMetadata>
    ├── frontmatter.rs # parse_frontmatter(&str) -> Result<(name, description), ...>
    └── skill.rs        # the `skill` PortableTool
```

**`discovery.rs`.** `discover_skills(root)`:

1. For each of `.claude/skills`, `.opencode/skills`, `.copilot/skills`, `.agents/skills` (in that precedence
   order) under `root`: `std::fs::read_dir` one level, and for every
   subdirectory that contains a `SKILL.md`, parse its frontmatter.
2. Fold into a `Vec<SkillMetadata>` keyed by `name`, first source wins on
   collision (`.claude/skills` first, so it wins — see Precedence above).
3. Sort by `name` for deterministic preamble/tool-listing output and
   stable tests.

No `ignore`/gitignore-aware walk needed here (unlike `find_files`) — this
is a fixed, one-level directory shape, not a general glob.

**`frontmatter.rs`.** The only two fields that matter (`name`,
`description`) are flat YAML scalars in practice. Rather than add a new
YAML dependency, hand-roll the narrow subset actually needed: split the
file on the leading `---`/`---` delimiters, then per line inside split on
the first `:`, trim, strip one layer of surrounding `"`/`'` quoting.
Reject anything that doesn't parse into at least `name` and `description`.

This is a real trade-off, worth calling out explicitly:

- *Recommended (hand-rolled):* zero new dependency, zero `deny.toml`
  license review, covers every real-world `SKILL.md` seen in Claude Code's
  and OpenCode's own examples (flat scalars, occasionally quoted).
  Doesn't handle YAML block/folded scalars (`description: |`) or a
  `description` containing a literal unescaped `\n`.
- *Alternative:* depend on a maintained YAML crate (the original
  `serde_yaml` is unmaintained; `yaml-serde`, the YAML-org-published fork,
  is the closest drop-in today) and deserialize into
  `{ name: String, description: String }`. Costs one new workspace
  dependency + a `deny.toml` license check, buys full YAML correctness.

Recommendation: start hand-rolled (`S1`); swap in a real parser later only
if a real skill author actually needs block scalars — don't pay for that
generality up front.

**`skill.rs`.** The tool:

```rust
pub struct SkillArgs {
    /// Exact `name` of a skill from the "Available skills" list.
    pub name: String,
}
```

`NAME = "skill"`. `call()`: look up `name` in `ctx.skills` (see `ToolCtx`
change below); if absent, `ToolFailure::NotFound`. Otherwise read the
skill's `SKILL.md` (`enforce_max_size`/`refuse_binary` the same as
`read_file`), strip the frontmatter block, and return:

```
Skill directory: .claude/skills/pdf-processing (read bundled files from here with read_file/find_files)

<body markdown>
```

The leading "Skill directory" line is the level-3 bridge: since `mate` has
no bash tool for the model to `cat FORMS.md` itself, it needs the
workspace-root-relative path of the skill's own directory so it can
construct a correct `read_file` path for anything the body references.
Emit a `ToolActivity::Note { text: "skill `pdf-processing` loaded" }` on
the existing `ctx.activity` sink — reuses the catch-all `Note` variant
already in `mate-tool-api::activity`, no new `ToolActivity` variant needed.

## Wiring into `mate-core`

**`mate-tool-api::ctx` (new type + field).** Add `SkillMetadata` next to
`AgentId`/`Approvals` (types `ToolCtx` itself needs, defined in
`mate-tool-api` precisely so a tool crate can depend on `mate-tool-api`
without `mate-tool-api` ever depending on a tool crate):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    /// Workspace-root-relative directory, e.g. ".claude/skills/pdf-processing".
    pub dir: PathBuf,
}
```

`ToolCtx` gains `pub skills: Arc<[SkillMetadata]>`. `mate-tool-skills`
depends on `mate-tool-api` and both discovers (`Vec<SkillMetadata>`,
built via its own richer discovery logic) and consumes
(`ctx.skills`) the same type — same pattern `mate-tool-agent` already
uses for `SubagentSpawner`.

**`mate-core::toolset::build_toolset`.** Attach `mate_tool_skills::Skill`
whenever `!ctx.skills.is_empty()` — same conditional-attachment pattern as
`http_request` (`http_policy.enabled`) and `spawn_agent`
(`ctx.spawner.is_some()`). `tool_descriptors()` gains a `skills_enabled:
bool` (or `skill_count: usize`) parameter so its output keeps matching
`build_toolset`'s real attachment set, same duplication this file already
flags at `toolset.rs`'s top.

**`mate-core::preamble::render_preamble`.** Needs a new, separate section
— skills are not tools, and conflating them into "Available tools" would
misrepresent what each entry is. Add a parameter (`skills:
&[SkillDescriptor]`, mirroring `ToolDescriptor`'s shape) and render:

```
Available skills:
- pdf-processing: Extract text and tables from PDF files. Use when the user mentions PDFs or document extraction.

Load one with the `skill` tool before following its instructions.
```

placed after the "Available tools" block. Empty list ⇒ omit the section
entirely (not a "(none)" placeholder — most workspaces will have zero
skills, and this section showing up empty everywhere would be noise the
"(none)" tools placeholder doesn't have to worry about since tools always
exist).

**Where discovery actually runs.** `ToolCtx` is only ever constructed at
three real (non-test) call sites, and `discover_skills(root)` must run
once at each root-level one, then get carried — not re-run — into every
subagent spawned from it:

1. `mate-cli/src/plain.rs` (plain/headless frontend's root `ToolCtx`)
2. `mate-tui/src/session_factory.rs::build_tool_ctx` (TUI frontend's root
   `ToolCtx`, one call per `-C` root / new tab)
3. `mate-core/src/subagent.rs` (`SubagentRunner`'s per-spawn `ToolCtx`) —
   this one must **not** re-walk the filesystem. `SubagentRunner` already
   carries `self.root`/`self.activity`/`self.http`/`self.approvals`
   forward from the session's root `ToolCtx` into every subagent it
   spawns; give it a `self.skills: Arc<[SkillMetadata]>` captured once at
   `SessionManager::spawn` time (mirrors exactly how `self.http` and
   `self.approvals` are already threaded) and clone it into each
   subagent's `ToolCtx` instead of calling `discover_skills` again.

`(1)` and `(2)` both call `mate_tool_skills::discover_skills(&root)`
directly — a new direct dependency edge from `mate-cli`/`mate-tui` onto
`mate-tool-skills`, same shape as `mate-cli`'s existing direct dependency
on `mate-tool-http` for `HttpShared`.

**Test-helper `ctx()` fns.** Every crate that builds a bare `ToolCtx` in
tests (`mate-tool-fs`, `mate-tool-http`, `mate-tool-agent`,
`mate-tool-api::ctx`, `mate-core::toolset`/`session`/`subagent`,
`mate-tui::app`/`session_factory`, `mate-core/tests/support`) needs one
new field added — mechanical, but wide: grep `ToolCtx {` across
`crates/` to get the full list before starting (11 non-test-module call
sites is that grep at time of writing).

## Config surface

None needed. Unlike `http_request` (SSRF surface — needs a policy toggle)
or `spawn_agent` (needs depth/concurrency guardrails), reading a
`SKILL.md` the workspace itself contains is exactly as sensitive as
`read_file` already reading any other file in the workspace, which has no
enable/disable toggle either. Don't add a `SkillsPolicy` — there's nothing
for it to gate that `.claude/skills`/`.agents/skills` simply not existing
doesn't already gate for free (zero skills discovered ⇒ tool never
attaches).

One thing genuinely worth flagging to whoever reviews this: Anthropic's
own docs warn that a Skill's instructions are attacker-controlled if the
directory came from an untrusted source ("a malicious Skill can direct
Claude to invoke tools... in ways that don't match its stated purpose").
`mate` only ever discovers skills that already live inside the workspace
root the user opened `mate` on — same trust boundary as every file
`read_file` can already read, and same boundary the user already crossed
by pointing `mate` at that directory. No new boundary to add here; worth
one line in whatever user-facing docs `mate` ends up with for Skills, not
a code change.

## Step-by-step

1. `mate-tool-skills` crate: `SkillMetadata` type lands in
   `mate-tool-api` first (so the crate can depend on it), then
   `discovery.rs` + `frontmatter.rs` + tests (fixture directories under
   `tests/fixtures/` or `tempfile::tempdir()`-built trees) — precedence,
   dedup, malformed-frontmatter-skips-with-warning, name/description
   validation, empty-when-no-skill-dirs.
2. `skill.rs` tool + tests — found/not-found, frontmatter-stripped body,
   the "Skill directory:" prefix, the `Note` activity emission, size/binary
   guards reused from `mate-tool-api`.
3. `ToolCtx.skills` field: touch every construction site (mechanical, see
   above); everything still compiles with every site passing
   `Arc::from([])` until step 4 wires real discovery in.
4. `mate-core::toolset`: attach `Skill` conditionally; extend
   `tool_descriptors`.
5. `mate-core::preamble`: `SkillDescriptor` + the "Available skills"
   section + tests (empty list omits the section; non-empty list renders
   it after "Available tools").
6. Wire real `discover_skills` calls into `plain.rs` and
   `session_factory.rs::build_tool_ctx`; thread `self.skills` through
   `SubagentRunner`/`SessionManager::spawn` the way `self.http` already is.
7. `mate-tui` panel/roster: skill loads show up for free once `Note` is
   emitted (no `panel.rs`/`roster.rs` change needed — `Note` already has a
   render arm; verify by reading `roster.rs:178` and `panel.rs:91,162`,
   don't add a second one).
8. Add `crates/mate-tool-skills = { path = "crates/mate-tool-skills" }` to
   `[workspace.dependencies]`; add it as a real dependency of `mate-core`,
   `mate-cli`, `mate-tui`.

## Non-goals (this pass)

- **Executable skill scripts.** No `mate-tool-*` crate can run a process
  today. A `SKILL.md` that says "run `scripts/validate.py`" will read as
  plain instruction text the model can't act on — acceptable for `S1`;
  revisit once/if `mate` grows an exec tool.
- **Global skill directories** (`~/.claude/skills`, `~/.agents/skills`,
  `~/.config/mate/skills`). Only asked for the two project-relative paths.
- **Claude API's `metadata`/`license`/`compatibility` frontmatter fields**
  beyond parse-and-ignore — no consumer.
- **Uploading/syncing Skills anywhere** (Claude API's Skills API, claude.ai
  zip upload) — `mate`'s Skills are filesystem-only, matching Claude
  Code's own filesystem-based custom-Skills model, not the API's.

## Open questions worth a second look before implementing

- Should a subagent's own preamble list skills at all, or is that
  scope the parent should filter (a narrow delegated task rarely needs
  the full project skill list)? Current lean: same list as root, no
  narrowing — skills are workspace knowledge, not a privilege to gate,
  and `mate-core::subagent` doesn't narrow `read_file`'s reach either.
- `SkillArgs.name` validation: reject-with-`ToolFailure::InvalidArgs` vs
  `NotFound` when the model passes a name that isn't in the discovered
  list. Recommendation: `NotFound` — same semantics `read_file` already
  uses for "the model asked for something that isn't there."
