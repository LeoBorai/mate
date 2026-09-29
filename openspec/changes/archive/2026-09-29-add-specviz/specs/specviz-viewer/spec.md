# Spec Delta

## Purpose

Serves a local, read-only, live-reloading web view of the Markdown specs in a workspace, so a user can browse specs and OpenSpec changes in a browser instead of raw files.

## ADDED Requirements

### Requirement: Loopback-only HTTP serving
The viewer SHALL listen only on the loopback interface (`127.0.0.1`) and SHALL serve both its web UI and its JSON API from that single listener. The web UI SHALL be served from assets embedded in the `mate` binary, with no asset directory required on disk at runtime.

#### Scenario: UI served from the binary
- **WHEN** the viewer is running and a browser requests `/` on its URL
- **THEN** the viewer SHALL respond with the embedded web UI, even when no UI build output exists anywhere on disk

#### Scenario: Unknown non-API path
- **WHEN** a browser requests a path that is neither an API route nor an embedded asset
- **THEN** the viewer SHALL respond with the web UI's entry page, so client-side routes survive a page reload

### Requirement: Source discovery
The viewer SHALL treat a served root directory as containing up to two spec sources: a plain source at `<root>/specs/` and an OpenSpec source at `<root>/openspec/`. Each source SHALL be included when its directory exists. When neither exists, the viewer SHALL still start and present an empty spec tree.

#### Scenario: Only plain specs present
- **WHEN** the root contains `specs/` and no `openspec/`
- **THEN** the spec tree SHALL list the Markdown files under `specs/` and no OpenSpec sections

#### Scenario: Both sources present
- **WHEN** the root contains both `specs/` and `openspec/`
- **THEN** the spec tree SHALL present both sources as separate top-level sections

#### Scenario: No sources present
- **WHEN** the root contains neither `specs/` nor `openspec/`
- **THEN** the viewer SHALL start successfully and SHALL return an empty spec tree

### Requirement: Spec identifiers are root-relative
Every document the viewer exposes SHALL be identified by its slash-separated path relative to the served root (for example `specs/auth/login.md` or `openspec/changes/add-x/tasks.md`), so documents from different sources never collide.

#### Scenario: Same file name in both sources
- **WHEN** `specs/overview.md` and `openspec/specs/overview/spec.md` both exist
- **THEN** each SHALL be listed and fetchable under its own distinct identifier

### Requirement: Sandboxed file access
The viewer SHALL read only Markdown files that resolve, after symlink resolution, to a location inside one of the discovered source directories. Any request naming a path outside them SHALL be answered as not found, and SHALL NOT disclose whether the target exists.

#### Scenario: Parent traversal
- **WHEN** a client requests a spec identifier containing `..` that resolves outside every source directory
- **THEN** the viewer SHALL respond with HTTP 404 and SHALL NOT read the target file

#### Scenario: Symlink escape
- **WHEN** a file inside `specs/` is a symlink pointing outside every source directory
- **THEN** the viewer SHALL NOT serve that file's content

### Requirement: Spec tree and rendered spec API
The viewer SHALL expose a JSON endpoint returning the full spec tree, and a JSON endpoint returning one document rendered to sanitized HTML together with its title and identifier. A document's title SHALL be its first level-1 heading, or its file stem when it has none. Rendered HTML SHALL NOT contain script elements or event-handler attributes, regardless of the document's content.

#### Scenario: Fetching a rendered document
- **WHEN** a client requests an existing document by identifier
- **THEN** the viewer SHALL respond with its title, identifier, and rendered HTML

#### Scenario: Missing document
- **WHEN** a client requests an identifier that names no existing document
- **THEN** the viewer SHALL respond with HTTP 404

#### Scenario: Embedded script in a document
- **WHEN** a document contains a raw `<script>` element
- **THEN** the rendered HTML SHALL NOT contain that script element

### Requirement: Live reload
The viewer SHALL watch every discovered source directory and notify connected browsers over a server-sent event stream when documents change. A content edit to one existing document SHALL notify for that document; a creation, deletion, or rename SHALL notify that the whole tree changed.

#### Scenario: Editing an open document
- **WHEN** a user saves a change to a document currently shown in the browser
- **THEN** the browser SHALL show the updated content without a manual reload

#### Scenario: Adding a new change directory
- **WHEN** a new directory with Markdown files is created under `openspec/changes/`
- **THEN** the browser's spec tree SHALL update to include it without a manual reload

### Requirement: No terminal output while embedded
The viewer SHALL NOT write to the process's standard output or standard error when started from inside the `mate` TUI; its diagnostics SHALL go through `mate`'s logging instead.

#### Scenario: Indexing error while the TUI is running
- **WHEN** the viewer, started from the TUI, fails to read a document during indexing
- **THEN** the TUI screen SHALL remain intact and the failure SHALL appear in `mate`'s log
