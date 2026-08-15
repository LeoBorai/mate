//! `build_agent` (§4, `M1-2`/`M1-3`): turns a [`Backend`] plus an [`AgentSpec`] into a Rig
//! `Agent`.
//!
//! `Agent<M>` is generic over the completion model, and `Backend`'s two provider paths
//! (`M1-3`) produce distinct model types — a HuggingFace-native client and an
//! OpenAI-compatible client are not interchangeable at the type level even though they're
//! both driven by the same Chat Completions wire format. [`BuiltAgent`] carries that
//! distinction forward instead of erasing it.
//!
//! `M4` attaches the toolset built by [`crate::toolset::build_toolset`] and the multi-turn
//! budget (`AgentBuilder::default_max_turns`, backed by [`crate::turn_cap::TurnCapHook`]). The
//! same function builds root agents and subagents alike; a subagent is just an `AgentSpec` with
//! a narrower preamble, a narrower `ToolCtx`, and `may_delegate: false`.

use mate_tool_api::ToolCtx;
use rig::agent::Agent;
use rig::client::AgentClientExt;
use rig::completion::Message;
use rig::providers::{huggingface, openai};
use tokio_util::sync::CancellationToken;

use crate::backend::Backend;
use crate::config::AgentSpec;
use crate::streaming::{self, AgentEventEnvelope, TurnOutcome};
use crate::toolset::build_toolset;
use crate::turn_cap::TurnCapHook;

/// A built agent — one variant per provider path a [`Backend`] can take (§4, `M1-3`).
pub enum BuiltAgent {
    HuggingFace(Agent<huggingface::completion::CompletionModel>),
    OpenAiCompatible(Agent<openai::completion::CompletionModel>),
}

impl BuiltAgent {
    /// Streams one prompt against whichever provider path this agent was built against (`M2`).
    /// See [`streaming::stream_turn`] for the event mapping and cancellation semantics — this
    /// just resolves the match `build_agent` already had to make.
    pub async fn stream_turn(
        &self,
        prompt: impl Into<Message> + Send,
        cancel: &CancellationToken,
        on_event: impl FnMut(AgentEventEnvelope),
    ) -> TurnOutcome {
        match self {
            BuiltAgent::HuggingFace(agent) => {
                streaming::stream_turn(agent, prompt, cancel, on_event).await
            }
            BuiltAgent::OpenAiCompatible(agent) => {
                streaming::stream_turn(agent, prompt, cancel, on_event).await
            }
        }
    }
}

/// Builds a Rig agent against `backend`'s client, configured per `spec`, with `ctx`'s toolset
/// attached (`M4-1`, `M4-2`) and `spec.max_turns` as the multi-turn budget, backed by
/// [`TurnCapHook`] (`M4-4`).
pub fn build_agent(backend: &Backend, spec: &AgentSpec, ctx: ToolCtx) -> BuiltAgent {
    let tools = build_toolset(ctx);
    match backend {
        Backend::HuggingFace(client) => BuiltAgent::HuggingFace(
            client
                .agent(&spec.model)
                .preamble(&spec.preamble)
                .temperature(spec.temperature)
                .max_tokens(spec.max_tokens)
                .tool_server_handle(tools)
                .default_max_turns(spec.max_turns)
                .add_hook(TurnCapHook {
                    max_turns: spec.max_turns,
                })
                .build(),
        ),
        Backend::OpenAiCompatible(client) => BuiltAgent::OpenAiCompatible(
            client
                .agent(&spec.model)
                .preamble(&spec.preamble)
                .temperature(spec.temperature)
                .max_tokens(spec.max_tokens)
                .tool_server_handle(tools)
                .default_max_turns(spec.max_turns)
                .add_hook(TurnCapHook {
                    max_turns: spec.max_turns,
                })
                .build(),
        ),
    }
}
