# Spec Delta

## Purpose

Lets a `mate` user run Claude Code inside a hardened, per-workspace Docker container, driven from the `mate` TUI, with conversations that survive detaches, container restarts, and `mate` relaunches.

## ADDED Requirements

### Requirement: Creating a sandbox session
The system SHALL let the user create a sandbox session from the TUI by choosing a host workspace directory, an image (defaulting to the configured image, itself defaulting to `ghcr.io/leoborai/dkc:latest`), and a network mode, then confirming. On confirm it SHALL pull the image if absent, create the container and its state volume, start the container, and open a sandbox tab for it.

#### Scenario: Create with defaults
- **WHEN** the user picks an existing workspace directory, keeps the default image and network mode, and confirms
- **THEN** a container named `dkc-<short-session-id>` is created and started, and a new sandbox tab shows the session as Running

#### Scenario: Image not present locally
- **WHEN** the chosen image is not present on the Docker host
- **THEN** the system pulls it and shows pull progress in the TUI before creating the container

#### Scenario: Workspace directory does not exist
- **WHEN** the user enters a workspace path that is not an existing directory
- **THEN** the system refuses to create the session, shows the reason, and creates no container or volume

### Requirement: Container identity and ownership labels
Every container the system creates SHALL carry the labels `managed-by=mate` and `session-id=<session id>`. The system SHALL NOT stop, remove, exec into, or otherwise act on any container lacking both labels with the expected values.

#### Scenario: Unlabelled container with a matching name
- **WHEN** a container exists whose name matches a stored session's container name but which lacks the `managed-by=mate` label
- **THEN** the system does not stop, remove, or exec into it, and reports the session as Missing

#### Scenario: Listing managed containers
- **WHEN** the system discovers sandbox containers
- **THEN** it considers only containers carrying `managed-by=mate`

### Requirement: Workspace and state mounts
A sandbox container SHALL mount the session's host workspace read-write at `/workspace` and use `/workspace` as its working directory. It SHALL mount a named volume `dkc-claude-<session id>`, unique to that session, as Claude Code's configuration directory, so login state and transcripts survive container restarts and recreation. Any extra mounts SHALL each declare read-only or read-write explicitly.

#### Scenario: State survives a container restart
- **WHEN** a sandbox container is stopped and started again
- **THEN** Claude Code inside it is still logged in and can resume earlier conversations

#### Scenario: Sessions do not share state
- **WHEN** two sandbox sessions exist
- **THEN** each uses its own state volume and neither can read the other's login or transcripts

### Requirement: Container hardening
Every sandbox container SHALL drop all Linux capabilities, set `no-new-privileges`, run unprivileged, run as the image's non-root user when the image defines one, and apply memory, PID, and CPU limits (configurable, with defaults). It SHALL NOT mount the Docker daemon socket.

#### Scenario: Hardening visible in inspection
- **WHEN** the user inspects a newly created sandbox
- **THEN** the configured section shows all capabilities dropped, `no-new-privileges`, privileged false, and the memory, PID, and CPU limits

#### Scenario: Extra mount of the Docker socket
- **WHEN** a sandbox's extra mounts would include the Docker daemon socket path
- **THEN** the system refuses to create the container and shows the reason

### Requirement: Network mode fixed at creation
The network mode (none, bridge, or a named network) SHALL be chosen when the session is created, default to bridge, and SHALL NOT change for the life of the container.

#### Scenario: Creating with no network
- **WHEN** the user selects network mode none during creation
- **THEN** the TUI warns that Claude Code cannot reach its API in that mode before the user confirms

### Requirement: No secrets in the sandbox path
The system SHALL NOT pass any API key or credential to the container through environment variables or labels, and SHALL NOT write credentials to the session store, logs, or inspection view. Authentication SHALL happen through Claude Code's own interactive login inside the container.

#### Scenario: First attach to a new session
- **WHEN** the user attaches to a session that has never logged in
- **THEN** Claude Code's own login flow runs inside the attached terminal, and the resulting credentials persist only in that session's state volume

#### Scenario: Environment shown in inspection
- **WHEN** the inspection view lists the container's environment
- **THEN** it shows variable names only, never values

### Requirement: Persisted sandbox sessions
The system SHALL persist each sandbox session (id, name, container id, image, workspace, policy, Claude session id, status, creation time) across `mate` restarts. A write that is interrupted SHALL NOT corrupt previously stored sessions.

#### Scenario: Relaunch keeps sessions
- **WHEN** the user quits `mate` and launches it again
- **THEN** every previously created sandbox session is restored with its workspace, policy, and Claude session id

### Requirement: Startup reconciliation
On startup the system SHALL reconcile every stored session against Docker without blocking the TUI, setting each session's status to Running, Exited, Created, or Missing (container no longer exists or is not ours), and SHALL open one sandbox tab per stored session.

#### Scenario: Container deleted outside mate
- **WHEN** a stored session's container was removed while `mate` was not running
- **THEN** on next launch that session's tab shows status Missing and offers removal

#### Scenario: Docker daemon unreachable
- **WHEN** the Docker daemon cannot be reached at startup or during an action
- **THEN** sandbox tabs show a clear "Docker daemon unreachable" message, the TUI keeps running, and agent tabs are unaffected

### Requirement: Interactive attach
The user SHALL be able to attach to a running sandbox from its tab. Attaching SHALL suspend the TUI, give the container's interactive Claude Code terminal full control of the host terminal (raw input, output, and window-size changes), and run Claude Code inside a persistent terminal multiplexer session in the container.

#### Scenario: Attach to a fresh session
- **WHEN** the user attaches to a session with no Claude session id yet
- **THEN** Claude Code starts with a session id the system generated and stored beforehand

#### Scenario: Attach while Claude Code is still running
- **WHEN** the user attaches and the session's multiplexer session is still alive from an earlier attach
- **THEN** the user rejoins the same running Claude Code terminal, including any turn in progress

#### Scenario: Attach after a container restart
- **WHEN** the user attaches and no multiplexer session exists but a Claude session id is stored
- **THEN** Claude Code starts resuming that stored conversation

#### Scenario: Terminal resize while attached
- **WHEN** the host terminal is resized while attached
- **THEN** Claude Code's terminal inside the container takes the new size

### Requirement: Detach
While attached, a reserved key sequence (`Ctrl-\`) SHALL detach: Claude Code and the container keep running, the host terminal is restored, and the TUI resumes on that sandbox's tab.

#### Scenario: Detach mid-turn
- **WHEN** the user presses `Ctrl-\` while Claude Code is working on a turn
- **THEN** the TUI returns, the container stays Running, and re-attaching later shows the turn's progress

#### Scenario: Claude Code exits while attached
- **WHEN** Claude Code exits inside the attached terminal
- **THEN** the TUI returns and the sandbox tab reports that the session ended, with its exit code

#### Scenario: Panic while attached
- **WHEN** `mate` panics while a sandbox is attached
- **THEN** the host terminal is restored to a usable state

### Requirement: Headless turns
Plain text submitted in a sandbox tab SHALL run as a one-shot, non-interactive Claude Code turn in the container (continuing the stored Claude session when one exists). The system SHALL store the returned Claude session id and show the result text in that tab's transcript.

#### Scenario: Successful headless turn
- **WHEN** the user submits a prompt in a sandbox tab with no live interactive Claude Code session
- **THEN** the result text appears in the tab and the session's Claude session id is updated

#### Scenario: Interactive session still alive
- **WHEN** the user submits a prompt while the sandbox's interactive Claude Code session is still alive (attached or detached)
- **THEN** the system refuses the turn and tells the user to exit Claude Code in the attached terminal first

#### Scenario: Claude Code fails or returns unparseable output
- **WHEN** the turn exits non-zero or its output is not the expected JSON
- **THEN** the tab shows an error including the exit code and Claude Code's stderr

#### Scenario: Not logged in yet
- **WHEN** a headless turn fails because the session has not completed Claude Code's login
- **THEN** the tab tells the user to attach and log in first

### Requirement: Inspection report
The user SHALL be able to open an inspection view for a sandbox that separates configured values (state, network mode, attached networks and IPs, published ports, mounts with read-write flags, limits and hardening, environment variable names) from observed values (one-shot CPU, memory, network, and block I/O usage; container-layer filesystem changes; running processes), each section labelled as configured or observed.

#### Scenario: Inspecting a running sandbox
- **WHEN** the user opens inspection on a Running sandbox
- **THEN** the view shows mounts, network mode, and limits under "configured" and live resource usage under "observed"

#### Scenario: Filesystem changes caveat
- **WHEN** the view shows filesystem changes
- **THEN** it states that they cover only the container's own layer, not the workspace bind mount or volumes

#### Scenario: No claims about network or file access
- **WHEN** the inspection view is shown
- **THEN** it makes no claim about which hosts were contacted or which files were read

### Requirement: Stop and remove
Stopping and removing a sandbox SHALL be explicit user actions. Removing SHALL require confirmation and removes the container. Removing the session's state volume SHALL be a separate confirmed choice that warns it deletes the login and transcripts.

#### Scenario: Remove and keep volume
- **WHEN** the user confirms removal and declines volume removal
- **THEN** the container is removed, the volume remains, and the session is deleted from the store

#### Scenario: Remove including volume
- **WHEN** the user confirms removal and confirms volume removal
- **THEN** both the container and `dkc-claude-<session id>` are removed

#### Scenario: Quitting mate
- **WHEN** the user quits `mate`
- **THEN** sandbox containers are left running

#### Scenario: Closing a sandbox tab
- **WHEN** the user closes a sandbox tab
- **THEN** the tab hides for this run only, the container is untouched, and the tab reappears on next launch
