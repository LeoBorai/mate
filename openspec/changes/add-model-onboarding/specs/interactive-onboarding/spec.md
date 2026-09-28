# Spec Delta

## Purpose

Lets a user reach mate's TUI and get productive without already having an `API_TOKEN` in place, by walking them through picking a backend, a model, and a token interactively instead of exiting before the UI renders.

## ADDED Requirements

### Requirement: TUI starts without API_TOKEN set
When `API_TOKEN` is unset in the environment, `mate`'s TUI SHALL render its interface rather than exiting before startup completes.

#### Scenario: Launching with no token in the environment
- **WHEN** a user runs `mate` (TUI mode) with `API_TOKEN` unset and no other config source providing it
- **THEN** the TUI renders and presents the onboarding flow, instead of exiting with an error

### Requirement: Onboarding walks backend, then model, then token
The onboarding flow SHALL present exactly three steps in order: choose a backend (`Huggingface` or `Gemini`), choose a model from the catalog filtered to that backend, then enter a token. The user SHALL NOT be asked for a token before a backend and model are chosen.

#### Scenario: Completing the onboarding steps in order
- **WHEN** a user completes backend selection, then model selection, then token entry
- **THEN** each step's choices are available before the next step is shown, and the model list offered is filtered to the chosen backend

### Requirement: Token is verified before sessions start
The token entered during onboarding SHALL be verified against the chosen backend before any session is spawned. An invalid token SHALL surface an error in the onboarding UI and let the user re-enter it, without restarting the process.

#### Scenario: Valid token
- **WHEN** the entered token passes verification against the chosen backend
- **THEN** mate proceeds to spawn sessions using that backend, model, and token

#### Scenario: Invalid token
- **WHEN** the entered token fails verification against the chosen backend
- **THEN** onboarding shows an error and lets the user re-enter a token, without exiting mate or losing the previously chosen backend and model

### Requirement: Onboarding token is never persisted
A token entered through onboarding SHALL exist only in the running process's memory. It SHALL NOT be written to any config file, the model catalog, or any other on-disk location, and SHALL NOT be available to a later `mate` invocation.

#### Scenario: Token does not survive a restart
- **WHEN** a user completes onboarding, exits mate, and launches mate again with `API_TOKEN` still unset
- **THEN** onboarding is shown again, with no memory of the previously entered token

### Requirement: An already-set API_TOKEN skips onboarding
When `API_TOKEN` is already available at startup (from the environment or any existing config source), the TUI SHALL start exactly as it does today, with no onboarding flow shown.

#### Scenario: Launching with a token already set
- **WHEN** a user runs `mate` (TUI mode) with `API_TOKEN` set to a valid value
- **THEN** mate starts sessions directly, with no onboarding modal shown

### Requirement: Plain/print mode is unaffected
`--plain`/`--print` mode SHALL continue to fail immediately when no token is available, with no onboarding flow offered, since it has no interactive surface to onboard through.

#### Scenario: Running --plain with no token
- **WHEN** a user runs `mate --plain` (or `--print`) with `API_TOKEN` unset and no other config source providing it
- **THEN** mate exits immediately with today's "API_TOKEN is not set" error, unchanged
