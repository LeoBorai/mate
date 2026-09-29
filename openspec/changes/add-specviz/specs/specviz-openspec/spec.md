# Spec Delta

## Purpose

Makes the viewer understand OpenSpec's layout and conventions, so capability specs and in-flight changes are navigable by meaning (capability, change, progress) and delta specs read as reviewable diffs rather than plain Markdown.

## ADDED Requirements

### Requirement: OpenSpec sections
When the OpenSpec source is present, the spec tree SHALL present it as three sections: **Capabilities** (one entry per `openspec/specs/<capability-path>/spec.md`), **Changes** (one entry per directory directly under `openspec/changes/` other than `archive`), and **Archive** (one entry per directory under `openspec/changes/archive/`). A section with no entries SHALL still be shown, marked empty, when the OpenSpec source is present.

#### Scenario: Active and archived changes
- **WHEN** `openspec/changes/` contains `add-x/`, `add-y/`, and `archive/2026-01-01-add-z/`
- **THEN** the Changes section SHALL list `add-x` and `add-y`, and the Archive section SHALL list `2026-01-01-add-z`

#### Scenario: Nested capability path
- **WHEN** a capability spec exists at `openspec/specs/identity/user-auth/spec.md`
- **THEN** the Capabilities section SHALL list it under the capability path `identity/user-auth`

#### Scenario: No capability specs yet
- **WHEN** `openspec/` exists but `openspec/specs/` contains no `spec.md`
- **THEN** the Capabilities section SHALL be shown and marked empty

### Requirement: Change artifact ordering
Each change entry SHALL list every Markdown file inside that change directory, recursively. Files named `proposal.md`, `design.md`, and `tasks.md` at the change's top level, and delta specs under its `specs/` directory, SHALL be ordered as proposal, delta specs, design, tasks; every other Markdown file SHALL follow, sorted by path.

#### Scenario: Standard change
- **WHEN** a change contains `tasks.md`, `design.md`, `proposal.md`, and `specs/foo/spec.md`
- **THEN** its entries SHALL appear in the order `proposal.md`, `specs/foo/spec.md`, `design.md`, `tasks.md`

#### Scenario: Extra artifact from a custom schema
- **WHEN** a change also contains `research.md`
- **THEN** `research.md` SHALL be listed after the standard artifacts

### Requirement: Change task progress
Each change entry SHALL carry a task progress count derived from its top-level `tasks.md`: the number of Markdown task-list items checked (`- [x]`, case-insensitive) and the total number of task-list items. A change with no `tasks.md`, or whose `tasks.md` has no task-list items, SHALL show no progress count. The count SHALL update on live reload when `tasks.md` changes.

#### Scenario: Partially complete change
- **WHEN** a change's `tasks.md` has 21 task-list items of which 13 are checked
- **THEN** the change entry SHALL show progress `13/21`

#### Scenario: No tasks file
- **WHEN** a change has no `tasks.md`
- **THEN** the change entry SHALL show no progress count

#### Scenario: Checking off a task
- **WHEN** a user changes a `- [ ]` item to `- [x]` in an open change's `tasks.md` and saves
- **THEN** the change's progress count SHALL increase by one without a manual reload

### Requirement: Capability requirement count
Each capability entry SHALL carry the number of `### Requirement:` headings in its `spec.md`.

#### Scenario: Capability with requirements
- **WHEN** a capability's `spec.md` contains four `### Requirement:` headings
- **THEN** the capability entry SHALL show a requirement count of 4

### Requirement: Delta section rendering
When rendering a document from the OpenSpec source, the viewer SHALL render each level-2 heading whose text is exactly `ADDED Requirements`, `MODIFIED Requirements`, `REMOVED Requirements`, or `RENAMED Requirements` (case-insensitive) with a visually distinct label for that operation. The same heading in a document from the plain `specs/` source SHALL render as an ordinary heading.

#### Scenario: Delta spec in a change
- **WHEN** a user opens `openspec/changes/add-x/specs/foo/spec.md` containing `## ADDED Requirements` and `## REMOVED Requirements`
- **THEN** each heading SHALL render with its own operation label, distinguishable from the other

#### Scenario: Same heading in a plain spec
- **WHEN** a user opens `specs/notes.md` containing `## ADDED Requirements`
- **THEN** the heading SHALL render as an ordinary level-2 heading with no operation label

### Requirement: Requirement and scenario rendering
When rendering a document from the OpenSpec source, the viewer SHALL render `### Requirement: <name>` headings with a distinct requirement style, and SHALL render each `#### Scenario: <name>` heading together with its content, up to the next heading of level 4 or higher, as one visually grouped block. Within a scenario block, a list item beginning with `WHEN`, `THEN`, or `AND` (optionally bolded) SHALL have that keyword emphasized.

#### Scenario: Scenario grouping
- **WHEN** a requirement has two scenarios, each with WHEN/THEN bullets
- **THEN** each scenario's heading and bullets SHALL render as its own grouped block, and the second scenario SHALL NOT be inside the first's block

#### Scenario: Keyword emphasis
- **WHEN** a scenario bullet reads `- **WHEN** the user saves`
- **THEN** `WHEN` SHALL render with keyword emphasis

### Requirement: Rendering stays sanitized
OpenSpec-aware rendering SHALL NOT weaken sanitization: styling SHALL be applied only through a fixed set of viewer-defined class names, and any class attribute authored in a document's raw HTML that is outside that set SHALL be stripped.

#### Scenario: Authored class attribute
- **WHEN** a spec document contains raw HTML `<div class="evil">x</div>`
- **THEN** the rendered HTML SHALL NOT contain the class `evil`

#### Scenario: Styling in a release build
- **WHEN** the viewer runs from a release build of `mate`
- **THEN** delta labels, requirement headings, and scenario blocks SHALL be visibly styled, identical to a development build
