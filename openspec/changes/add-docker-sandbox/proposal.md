# Proposal

## Why

`mate` runs every agent turn in-process against the host filesystem, jailed
only by `ToolCtx`'s path checks. There is no way today to hand a workspace to
a stronger, OS-level sandbox and drive a different agent (Claude Code) inside
it from the same TUI. Running Claude Code inside a hardened Docker container
gives a disposable, resource-limited, capability-dropped environment bound to
a single host workspace, while keeping the conversation resumable across
detaches, container restarts, and `mate` relaunches.

## What Changes

- New `mate-sandbox` crate: a thin Docker Engine API client wrapper (no
  `docker` CLI shell-outs), a sandbox policy type with a pure translation to
  the container's host config, a persistent sandbox-session store, startup
  reconciliation against Docker, and an inspection report builder.
- A **sandbox session** is a long-lived container created from a configurable
  image (default `ghcr.io/leoborai/dkc:latest`), named `dkc-<short-id>`,
  labelled `managed-by=mate` + `session-id=<id>`, kept alive with a no-op
  command, mounting the chosen host workspace read-write at `/workspace` and a
  per-session named volume `dkc-claude-<id>` for Claude Code's state.
- **Authentication is Claude Code's own subscription login flow**, performed
  interactively inside the container on first attach. No API key or other
  secret is passed through the container's environment, labels, the session
  store, or logs. Login state lives only in that session's volume.
- **Interactive attach** runs Claude Code inside a `tmux` session in the
  container, so detaching (host-side `Ctrl-\`) leaves Claude Code — including
  a turn in progress — running, and re-attaching rejoins the same terminal.
  If the tmux session is gone (container restarted, Claude exited),
  re-attaching starts Claude Code again with `--resume <claude-session-id>`.
- **Headless turns** run `claude -p --output-format json [--resume <id>]` in
  the container and show the parsed result in the sandbox's tab. A headless
  turn is refused while that sandbox's tmux session is alive, so a transcript
  never has two writers.
- **Inspection** view per sandbox, separating *configured* values (from
  container inspect: state, network, mounts, limits/hardening, env key
  names only) from *observed* values (one-shot stats, container-layer
  filesystem changes, process list).
- **TUI**: the tab list becomes a list of two tab kinds — the existing agent
  tabs and new sandbox tabs. Every stored sandbox reopens as a tab on startup.
  Sandbox actions are slash commands inside a sandbox tab (`/attach`,
  `/inspect`, `/stop`, `/rm`); plain input on a sandbox tab is a headless
  prompt. The `Ctrl+T` spawn form gains an agent/sandbox kind choice.
- Network mode is chosen at creation only (default `bridge`, since Claude Code
  must reach Anthropic). There is no runtime network toggle.
- Containers are left running when `mate` exits. Stop and remove are explicit
  user actions; removing the session's volume is a separate, confirmed step.
- **Image contract** (outside this repo): the image must ship `tmux` and a
  non-root user with a writable home. This change documents the contract and
  checks for it; it does not modify the image.

## Capabilities

### New Capabilities

- `docker-sandbox`: creating, persisting, reconciling, attaching to, running
  headless turns in, inspecting, stopping, and removing Docker-sandboxed
  Claude Code sessions from the `mate` TUI, including the container hardening
  and secret-handling guarantees those sessions carry.

### Modified Capabilities

None. No main specs exist yet under `openspec/specs/`.

## Impact

- **New crate** `crates/mate-sandbox` and a new external dependency
  (`bollard`, the Docker Engine API client), plus `uuid` as a direct
  dependency (Claude Code's `--session-id` takes a UUID; the workspace
  otherwise uses `ulid`).
- **`mate-tui`**: `App.tabs` changes from `Vec<SessionTab>` to a two-variant
  tab enum; a second event receiver for sandbox events joins the `select!`
  loop; the terminal is handed off to and back from the attach routine; new
  slash commands; the spawn form gains a kind toggle.
- **`mate-cli`**: `tui.rs` loads the sandbox store, starts reconciliation, and
  passes the sandbox event channel into `mate_tui::run`; config gains a
  `[sandbox]` section (image, default limits).
- **Persistence**: a new `sandboxes.json` under `mate`'s state directory
  (`$XDG_STATE_HOME/mate`).
- **Host requirements**: a reachable Docker daemon (respecting `DOCKER_HOST`).
  Without one, sandbox tabs show an error; agent tabs are unaffected.
- **Docs**: `.agents/docs/architecture.md` (new crate, graph), `config.md`
  (`[sandbox]`), and a new `.agents/docs/sandbox.md`.
