# Docker sandbox (`mate-sandbox`)

Runs Claude Code inside a hardened, per-workspace Docker container. All
Docker access sits behind this crate; `bollard` never leaks into `mate-core`.

## Modules

| Module | Role |
|---|---|
| `image` | the embedded Dockerfile and `tmux.conf`, the `mate-sandbox:<hash12>` tag, the in-memory tar build context |
| `docker` | `DockerApi` trait, `BollardDocker`, error mapping, ownership guard, `exec_collect`, `ensure_image` (build or pull) |
| `policy` | `SandboxPolicy`, `validate_policy`, pure `policy_to_host_config` |
| `spec` | `image_identity`, pure `build_container_spec` |
| `session` / `store` | `SandboxSession`, `sandboxes.json` persistence (temp file, fsync, rename) |
| `reconcile` | startup status check against Docker |
| `claude` | command builders and `parse_headless` |
| `headless` | `run_headless` (one-shot turns) |
| `attach` | interactive terminal hand-off |
| `report` | configured vs observed inspection report |

Unit tests use the in-memory `FakeDocker` (`src/fake.rs`, test-only). The one
live test, `tests/docker.rs`, is `#[ignore]`d and also needs
`DKC_DOCKER_TESTS=1`.

## Ownership

Every container carries `managed-by=mate` and `session-id=<uuid>`. Stop,
remove, and exec go through `verify_ownership` first and refuse with
`SandboxError::NotOurs` otherwise, so an unlabelled container that happens to
share a name is never touched. Reconcile reports such a container as
`Missing`.

## Secrets

No API key or credential is passed through env or labels. The container env
holds only `CLAUDE_CONFIG_DIR` (a path). Login happens interactively inside
the container and persists in the session's volume `dkc-claude-<uuid>`.
`SandboxSession` has no field that could hold a credential, and the report
shows env variable names only.

Caveat: the OAuth token in that volume is readable by anyone with access to
the Docker daemon. This is inherent to Docker. Removing the volume deletes the
login and the transcripts.

## Attach mechanics

Interactive Claude Code runs in a tmux session named `claude`:

```
tmux new-session -A -s claude -- claude (--session-id <uuid> | --resume <uuid>)
```

`-A` rejoins an existing session and ignores the command. `prepare_attach`
chooses the argument and records the id on the session; the caller must
persist the store before calling `attach::run`.

`attach::run` sets `O_NONBLOCK` on fd 0 and reads it through `AsyncFd`, so a
pending read is cancellable and cannot swallow the first keystroke after
detach. `NonBlockingFd` restores the flags on drop, and a panic hook chained
once per process restores them too. `SIGKILL` cannot be handled and may leave
stdin non-blocking; running `reset` or opening a new shell recovers.

`Ctrl-\` (`0x1c`) triggers `tmux detach-client -s claude`; tmux's own
`Ctrl-b d` ends the attach the same way. When the output stream ends,
`has-session` decides `Detached` vs `Ended { exit_code }`.

Containers keep running when `mate` exits; stop and remove are explicit.

## Image contract

Any sandbox image must provide `tmux` (with `status off` for a clean screen),
a non-root user, and a `$HOME/.claude` directory owned by that user. Docker
copies a directory's ownership into a new named volume only if the directory
exists in the image.

## Embedded image

`crates/mate-sandbox/image/Dockerfile` and `tmux.conf` are embedded in the
binary with `include_str!` (so they ship with `cargo package`). The image is
Debian slim with `tmux`, `git`, `curl`, Node, Claude Code installed with
`npm`, and the non-root user `claude` owning `/home/claude/.claude`.

- **Tag.** `mate-sandbox:<hash12>`: the first 12 hex characters of a SHA-256
  over both files (each length-prefixed). Changing either file gives a new
  tag, so an unchanged image is reused and a changed one is rebuilt.
- **Build.** `ensure_image` inspects the image; when the reference is the
  current embedded tag and it is absent, it sends an in-memory tar of the two
  files to the daemon's build API and streams the output lines to the tab. A
  failing step becomes `SandboxError::Docker { op: "build_image", .. }` with
  the step's message, and nothing is created. Any other reference is pulled.
- **Override.** `[sandbox] image` (see `config.md`) replaces the embedded
  image and is never built; it must meet the contract above. Create probes
  `test -w "$CLAUDE_CONFIG_DIR"` and reports a missing part.
- **Staleness.** The tag reflects only the embedded files, so Claude Code's
  version floats until they change. Build your own image and set `image` to
  pin a version.

## crossterm `EventStream` and buffered input

Finding, from `crossterm-0.29.0/src/event/stream.rs` and
`src/event/read.rs`:

- `EventStream::drop` only sets `stream_wake_task_should_shutdown` and wakes
  the poll. It does not close or drain anything.
- The reader is a process-global (`static INTERNAL_EVENT_READER` in
  `src/event.rs`) that keeps its tty descriptor (stdin if it is a tty, else
  `/dev/tty`) and its `events` queue for the life of the process.
- A helper thread polls that reader while the stream is pending. Bytes it has
  already read and parsed before the drop stay queued in the global reader and
  are delivered to the next `EventStream`, so at most those bytes replay after
  re-entering the TUI. Nothing reads the tty between the drop and the new
  stream, so keystrokes typed during attach go to `attach::run`.
- Race: the helper thread may be mid-`poll` when the stream is dropped and can
  consume one read before it sees the shutdown flag. At worst one keystroke
  typed at the instant of attach is lost or replayed.
