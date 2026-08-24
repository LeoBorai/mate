# Providers, agent construction, preambles

Everything here lives in `mate-core`: `backend.rs`, `agent.rs`,
`preamble.rs`, `provider_error.rs`.

## `Backend` (`backend.rs`)

Process-wide, shared across every session and subagent (one connection pool,
one auth setup). Three provider paths, config-selected (`mate-cli`'s
`--backend`/`BackendKind`, default `huggingface`):

- `Backend::huggingface(api_key, sub_provider, base_url)` — the default.
  Talks to HuggingFace Inference Providers natively. `sub_provider` is a
  free-text `Option<&str>` mapped to Rig's `SubProvider` enum (`together`,
  `fireworks`, `sambanova`, `hyperbolic`, `nebius`, `novita`, else
  `hf-inference`/`Custom`) — an **unrecognized name falls back to
  `hf-inference` rather than erroring**, since the partner list moves
  independently of `mate`. `base_url` overrides the router (a dedicated
  Inference Endpoint, or a local server speaking HF's wire format).

  Rig 0.41.0's `SubProvider` only actually affects the outgoing chat-
  completions request for `Fireworks` (it rewrites the model id to
  Fireworks' native `accounts/fireworks/models/…` form) — every other
  partner is otherwise silently ignored: the request URL is always
  `v1/chat/completions` and the model id is passed through unqualified,
  so HF's router auto-selects a provider instead of honoring the one
  `mate` chose, and 404s if auto-selection can't resolve the model.
  `Backend::qualify_model(model)` works around this by appending
  `:<partner-slug>` (HF's documented `model:provider` pinning syntax)
  itself before the model id reaches Rig — `build_agent` calls it rather
  than passing `spec.model` straight through. It's a no-op for the default
  `hf-inference` partner, for `Fireworks` (already qualified by Rig), for
  an already-qualified model id, and whenever `base_url` overrides the
  router (a dedicated endpoint has no partner to select).
- `Backend::openai_compatible(api_key, base_url)` — the escape hatch. Point
  `base_url` at `HF_ROUTER_OPENAI_BASE_URL` (`https://router.huggingface.co/v1`)
  to reach HF's OpenAI-compatible surface if Rig's native HF provider ever
  lags a router change, or at an arbitrary OpenAI-compatible server (local
  TGI/vLLM).
- `Backend::gemini(api_key)` — Google's Gemini API, Rig's native
  `generateContent` client (`rig::providers::gemini`), not an
  OpenAI-compatible shim. Same `api_key` convention as every other path
  (`API_TOKEN`, taken as a value, never read from the environment inside
  `mate-core`). No `sub_provider`/`model_qualifier` concept — `qualify_model`
  is a no-op on this path, same as `OpenAiCompatible`. `verify()` uses Rig's
  own `client.verify()` unmodified: this provider's `VERIFY_PATH` is a real
  model-listing endpoint on `generativelanguage.googleapis.com`, so it
  doesn't need the HF hub-token workaround described below.

`Backend::verify()` is the only method that touches the network; construction
alone never does. For `OpenAiCompatible`, and for `HuggingFace` with a
`base_url` override, it's exactly Rig's `VerifyClient::verify()` — a caller
that overrode `base_url` gets taken at their word about what's listening
there.

For `HuggingFace` on the **default** router path, it does *not* call Rig's
`verify()`. That built-in call sends `GET {base_url}/api/whoami-v2`, and
`/api/whoami-v2` is a Hub account-info route — it lives on `huggingface.co`,
not `router.huggingface.co` (the router only proxies inference endpoints:
chat completions, etc.). So `client.verify()` 404s unconditionally on the
default path, before a single agent gets built, regardless of model,
sub-provider, or token validity — this was the actual cause of a "404 Not
Found" that reproduced no matter what model or sub-provider was configured.
`verify_huggingface_hub_token` sidesteps it by hitting
`https://huggingface.co/api/whoami-v2` directly, over its own
`reqwest::Client` (`hub_verify_client`, `Authorization` header pre-set at
construction — Rig's `huggingface::Client` never exposes the bearer token it
holds internally, so a check against a *different* host needs its own
client). It still returns Rig's own `VerifyError` variants
(`InvalidAuthentication`, `HttpError`), so `ProviderError::classify` and
everything downstream is unaffected.

The one test that hits the live network (`crates/mate-core/tests/hf_backend.rs`)
is `#[ignore]`d and needs a real `API_TOKEN` — never run it in CI.
`crates/mate-core/tests/provider_error_mapping.rs` covers the status-code
classification offline instead, against a `base_url`-overridden backend (a
wiremock server) — that's exactly the path that still goes through
`client.verify()` unchanged.

`API_TOKEN` is taken as a value by `Backend`'s constructors, never read from
the environment inside `mate-core` — see `config.md`.

## `build_agent` / `BuiltAgent` (`agent.rs`)

```rust
pub enum BuiltAgent {
    HuggingFace(Agent<huggingface::completion::CompletionModel>),
    OpenAiCompatible(Agent<openai::completion::CompletionModel>),
    Gemini(Agent<gemini::completion::CompletionModel>),
}
```

One function, `build_agent(backend, spec)`, builds both root agents and
subagents — a subagent is just an `AgentSpec` with a narrower preamble and
`may_delegate: false`. `Agent<M>` is generic over the completion model, and
the three `Backend` variants produce distinct model types, so `BuiltAgent`
carries that distinction forward instead of erasing it — expect to `match` on
it (see `streaming.rs`'s `stream_turn` for the pattern). Every exhaustive
`match` on `BuiltAgent` needs a `Gemini` arm too — `session.rs`'s
`spawn_supervised` dispatch and `subagent.rs`'s `drive_subagent` dispatch are
the other two sites, besides `stream_turn`.

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
