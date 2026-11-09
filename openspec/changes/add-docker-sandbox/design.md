# Design

## Context

See `proposal.md` for motivation and `specs/docker-sandbox/spec.md` for the
behavior contract. Current state that shapes the approach:

- `mate-core::session::SessionManager` is an in-memory registry of Rig agent
  sessions. It has no persistence and its event channel is typed
  `mpsc::Receiver<SessionEvent>`. A sandbox session is a different object
  (a container plus a Claude Code conversation), so it does not reuse it.
- `mate-tui::app::App` holds `tabs: Vec<SessionTab>`, where `SessionTab`
  carries agent-only state (handle, roster, panel, approvals, usage).
  `run_loop` owns a local `crossterm::event::EventStream` and `select!`s over
  it, `app.events`, and a redraw tick.
- `ratatui::try_init` (in `app::run`) installs the panic hook that restores
  raw mode and the alternate screen.
- Slash commands are parsed by the pure `slash::parse`.
- State lives under `mate-cli/src/logging.rs::state_dir()`
  (`$XDG_STATE_HOME/mate` or `~/.local/state/mate`).
- IDs elsewhere are `ulid`; Claude Code's `--session-id` needs a UUID.
- `bollard` is not in `Cargo.lock` or the local registry. This repo forbids
  running `cargo`, so its API must be checked by reading its source in
  `~/.cargo/registry/src/` after the user fetches it.

## Goals / Non-Goals

**Goals:**
- Keep `bollard` out of `mate-core`. All Docker code sits behind one crate.
- Keep the TUI event loop non-blocking: every Docker call runs in a spawned
  task on the existing Tokio runtime and reports back over a channel.
- Make the policy translation, Claude JSON parsing, and reconciliation
  testable without a daemon.

**Non-Goals:**
- Non-Unix hosts for interactive attach (raw fd handling is Unix-only).
  `/attach` on other platforms reports "unsupported". Everything else is
  platform-neutral.
- Modifying the `dkc` image. This change states the image contract and
  checks it at create time.
- Observing which hosts a container contacted or which files it read.
- Runtime network changes.

## Decisions

### D1. New crate `mate-sandbox`

```
cli --> tui --> core --> rig
         |
         +--> mate-sandbox --> bollard
```

Modules: `docker` (bollard wrapper + `DockerApi` trait), `policy`
(`SandboxPolicy`, `policy_to_host_config`), `session` (`SandboxSession`,
`SandboxStatus`), `store` (JSON persistence), `reconcile`, `claude` (command
builders + headless JSON parsing), `report` (`SessionReport`), `error`
(`SandboxError`, thiserror, matching the library-crate convention).

*Alternative:* a module inside `mate-core`. Rejected because it pulls the
Docker client and its dependency tree into every core consumer, and core has
no use for it.

### D2. `DockerApi` trait over a `BollardDocker` impl

The trait has only the operations the spec needs: ensure image (streaming
progress), create/start/stop/remove container, remove volume, inspect image,
inspect container, list by label, create/start/resize/inspect exec, one-shot
stats, changes, and top. `reconcile` and the action layer take
`&dyn DockerApi` (or a generic), so unit tests use an in-memory fake.

Every concrete bollard type, method name, and option struct is taken from
the fetched crate's source (the version is pinned to the latest stable release
when the dependency is added), never from memory.
`Docker::connect_with_socket_defaults()` is the only constructor, so
`DOCKER_HOST` is respected.

Error mapping: connection, socket, and timeout errors become
`SandboxError::DaemonUnavailable`. A 404 on container inspect becomes
`SandboxError::NotFound`, which reconcile turns into `Missing`. Everything
else is `SandboxError::Docker { op, source }`. The TUI renders these as
transcript errors and never panics on them.

### D3. Ownership check before every mutating call

Every stop, remove, and exec first inspects the container and verifies
`managed-by=mate` and `session-id=<id>`. It refuses with
`SandboxError::NotOurs` otherwise. This adds a round-trip but makes "never
touch unlabelled containers" structural instead of relying on stored ids
staying correct.

### D4. Policy and `policy_to_host_config`

`SandboxPolicy { network: NetworkMode::{None, Bridge, Custom(String)},
workspace, extra_mounts: Vec<Mount{source, target, read_only}>, memory_limit,
pids_limit, cpu_limit, read_only_rootfs }`. Defaults: bridge, 4 GiB, 512
PIDs, 2 CPUs (`nano_cpus = 2e9`), `read_only_rootfs = false`.

`policy_to_host_config` is pure. It emits `cap_drop ["ALL"]`,
`security_opt ["no-new-privileges:true"]`, `privileged false`, the limits,
the network mode, binds (`<workspace>:/workspace:rw` plus each extra mount
with an explicit `:ro`/`:rw`), and the state volume mount (D5). A separate
pure `validate_policy` refuses a non-directory workspace and any mount whose
source resolves to the Docker socket (`/var/run/docker.sock`,
`/run/docker.sock`, or the path in a `unix://` `DOCKER_HOST`).

Defaults are configurable through a new `[sandbox]` section in `mate-cli`'s
layered config (`image`, `memory`, `pids`, `cpus`), following `config.md`'s
pattern.

### D5. Claude state volume and `CLAUDE_CONFIG_DIR`

Claude Code keeps credentials and transcripts under `~/.claude/`, but its
account and onboarding file `~/.claude.json` sits beside that directory.
Setting `CLAUDE_CONFIG_DIR` moves both under one directory, so one volume
holds all state.

Create flow:
1. Inspect the image to read `Config.User` and `HOME` from `Config.Env`.
   Fall back to `/home/<user>` if `HOME` is unset.
2. Mount volume `dkc-claude-<id>` at `<HOME>/.claude` and set container env
   `CLAUDE_CONFIG_DIR=<HOME>/.claude`. This value is a path, not a secret.
3. Run as `Config.User`. If the image user is empty or `root`, create still
   proceeds but the tab shows a warning (most conservative option that does
   not block an unconventional image).

Docker copies the image's content and ownership at the mount point into a new
named volume only when the directory exists in the image. The image contract
therefore requires `<HOME>/.claude` to exist and be owned by the image user;
otherwise the volume is root-owned and unwritable. Create checks this by
running `test -w "$CLAUDE_CONFIG_DIR"` in a one-shot exec after start, and
reports a contract violation clearly if it fails.

Env is `CLAUDE_CONFIG_DIR` only. No API key is ever set (login is
interactive).

### D6. Container spec

Name `dkc-<first 8 hex of uuid>`. Labels `managed-by=mate`,
`session-id=<uuid>`. `cmd ["sleep", "infinity"]`, `tty true`,
`open_stdin true`, `working_dir /workspace`, user per D5, `host_config` from
D4. Sessions are keyed by a UUID (`uuid` crate, v4). The same type keeps the
container name, labels, and Claude session id consistent.

### D7. Interactive attach with tmux

Exec command (always, with `tty true`, all three streams attached, and env
`TERM=xterm-256color`):

```
tmux new-session -A -s claude -- claude <claude-args>
```

`-A` attaches if the tmux session `claude` already exists, ignoring the
command. Otherwise it starts Claude Code. The Claude arguments are:
- no `claude_session_id` stored: generate a UUID, persist it **before** the
  exec, and pass `--session-id <uuid>`;
- an id is stored and its transcript exists: `--resume <uuid>`;
- an id is stored but no transcript exists (for example, the user quit during
  login): `--session-id <uuid>` again.

The transcript check is a one-shot exec of `test -f
"$CLAUDE_CONFIG_DIR/projects/-workspace/<uuid>.jsonl"`. Claude Code names a
project's transcript directory after its cwd, and cwd is always `/workspace`.
The check runs only when tmux has no session. It is the one place the design
reads Claude's on-disk layout, and it is a fallback, never the source of the
id.

*Alternative:* plain `claude` per exec, ending it on detach. Rejected: Docker
has no API to re-attach to or kill an exec, so detach would either orphan a
running Claude Code or lose an in-progress turn.

The image's tmux config is expected to set `status off` so the screen looks
like plain Claude Code. This is part of the image contract, not checked.

### D8. Terminal hand-off

`App` gets `pending_attach: Option<SandboxId>`, set by `/attach`. After each
`select!` iteration, `run_loop` checks it and, if set:

1. Drops its `EventStream` so crossterm stops reading stdin.
2. Leaves the alternate screen, disables mouse capture, and shows the cursor.
   Raw mode stays on, which is what the exec needs.
3. Runs `attach::run(docker, session, size)` to completion (below).
4. Re-enters the alternate screen, enables mouse capture, calls
   `terminal.clear()`, creates a new `EventStream`, and marks the app dirty.

Not calling `ratatui::restore`/`try_init` again keeps the original panic
hook installed once.

`attach::run`:
- Sets `O_NONBLOCK` on fd 0 and wraps it in `tokio::io::unix::AsyncFd`, so
  reads are cancellable. `tokio::io::stdin()` uses a blocking thread whose
  pending read would swallow the first keystroke after detach. A drop guard
  restores the original fd flags. A panic hook added at attach start (chained
  in front of ratatui's) also restores them, because a non-blocking stdin left
  behind breaks the user's shell.
- Loop `select!`:
  - stdin bytes: scan for `0x1c` (`Ctrl-\`). Bytes before it are forwarded. On
    `0x1c`, spawn a one-shot exec of `tmux detach-client -s claude` and keep
    pumping until the attach stream ends.
  - exec output: written to stdout and flushed.
  - `SIGWINCH` (`tokio::signal::unix`): `resize_exec` with the new size. An
    initial resize happens right after start.
- When the output stream ends, `inspect_exec` gives the exit code, and a
  one-shot `tmux has-session -t claude` decides the outcome:
  `Detached` if the session is alive, `Ended { exit_code }` if not. The result
  goes into the tab's transcript.

tmux's own detach (`Ctrl-b d`) produces the same outcome through the same
path.

Tokio's `net` and `signal` features are added for `AsyncFd` and signals.
`libc` (already in `Cargo.lock`) provides `fcntl`. This needs `unsafe`. The
workspace does not forbid it, but it is confined to one small `#[cfg(unix)]`
function with a `// SAFETY:` comment.

### D9. Headless turns

1. Check `tmux has-session -t claude`. If it is alive, refuse (spec).
2. Run an exec with `tty false`, stdout and stderr attached, stdin not
   attached:
   `claude -p <prompt> --output-format json` plus `--resume <id>` or
   `--session-id <id>`, using the same rule as D7.
3. Demultiplexed stdout and stderr are collected separately and capped at
   1 MiB each. `inspect_exec` gives the exit code.

Parsing (`claude::parse_headless`, pure) takes stdout as a JSON object and
reads `result: String`, `session_id: String`, and `is_error: bool`. A
non-zero exit, `is_error: true`, or a parse failure becomes
`SandboxError::Headless { exit_code, stderr, detail }`. Stderr mentioning
login or authentication maps to the "attach and log in first" message. The
`session_id` from the output overwrites the stored id.

The exact JSON field set is confirmed against `claude -p --output-format
json` in the image during implementation, and the test fixtures use captured
output.

### D10. Persistence

`store::SandboxStore` reads and writes `sandboxes.json` (a versioned
`{ "version": 1, "sessions": [...] }`) in a directory passed in by
`mate-cli` (`state_dir()`). The crate never computes paths itself. Writes go
to a temp file in the same directory, then fsync, then rename.
`SandboxSession` has no field that could hold a credential. Status is
persisted but always recomputed by reconcile on startup.

### D11. Reconciliation

`reconcile(store, docker) -> Vec<(SandboxSession, Result<SandboxStatus,
SandboxError>)>` inspects each stored container. A 404, or labels that don't
match, gives `Missing`. Otherwise Docker's state maps to Running, Exited, or
Created. `DaemonUnavailable` is kept per session, so the tab shows the error
instead of a guessed status. Reconcile runs in a spawned task.
`mate-cli` opens a sandbox tab per stored session immediately with status
"checking", and the result arrives as `SandboxEvent::Reconciled`.

### D12. TUI shape

```
enum Tab { Agent(SessionTab), Sandbox(SandboxTab) }
```

`SandboxTab` has the session snapshot, transcript, wrap cache, input box,
scroll, unread and needs-attention flags, a pending confirm
(`Remove`/`RemoveVolume`), and an optional `ReportModal`.

- Call sites that need agent state go through `Tab::as_agent(_mut)` and
  become no-ops on sandbox tabs: panel, approvals, `Ctrl+B`/`Ctrl+P`.
- `on_session_event` routes only to `Tab::Agent`.
- `run_loop` gains an arm for `mpsc::Receiver<SandboxEvent>`: `Reconciled`,
  `PullProgress`, `Created`, `StatusChanged`, `HeadlessDone`,
  `AttachFinished`, `Report`, `Removed`, and `Error`, each tagged with the
  sandbox id. Every Docker action is `tokio::spawn`ed with a cloned sender.
- New slash commands, valid only on a sandbox tab: `/attach`, `/inspect`,
  `/stop`, `/start`, `/rm`. On an agent tab they show "only valid on a sandbox
  tab". Plain text on a sandbox tab is a headless prompt.
- The `Ctrl+T` spawn form gains a kind field (Agent | Sandbox). The sandbox
  variant asks for workspace, image (prefilled), and network
  (none/bridge/custom name, with a warning on none), then confirms.
- `Ctrl+W` on a sandbox tab removes the tab only.
- The tab bar labels sandbox tabs `sbx:<name>` with a status glyph.

No new global keybindings are added, so none collide.

### D13. Report

`report::build(inspect, stats, changes, top) -> SessionReport`, pure.
`SessionReport { configured: Configured{...}, observed: Observed{...} }`.
CPU % uses the standard delta formula from one stats sample, which includes
`precpu_stats`. Env becomes names only by splitting at the first `=` and
dropping the value inside the builder, so the value never reaches the view
model. The view renders two titled blocks, "Configured (docker inspect)" and
"Observed (stats / changes / top)", plus the container-layer caveat line.

## Risks / Trade-offs

- [bollard API drift from memory] → Read the fetched source for every call.
  The user runs `cargo fetch`, and signatures are checked in
  `~/.cargo/registry/src/` before writing the wrapper.
- [crossterm's `EventStream` waker thread may already have buffered input
  bytes when it is dropped] → Check `crossterm` 0.29's event-reader source
  during implementation. At worst, one keystroke at the moment of attach is
  lost. This is acceptable and documented.
- [Non-blocking stdin left set after a crash] → Drop guard plus chained panic
  hook (D8). A `SIGKILL` cannot be handled; documented in `sandbox.md`.
- [The image does not meet the contract (no tmux, no user-owned
  `~/.claude`)] → A create-time probe reports exactly which part is missing.
  The container is left in place so the user can inspect it.
- [The transcript-existence probe depends on Claude Code's on-disk layout] →
  Used only to choose `--resume` or `--session-id`. If the layout changes,
  the worst case is "No conversation found" on resume, shown as an error.
- [The OAuth token in the volume is readable by anyone with Docker access] →
  This is inherent to Docker. Documented. The remove-volume confirm says it
  deletes the login.
- [Ownership check adds an inspect round-trip per action] → Negligible next
  to container operations.
- [Two `mate` instances acting on the same sandbox] → Not prevented. The tmux
  `-A` attach is shared, which tmux handles. The headless refusal still
  applies.

## Migration Plan

New feature with no existing data. Rollback means removing the crate and the
TUI wiring. Leftover containers and volumes are found by the
`managed-by=mate` label and can be removed by hand.

## Open Questions

- The exact bollard version is the latest stable release when the dependency
  is added. This does not change the design.
- Whether the image's Claude Code supports `--session-id` is checked with
  `claude --help` in the image. If it is missing, the fallback is reading
  `session_id` from the first headless or interactive run, which only affects
  D7 and D9 internals.
