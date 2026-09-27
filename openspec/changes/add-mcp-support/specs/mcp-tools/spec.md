# Spec Delta

## Purpose

Lets a mate agent call external tools exposed by configured MCP servers, so users can extend agent capability without mate itself shipping native code per integration.

## ADDED Requirements

### Requirement: Configuring MCP servers
The system SHALL let a user configure zero or more named MCP servers through mate's layered configuration. Each server entry SHALL specify: a name unique among configured servers, a spawn command and arguments, optional environment variables, a transport, and an explicit allow-list of tool names the model may call unattended.

#### Scenario: No servers configured
- **WHEN** the configuration declares no MCP servers
- **THEN** the agent's toolset contains no MCP-related tool

#### Scenario: Duplicate server name
- **WHEN** the configuration declares two MCP servers with the same name
- **THEN** the system SHALL reject the configuration with a validation error identifying the duplicate name, before any server is spawned

### Requirement: stdio-only transport
The system SHALL support only the stdio transport (a spawned child process communicating over its own stdin/stdout) for MCP servers in this capability.

#### Scenario: Unsupported transport configured
- **WHEN** a configured server names a transport other than stdio
- **THEN** the system SHALL reject the configuration with a validation error at startup, and SHALL NOT spawn that server or silently fall back to stdio

### Requirement: Server initialization
For each configured, valid MCP server, the system SHALL spawn the server's command as a child process and complete the MCP initialization handshake, including retrieving the server's advertised tool list, before routing any call to that server through the proxy tool.

#### Scenario: Server spawns and initializes successfully
- **WHEN** a configured server's process starts and completes the MCP initialize handshake and tool listing
- **THEN** calls naming that server through the proxy tool become routable

#### Scenario: Server fails to spawn or initialize
- **WHEN** a configured server's process fails to start, or does not complete the MCP initialize handshake
- **THEN** the system SHALL exclude that server from the set of servers the proxy tool will route to, SHALL continue constructing the agent and initializing every other configured server, and SHALL make the failure observable (not silently dropped)

### Requirement: Single proxy tool across all servers
The system SHALL expose exactly one tool to the model for all configured MCP servers combined. That tool's arguments SHALL identify which configured server to target, which of that server's advertised tools to invoke, and with what arguments. The system SHALL NOT expose a separately named top-level tool per server or per MCP-advertised tool.

#### Scenario: Model invokes an allow-listed underlying tool
- **WHEN** the model calls the proxy tool naming a configured, initialized server and one of that server's advertised, allow-listed tools, with valid arguments
- **THEN** the system SHALL forward the call to that MCP server and SHALL return the server's result to the model

#### Scenario: Model names a server that is not configured or not initialized
- **WHEN** the model calls the proxy tool naming a server that is not configured, or that failed to initialize
- **THEN** the system SHALL refuse the call with an error describing the problem, and SHALL NOT attempt to contact any server

#### Scenario: Model names a tool the server never advertised
- **WHEN** the model calls the proxy tool naming a configured, initialized server and a tool that server did not advertise in its tool list
- **THEN** the system SHALL refuse the call with an error describing the problem, and SHALL NOT contact the server

#### Scenario: No servers configured or initialized
- **WHEN** no MCP server is configured, or none successfully initializes
- **THEN** the system SHALL NOT attach the proxy tool to the toolset at all

### Requirement: Allow-list enforcement
The system SHALL refuse, without contacting the server, any call naming a tool absent from the targeted server's configured allow-list.

#### Scenario: Call to a disallowed tool
- **WHEN** the model calls the proxy tool naming a server and a tool that server advertises but that is not in that server's configured allow-list
- **THEN** the system SHALL refuse the call with an error explaining the tool is not allowed, and SHALL NOT invoke the underlying MCP tool

#### Scenario: Empty allow-list
- **WHEN** a configured server's allow-list contains no tool names
- **THEN** every call naming that server SHALL be refused, and the proxy tool SHALL still attach to the toolset and remain usable for any other, non-empty-allow-list server

### Requirement: Crash isolation
A single MCP server crashing, hanging, or misbehaving SHALL NOT crash the mate process, SHALL NOT block or fail calls routed to any other tool, and SHALL NOT block or fail calls routed to any other configured MCP server.

#### Scenario: Server process exits mid-session
- **WHEN** an initialized MCP server's process exits while the agent session is still running
- **THEN** subsequent calls naming that server through the proxy tool SHALL fail with a descriptive error rather than hanging, and calls naming every other tool or every other configured MCP server SHALL continue to work normally

### Requirement: No MCP tools for subagents
The system SHALL NOT attach the proxy tool to a subagent's toolset.

#### Scenario: Root agent delegates to a subagent
- **WHEN** a root agent with the proxy tool attached spawns a subagent
- **THEN** the subagent's toolset SHALL contain no MCP proxy tool, regardless of how many MCP servers are configured for the root agent
