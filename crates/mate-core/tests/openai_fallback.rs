//! Exercises the OpenAI-compatible fallback path (§4, `M1-3`) end to end: builds a
//! `Backend::openai_compatible` pointed at a local stub server, builds an agent from it, and
//! confirms a prompt actually round-trips through the OpenAI-shaped `/chat/completions` wire
//! format rather than just constructing without error.

use mate_core::agent::{BuiltAgent, build_agent};
use mate_core::backend::Backend;
use mate_core::config::{AgentSpec, DelegationPolicy, HttpPolicy};
use rig::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn stub_agent_spec() -> AgentSpec {
    AgentSpec {
        model: "stub-model".to_string(),
        sub_provider: None,
        base_url: None,
        preamble: "you are a test agent".to_string(),
        temperature: 0.0,
        max_tokens: 64,
        http: HttpPolicy::default(),
        may_delegate: false,
        delegation: DelegationPolicy::default(),
    }
}

#[tokio::test]
async fn openai_compatible_fallback_round_trips_through_a_stub_server() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-stub",
            "model": "stub-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "pong from stub" },
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;

    let backend = Backend::openai_compatible("dummy-key", server.uri())
        .expect("failed to build the OpenAI-compatible client");

    let agent = match build_agent(&backend, &stub_agent_spec()) {
        BuiltAgent::OpenAiCompatible(agent) => agent,
        BuiltAgent::HuggingFace(_) => panic!("expected the OpenAiCompatible variant"),
    };

    let reply = agent
        .prompt("ping")
        .await
        .expect("prompt against the stub server failed");

    assert_eq!(reply, "pong from stub");
}
