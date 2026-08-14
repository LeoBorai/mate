//! `build_agent` (§4, `M1-2`/`M1-3`): turns a [`Backend`] plus an [`AgentSpec`] into a Rig
//! `Agent`.
//!
//! `Agent<M>` is generic over the completion model, and `Backend`'s two provider paths
//! (`M1-3`) produce distinct model types — a HuggingFace-native client and an
//! OpenAI-compatible client are not interchangeable at the type level even though they're
//! both driven by the same Chat Completions wire format. [`BuiltAgent`] carries that
//! distinction forward instead of erasing it.
//!
//! No tools yet — that lands with the tool crates in M4. The same function builds root
//! agents and subagents alike; a subagent is just an `AgentSpec` with a narrower preamble
//! and `may_delegate: false`.

use rig::agent::Agent;
use rig::client::AgentClientExt;
use rig::providers::{huggingface, openai};

use crate::backend::Backend;
use crate::config::AgentSpec;

/// A built agent — one variant per provider path a [`Backend`] can take (§4, `M1-3`).
pub enum BuiltAgent {
    HuggingFace(Agent<huggingface::completion::CompletionModel>),
    OpenAiCompatible(Agent<openai::completion::CompletionModel>),
}

/// Builds a Rig agent against `backend`'s client, configured per `spec`.
pub fn build_agent(backend: &Backend, spec: &AgentSpec) -> BuiltAgent {
    match backend {
        Backend::HuggingFace(client) => BuiltAgent::HuggingFace(
            client
                .agent(&spec.model)
                .preamble(&spec.preamble)
                .temperature(spec.temperature)
                .max_tokens(spec.max_tokens)
                .build(),
        ),
        Backend::OpenAiCompatible(client) => BuiltAgent::OpenAiCompatible(
            client
                .agent(&spec.model)
                .preamble(&spec.preamble)
                .temperature(spec.temperature)
                .max_tokens(spec.max_tokens)
                .build(),
        ),
    }
}
