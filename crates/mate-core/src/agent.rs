//! `build_agent` (§4, `M1-2`): turns a [`Backend`] plus an [`AgentSpec`] into a Rig `Agent`.
//!
//! No tools yet — that lands with the tool crates in M4. This ticket wires the part of §4's
//! builder that's available today: model, preamble, temperature, max_tokens. The same
//! function builds root agents and subagents alike; a subagent is just an `AgentSpec` with a
//! narrower preamble and `may_delegate: false`.

use rig::agent::Agent;
use rig::client::AgentClientExt;
use rig::providers::huggingface;

use crate::backend::Backend;
use crate::config::AgentSpec;

/// Builds a Rig agent against `backend`'s HuggingFace client, configured per `spec`.
pub fn build_agent(
    backend: &Backend,
    spec: &AgentSpec,
) -> Agent<huggingface::completion::CompletionModel> {
    backend
        .client()
        .agent(&spec.model)
        .preamble(&spec.preamble)
        .temperature(spec.temperature)
        .max_tokens(spec.max_tokens)
        .build()
}
