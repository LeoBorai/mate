//! Exercises the OpenAI-compatible fallback path (§4, `M1-3`) end to end: builds a
//! `Backend::openai_compatible` pointed at a local stub server, builds an agent from it, and
//! confirms a prompt actually round-trips through the OpenAI-shaped `/chat/completions` wire
//! format rather than just constructing without error.

mod support;

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
        max_turns: 4,
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
    let tmp = tempfile::tempdir().unwrap();

    let agent = match build_agent(
        &backend,
        &support::http_shared(),
        &stub_agent_spec(),
        support::tool_ctx(tmp.path().to_path_buf()),
    ) {
        BuiltAgent::OpenAiCompatible(agent) => agent,
        BuiltAgent::HuggingFace(_) => panic!("expected the OpenAiCompatible variant"),
    };

    let reply = agent
        .prompt("ping")
        .await
        .expect("prompt against the stub server failed");

    assert_eq!(reply, "pong from stub");
}

/// `M4-2`'s "tools attached" half, exercised through the real `build_agent` path rather than
/// `build_toolset` directly (that's `tests/tool_round_trip.rs`'s job): no network call needed —
/// `tool_definitions` reads the local registry `build_agent` populated at construction time.
#[tokio::test]
async fn build_agent_attaches_the_fs_toolset() {
    let backend = Backend::openai_compatible("dummy-key", "http://127.0.0.1:0")
        .expect("failed to build the OpenAI-compatible client");
    let tmp = tempfile::tempdir().unwrap();

    let agent = match build_agent(
        &backend,
        &support::http_shared(),
        &stub_agent_spec(),
        support::tool_ctx(tmp.path().to_path_buf()),
    ) {
        BuiltAgent::OpenAiCompatible(agent) => agent,
        BuiltAgent::HuggingFace(_) => panic!("expected the OpenAiCompatible variant"),
    };

    let mut names: Vec<String> = agent
        .tool_definitions(None)
        .await
        .expect("tool_definitions should resolve without any network call")
        .into_iter()
        .map(|def| def.name)
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec!["find_files", "http_request", "list_dir", "read_file"],
        "stub_agent_spec's HttpPolicy::default() has enabled: true, so http_request must attach"
    );
}
