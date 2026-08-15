# Providers, agent construction, preambles

Everything here lives in `mate-core`: `backend.rs`, `agent.rs`,
`preamble.rs`, `provider_error.rs`.

## `Backend` (`backend.rs`)

Process-wide, shared across every session and subagent (one connection pool,
one auth setup). Two provider paths, config-selected:

- `Backend::huggingface(api_key, sub_provider, base_url)` — the default.
  Talks to HuggingFace Inference Providers natively. `sub_provider` is a
  free-text `Option<&str>` mapped to Rig's `SubProvider` enum (`together`,
  `fireworks`, `sambanova`, `hyperbolic`, `nebius`, `novita`, else
  `hf-inference`/`Custom`) — an **unrecognized name falls back to
  `hf-inference` rather than erroring**, since the partner list moves
  independently of `mate`. `base_url` overrides the router (a dedicated
  Inference Endpoint, or a local server speaking HF's wire format).
- `Backend::openai_compatible(api_key, base_url)` — the escape hatch. Point
  `base_url` at `HF_ROUTER_OPENAI_BASE_URL` (`https://router.huggingface.co/v1`)
  to reach HF's OpenAI-compatible surface if Rig's native HF provider ever
  lags a router change, or at an arbitrary OpenAI-compatible server (local
  TGI/vLLM).

`Backend::verify()` calls Rig's `VerifyClient::verify()` — the only method
that touches the network; construction alone never does. The one test that
hits the live router (`crates/mate-core/tests/hf_backend.rs`) is `#[ignore]`d
and needs a real `API_TOKEN` — never run it in CI.

`API_TOKEN` is taken as a value by `Backend`'s constructors, never read from
the environment inside `mate-core` — see `config.md`.

## `build_agent` / `BuiltAgent` (`agent.rs`)

```rust
pub enum BuiltAgent {
    HuggingFace(Agent<huggingface::completion::CompletionModel>),
    OpenAiCompatible(Agent<openai::completion::CompletionModel>),
}
```

One function, `build_agent(backend, spec)`, builds both root agents and
subagents — a subagent is just an `AgentSpec` with a narrower preamble and
`may_delegate: false`. `Agent<M>` is generic over the completion model, and
the two `Backend` variants produce distinct model types, so `BuiltAgent`
carries that distinction forward instead of erasing it — expect to `match` on
it (see `streaming.rs`'s `stream_turn` for the pattern).

No tools are attached yet — that lands with the tool crates.

## Preambles (`preamble.rs`)

`render_preamble(role, workspace_root, os, tools)` renders the system prompt.
`PreambleRole::{Root, Subagent}` only changes the intro paragraph — the
workspace/OS/tool-list scaffolding is identical, since a subagent's tool
context is the same shape as a root agent's, just narrower. `ToolDescriptor`
is a plain `{name, description}` pair for now; once tool crates land, the real
list gets derived from each agent's attached `ToolSet` instead of being passed
by hand.

## Provider error mapping (`provider_error.rs`)

`ProviderError::classify(&err)` turns any Rig capability error
(`CompletionError`, `VerifyError`, …) into one of: `RateLimited` (429),
`ModelWarming` (503), `AuthFailed` (401/403, or Rig's own
`InvalidAuthentication` — **never retryable**), `ServerError` (other 5xx), or
`Other` (everything else, not retryable). Add support for a new Rig error type
by implementing the `ProviderResponse` trait for it, not by hand-rolling a new
classifier.

`RetryPolicy::backoff(&error, attempt)` is **full jitter** (uniform draw over
`[0, backoff]`), not fixed exponential — with several sessions/subagents
retrying the same rate limit concurrently, a fixed schedule has them all
retry in lockstep and re-trip the limit together. Default: 500ms base,
factor 2.0, 30s ceiling, 5 max retries.
