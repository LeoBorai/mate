//! The `http_request` tool: outbound network access for the agent, hardened
//! against SSRF — scheme allowlist, resolved-IP validation, DNS-rebinding
//! defense, manual redirect handling, header stripping, and response caps.
