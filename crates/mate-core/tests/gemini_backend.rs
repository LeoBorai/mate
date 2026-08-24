//! Exercises the Gemini path (§4, `M1-3`) through the real `build_agent`, offline: confirms
//! `Backend::gemini` builds a `BuiltAgent::Gemini` with the fs toolset attached, the same
//! `M4-2` guarantee `tests/openai_fallback.rs` checks for the OpenAI-compatible path. No live
//! network call — `tool_definitions` reads the local registry `build_agent` populated at
//! construction time, and Gemini's wire format differs enough from OpenAI's that a stubbed
//! round-trip belongs in its own test once that shape is actually exercised, not guessed at
//! here.

mod support;

use mate_core::agent::{BuiltAgent, build_agent};
use mate_core::backend::Backend;
use mate_core::config::{AgentSpec, DelegationPolicy, HttpPolicy};
use rig::prelude::*;

fn stub_agent_spec() -> AgentSpec {
    AgentSpec {
        model: "gemini-2.5-flash".to_string(),
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
async fn build_agent_attaches_the_fs_toolset_on_the_gemini_path() {
    let backend = Backend::gemini("dummy-key").expect("failed to build the Gemini client");
    let tmp = tempfile::tempdir().unwrap();

    let agent = match build_agent(
        &backend,
        &support::http_shared(),
        &stub_agent_spec(),
        support::tool_ctx(tmp.path().to_path_buf()),
    ) {
        BuiltAgent::Gemini(agent) => agent,
        BuiltAgent::HuggingFace(_) | BuiltAgent::OpenAiCompatible(_) => {
            panic!("expected the Gemini variant")
        }
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
        vec![
            "find_files",
            "http_request",
            "list_dir",
            "read_file",
            "write_file"
        ],
        "stub_agent_spec's HttpPolicy::default() has enabled: true, so http_request must attach"
    );
}
