# Spec Delta

## Purpose

Lets a user start the spec viewer from `mate` — either as a standalone foreground command or from inside a running TUI session without blocking it — and find out where it is serving.

## ADDED Requirements

### Requirement: Foreground subcommand
`mate specviz` SHALL start the viewer in the foreground for a served root, print the viewer's URL to standard output, and keep serving until interrupted (Ctrl+C), then exit with status 0. The served root SHALL default to the current directory and SHALL be overridable with `-C <dir>`. The port SHALL default to 7732 and SHALL be overridable with `--port <port>`.

#### Scenario: Default invocation
- **WHEN** a user runs `mate specviz` in a repository root
- **THEN** `mate` SHALL print `http://127.0.0.1:7732` (as part of its startup line) and serve that repository's specs until interrupted

#### Scenario: Custom root and port
- **WHEN** a user runs `mate specviz -C ../other --port 9000`
- **THEN** `mate` SHALL serve `../other` on `http://127.0.0.1:9000`

#### Scenario: Port already in use
- **WHEN** the requested port is already bound by another process
- **THEN** `mate specviz` SHALL exit with a non-zero status and an error naming the port

#### Scenario: Root does not exist
- **WHEN** `-C` names a directory that does not exist
- **THEN** `mate specviz` SHALL exit with a non-zero status and an error naming the directory

### Requirement: Foreground subcommand needs no agent setup
`mate specviz` SHALL NOT require an API token, SHALL NOT show the first-run notice, SHALL NOT contact any model provider, and SHALL NOT spawn any configured MCP server.

#### Scenario: No API token configured
- **WHEN** a user with no API token configured runs `mate specviz`
- **THEN** the viewer SHALL start normally

#### Scenario: MCP servers configured
- **WHEN** the configuration declares MCP servers and the user runs `mate specviz`
- **THEN** no MCP server process SHALL be spawned

### Requirement: Subcommand takes precedence over a bare prompt
The bare argument `specviz` in the first positional position SHALL select the subcommand. Any other one-shot prompt, including one that begins with the word `specviz` but is passed as a single quoted argument containing more text, SHALL continue to be treated as a prompt.

#### Scenario: Quoted prompt mentioning specviz
- **WHEN** a user runs `mate "specviz is not loading"`
- **THEN** `mate` SHALL treat the argument as a one-shot prompt, not as the subcommand

### Requirement: Background slash command
In the TUI, `/specviz` SHALL start the viewer for the active tab's workspace root on an OS-assigned loopback port, without blocking input, rendering, or any session. It SHALL write one system line to the active tab's transcript containing the served root and the viewer's URL, with the URL on its own, unpunctuated so terminals can detect it as a link. It SHALL NOT open a browser. It SHALL NOT be sent to the model.

#### Scenario: First /specviz in a tab
- **WHEN** a user submits `/specviz` in a tab whose workspace root is `/repo`
- **THEN** the tab's transcript SHALL show a system line naming `/repo` and a `http://127.0.0.1:<port>` URL, and no browser SHALL be launched

#### Scenario: TUI stays responsive during startup
- **WHEN** the viewer is still indexing a large root after `/specviz`
- **THEN** the TUI SHALL keep accepting keystrokes and rendering, and the URL SHALL already be shown

### Requirement: One background viewer per root
The TUI SHALL run at most one background viewer per distinct workspace root. Submitting `/specviz` again in any tab whose root already has a running viewer SHALL re-print that viewer's URL and SHALL NOT start another. Tabs with different roots SHALL each get their own viewer on their own port.

#### Scenario: Repeat /specviz
- **WHEN** a user submits `/specviz` twice in the same tab
- **THEN** both system lines SHALL show the same URL, and only one viewer SHALL be running

#### Scenario: Two tabs, two roots
- **WHEN** a user submits `/specviz` in a tab rooted at `/a` and then in a tab rooted at `/b`
- **THEN** each tab SHALL show a different URL, each serving its own root

### Requirement: Background startup failure is reported
If a background viewer fails to bind or to start, the TUI SHALL write an error system line to the tab that issued `/specviz`, SHALL NOT crash or disturb other tabs, and SHALL allow a later `/specviz` for that root to try again.

#### Scenario: Startup fails after the URL is shown
- **WHEN** the viewer's startup fails after `/specviz` already printed its URL
- **THEN** the issuing tab SHALL show an error system line for that root, and a later `/specviz` for that root SHALL attempt a fresh start

### Requirement: Background viewers stop with mate
Every background viewer SHALL stop when `mate` exits, leaving no process or listening socket behind.

#### Scenario: Quitting mate
- **WHEN** a user runs `/specviz` and then quits `mate`
- **THEN** the viewer's URL SHALL stop accepting connections and no viewer process SHALL remain
