# Tasks

Verification note: this repo forbids running `cargo` from the agent. "Verify:
tests pass" means the user runs `just test` (or the named test) and reports
back. API correctness against dependencies is checked by reading their
source in `~/.cargo/registry/src/`.

## 1. Image contract spike

- [ ] 1.1 In a throwaway `ghcr.io/leoborai/dkc:latest` container, record: `Config.User`, `HOME`, whether `$HOME/.claude` exists and who owns it, whether `tmux` is installed, the `tmux` config (`status off`?), and `claude --help` output for `--session-id`, `--resume`, `-p`, `--output-format`. Capture one real `claude -p --output-format json` stdout (success and not-logged-in) as test fixtures. Verify: findings and fixtures saved under `crates/mate-sandbox/tests/fixtures/`. Any contract gap (no tmux, no user-owned `.claude`) is reported to the user as a required `dkc` image change before continuing past group 6.

## 2. Crate scaffolding and dependencies

- [x] 2.1 Create `crates/mate-sandbox` (`Cargo.toml`, `src/lib.rs` with module stubs `docker`, `policy`, `session`, `store`, `reconcile`, `claude`, `report`, `error`, `attach`). Add it to the root `[workspace.dependencies]` and members. Verify: the files exist and the workspace member list includes the crate.
- [x] 2.2 Add `bollard` (latest stable) and `uuid` (`v4`, `serde`) to `[workspace.dependencies]`, and add tokio features `net` and `signal`. Ask the user to run `cargo fetch`, then read the fetched `bollard` source and record its version plus the exact types and methods used (containers, exec, images, volumes, stats) as a short note at the top of `docker.rs`. Verify: the note names real paths in `~/.cargo/registry/src/` for the bollard version.

## 3. Errors, policy, and host-config translation

- [x] 3.1 Add `SandboxError` (thiserror) with `DaemonUnavailable`, `NotFound`, `NotOurs`, `InvalidPolicy`, `ImageContract`, `Headless { exit_code, stderr, detail }`, `Store`, `Docker { op, source }`. Verify: `error.rs` compiles in the user's `just test` run.
- [x] 3.2 Add `SandboxPolicy`, `NetworkMode`, and `Mount` with the defaults from design D4, plus pure `validate_policy` (non-directory workspace, Docker socket mounts). Verify: unit tests cover a missing workspace, a file-as-workspace, `/var/run/docker.sock`, `/run/docker.sock`, and a `unix://` `DOCKER_HOST` path.
- [x] 3.3 Add pure `policy_to_host_config`. Verify: unit tests assert `cap_drop ["ALL"]`, `security_opt ["no-new-privileges:true"]`, `privileged false`, the memory, PID, and nano-CPU values, each network mode, the workspace bind `:rw`, extra mounts with explicit `:ro`/`:rw`, the state volume mount, `read_only_rootfs`, and no Docker socket in any bind.

## 4. Docker wrapper

- [x] 4.1 Define the `DockerApi` trait (D2), and add `BollardDocker` built with `Docker::connect_with_socket_defaults()`, implementing every method using only the API recorded in 2.2. Map errors as in D2. Verify: unit tests for the error-mapping function (connection error to `DaemonUnavailable`, 404 to `NotFound`).
- [x] 4.2 Add the label ownership guard (D3) used by stop, remove, and exec. Verify: a unit test with the fake refuses a container with a missing or mismatched label and returns `NotOurs`.
- [x] 4.3 Add `ensure_image` with a progress callback or stream, and `list_managed_containers` filtered by `managed-by=mate`. Verify: covered by the integration test in 4.5.
- [x] 4.4 Add a container-spec builder (D5, D6): name, labels, `sleep infinity`, tty, open stdin, `working_dir`, user, env `CLAUDE_CONFIG_DIR` only, and the state volume. Verify: unit tests assert the exact labels, env keys (no credential-like keys), name format `dkc-<8 hex>`, and volume name `dkc-claude-<uuid>`.
- [ ] 4.5 Add integration test `tests/docker.rs`, marked `#[ignore]`. It skips with a message unless `DKC_DOCKER_TESTS=1` and the daemon is reachable. It covers create, start, exec `echo hi` (output and exit code 0), inspect (labels, hardening), stop, and remove, using a small public image. Verify: the user runs `DKC_DOCKER_TESTS=1 cargo test -p mate-sandbox -- --ignored` and it passes, and running without the env var reports a skip, not a failure.

## 5. Session model, store, and reconciliation

- [x] 5.1 Add `SandboxSession` and `SandboxStatus` (D6, D10) with serde. Verify: a serde round-trip unit test, and a test that the serialized JSON keys are exactly the expected set.
- [x] 5.2 Add `SandboxStore` (versioned JSON, temp-file then fsync then rename, directory injected). Verify: unit tests with `tempfile` cover load-missing-file as empty, save then load, and a leftover temp file not corrupting the real file.
- [ ] 5.3 Add `reconcile` (D11) against `DockerApi`. Verify: unit tests with the fake cover running, exited, created, 404 to Missing, label mismatch to Missing, and daemon unavailable kept per session.

## 6. Claude command building and headless parsing

- [x] 6.1 Add pure builders for the interactive tmux command (D7) and the headless command (D9), choosing `--session-id` or `--resume` from inputs (stored id, transcript exists). Verify: unit tests for all three D7 cases and both headless cases.
- [ ] 6.2 Add pure `parse_headless(stdout, stderr, exit_code)` (D9). Verify: unit tests using the fixtures from 1.1 cover success, `is_error: true`, non-zero exit, malformed JSON (error carries stderr), and the not-logged-in mapping.
- [ ] 6.3 Add `run_headless` (tmux liveness refusal, exec with `tty false`, demultiplexed capture with 1 MiB caps, `inspect_exec`, parse, persist the new `session_id`). Verify: a unit test with the fake refuses when `has-session` reports alive. The happy path is covered by the integration test in 9.1.

## 7. Inspection report

- [ ] 7.1 Add `SessionReport` and pure `report::build` (D13), including the CPU % delta formula and env names only. Verify: unit tests with canned inspect, stats, changes, and top values assert the CPU %, memory, net and block totals, mount RW flags, and that no env value string appears anywhere in the built report's `Debug` output.
- [ ] 7.2 Add `inspect_session(id)`, which fetches the four calls (stats, changes, and top only when Running) and builds the report. Verify: a unit test with the fake for a stopped container shows the configured section and an "observed: not running" state.

## 8. Interactive attach

- [ ] 8.1 Add a `#[cfg(unix)]` stdin raw-fd guard (`O_NONBLOCK` set/restore with `// SAFETY:`), plus a chained panic hook that restores the flags. Verify: a unit test sets and drops the guard on a pipe fd and asserts the flags are restored.
- [ ] 8.2 Add `attach::run` (D8): AsyncFd stdin pump, `0x1c` detection triggering `tmux detach-client`, output to stdout, `SIGWINCH` to `resize_exec` plus an initial resize, and `Detached`/`Ended{exit_code}` decided via `inspect_exec` and `has-session`. Make the `0x1c` splitter a pure function. Verify: unit tests for the splitter (byte at start, middle, end, and absent).
- [ ] 8.3 Read `crossterm` 0.29's `EventStream` and internal reader source to confirm what happens to buffered input when the stream drops. Document the finding in `.agents/docs/sandbox.md`. Verify: the doc section cites the source file.

## 9. TUI integration

- [ ] 9.1 Refactor `App.tabs` to `Vec<Tab>` with `Tab::{Agent, Sandbox}` and `as_agent(_mut)` accessors. Agent-only paths become no-ops on sandbox tabs. Verify: all existing `mate-tui` tests pass unchanged except for mechanical constructor updates.
- [ ] 9.2 Add `SandboxTab`, `SandboxEvent`, and the second `select!` arm in `run_loop`. Every Docker action is spawned with a cloned sender. Verify: unit tests feed `SandboxEvent`s into `App` and assert transcript and status changes, and that an event for an unknown id is ignored.
- [ ] 9.3 Add slash commands `/attach`, `/inspect`, `/stop`, `/start`, and `/rm` to `slash::parse`, plus plain-text-as-headless on sandbox tabs. Verify: `slash.rs` parse tests, and `App` tests for "only valid on a sandbox tab" on agent tabs and the headless refusal message.
- [ ] 9.4 Add `/rm` two-step confirm (container, then a separate volume question warning about login and transcripts). Verify: `App` tests that declining at each step issues no removal and confirming issues the right calls on the fake.
- [ ] 9.5 Add the spawn form kind toggle and sandbox fields (workspace, image prefilled, network with a warning on none), plus pull-progress rendering. Verify: `App` tests for form validation (bad directory refused, no container call) and that the none-network warning appears.
- [ ] 9.6 Add the terminal hand-off in `run_loop` (D8: drop `EventStream`, leave alt screen, run attach, re-enter, new `EventStream`, redraw). Verify: manual run by the user (attach, `Ctrl-\`, TUI redraws correctly, keystrokes work immediately).
- [ ] 9.7 Add the report modal with "Configured" and "Observed" blocks and the container-layer caveat. Verify: render tests on plain strings (the `ui.rs` convention) assert both titles and the caveat text.
- [ ] 9.8 Add the tab bar `sbx:<name>` label with status glyph, and make `Ctrl+W` hide-only on sandbox tabs. Verify: `tab_bar_segments` unit test, and an `App` test that `Ctrl+W` issues no Docker call.

## 10. CLI wiring and config

- [ ] 10.1 Add a `[sandbox]` config section (`image`, `memory`, `pids`, `cpus`) to `mate-cli`'s layered config. Verify: config load tests for defaults and file overrides, matching the existing `mcp` config tests.
- [ ] 10.2 In `mate-cli/src/tui.rs`: build `BollardDocker` (a failure is not fatal), load `SandboxStore` from `state_dir()`, open the sandbox tabs, spawn reconcile, and pass the sandbox channel into `mate_tui::run` (and the onboarding path). Verify: the user runs `mate` with Docker stopped, and the TUI starts with sandbox tabs showing "Docker daemon unreachable".
- [ ] 10.3 Docs: add `.agents/docs/sandbox.md` (architecture, image contract, attach mechanics, secret handling, the OAuth-token-in-volume caveat, the containers-left-running-on-exit behavior), and update `architecture.md` (crate table and graph) and `config.md` (`[sandbox]`). Verify: the docs mention no out-of-band planning file, and the crate table lists `mate-sandbox`.

## 11. End-to-end acceptance

- [ ] 11.1 The user runs `just test` and `cargo clippy -- -D warnings`. Verify: both pass.
- [ ] 11.2 The user runs the manual flow: create a sandbox, attach, log in, chat, `Ctrl-\`, quit `mate`, relaunch, re-attach (same conversation, via tmux or via `--resume` after `/stop` then `/start`), `/inspect` (mounts, network, limits, live stats), and `/rm` with the volume declined, then removed. Verify: every step behaves as the spec scenarios describe.
- [ ] 11.3 Secret audit: grep `sandboxes.json`, the log file, `docker inspect` labels and env, and the report modal for any token or credential. Verify: none found.
