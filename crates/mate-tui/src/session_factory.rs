//! Builds the pieces `SessionManager::spawn` needs (`M8-3`, `M8-5`): the same [`SessionSpec`] /
//! [`ToolCtx`] assembly `mate-cli`'s `tui.rs` already does once at startup, factored out so the
//! `Ctrl+T` spawn form (`crate::app`) can do it again for every tab opened after that, without
//! `mate-tui` reaching back into `mate-cli`.
//!
//! [`SessionDefaults`] is the knob set every new tab starts from — the loaded `Config`, minus
//! the workspace root and title, which are per-tab. Cloning it per spawn is cheap (a handful of
//! strings and small enums) and keeps the borrow checker out of the spawn form entirely.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use mate_core::agents_md::discover_agents_md_capped;
use mate_core::config::{AgentSpec, DelegationPolicy, HttpPolicy, SessionSpec};
use mate_core::preamble::{PreambleRole, SkillDescriptor, render_preamble};
use mate_core::toolset::tool_descriptors;
use mate_tool_api::{AgentId, ToolCtx};
use tokio_util::sync::CancellationToken;

/// Everything a new session's [`AgentSpec`] needs besides its workspace root and title —
/// carried once from `mate-cli`'s loaded `Config` and reused for every tab, including ones
/// opened later through the spawn form.
#[derive(Debug, Clone)]
pub struct SessionDefaults {
    pub model: String,
    /// Which `Backend` path the process is talking through ("huggingface", "gemini", …) —
    /// `mate-tui` doesn't know the concrete `Backend`/`BackendKind` types (`cli` depends on
    /// `tui`, not the reverse), so `mate-cli` hands this in as a plain label. Only used as
    /// [`SessionDefaults::provider_label`]'s fallback when `sub_provider` is unset.
    pub backend_name: String,
    pub sub_provider: Option<String>,
    pub temperature: f64,
    pub max_tokens: u64,
    pub max_turns: usize,
    pub http: HttpPolicy,
    pub delegation: DelegationPolicy,
    pub max_output_bytes: usize,
    pub agents_md_enabled: bool,
    pub agents_md_max_bytes: usize,
}

impl SessionDefaults {
    /// The provider label shown in a tab's status bar — `Backend` never stores this as a
    /// string (§ providers.md), so it's derived here from config the same way `mate-cli`
    /// already did for the single-tab `M7` wiring.
    pub fn provider_label(&self) -> String {
        self.sub_provider
            .clone()
            .unwrap_or_else(|| self.backend_name.clone())
    }

    /// The subagent model the status panel's `MODEL` widget shows on its third line (§9.4) —
    /// `None` when delegation is off, since there's then no subagent model in play to show.
    pub fn subagent_model_label(&self) -> Option<String> {
        if !self.delegation.enabled {
            return None;
        }
        self.delegation.subagent_model.clone()
    }
}

/// Assembles one session's spec (§4, §5.1) from `defaults` plus the per-tab `root` and `title`.
/// `http_enabled` overrides `defaults.http.enabled` only — the rest of the http policy (rate
/// limit, public/localhost) stays shared, matching §7.4's "narrowing-only" rule for anything
/// that scopes a tab down rather than up.
pub fn build_spec(
    defaults: &SessionDefaults,
    root: &Path,
    title: String,
    http_enabled: bool,
) -> SessionSpec {
    let mut http = defaults.http.clone();
    http.enabled = http_enabled;

    let skills = mate_tool_skills::discover_skills(root);
    let skill_descriptors: Vec<SkillDescriptor> = skills
        .iter()
        .map(|s| SkillDescriptor::new(s.name.clone(), s.description.clone()))
        .collect();
    let agents_md = discover_agents_md_capped(
        root,
        defaults.agents_md_enabled,
        defaults.agents_md_max_bytes,
    );
    let preamble = render_preamble(
        PreambleRole::Root,
        root,
        std::env::consts::OS,
        &tool_descriptors(
            defaults.delegation.enabled,
            http_enabled,
            !skills.is_empty(),
        ),
        &skill_descriptors,
        agents_md.as_ref(),
    );

    let agent = AgentSpec {
        model: defaults.model.clone(),
        sub_provider: defaults.sub_provider.clone(),
        base_url: None,
        preamble,
        temperature: defaults.temperature,
        max_tokens: defaults.max_tokens,
        max_turns: defaults.max_turns,
        http,
        may_delegate: defaults.delegation.enabled,
        delegation: defaults.delegation.clone(),
    };

    SessionSpec {
        title,
        root: root.to_path_buf(),
        agent,
        delegation: defaults.delegation.clone(),
        max_turns: defaults.max_turns,
    }
}

/// Builds the root agent's [`ToolCtx`] for a new session. The activity sink's receiver is
/// dropped immediately — `SessionManager::spawn` (`M11-4`) overwrites `ctx.activity` with its
/// own channel before the agent is built, the same way it already overwrites `ctx.cancel`, so
/// this one is just a placeholder that's never actually read from.
pub fn build_tool_ctx(
    root: PathBuf,
    max_output_bytes: usize,
    agents_md_enabled: bool,
    agents_md_max_bytes: usize,
) -> ToolCtx {
    let (activity, _activity_rx) = tokio::sync::mpsc::channel(64);
    let skills = mate_tool_skills::discover_skills(&root);
    let agents_md = discover_agents_md_capped(&root, agents_md_enabled, agents_md_max_bytes);
    ToolCtx {
        agent: AgentId::ROOT,
        root,
        max_output_bytes,
        spawner: None,
        activity,
        cancel: CancellationToken::new(),
        approvals: None,
        skills: Arc::from(skills),
        agents_md: agents_md.map(Arc::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> SessionDefaults {
        SessionDefaults {
            model: "org/model".to_string(),
            backend_name: "huggingface".to_string(),
            sub_provider: None,
            temperature: 0.2,
            max_tokens: 512,
            max_turns: 4,
            http: HttpPolicy::default(),
            delegation: DelegationPolicy::default(),
            max_output_bytes: 1_000_000,
            agents_md_enabled: true,
            agents_md_max_bytes: 32_768,
        }
    }

    #[test]
    fn provider_label_falls_back_to_huggingface_when_unset() {
        assert_eq!(defaults().provider_label(), "huggingface");
    }

    #[test]
    fn provider_label_falls_back_to_the_configured_backend_when_unset() {
        let mut d = defaults();
        d.backend_name = "gemini".to_string();
        assert_eq!(d.provider_label(), "gemini");
    }

    #[test]
    fn provider_label_reflects_an_explicit_sub_provider() {
        let mut d = defaults();
        d.sub_provider = Some("together".to_string());
        assert_eq!(d.provider_label(), "together");
    }

    #[test]
    fn subagent_model_label_is_none_when_delegation_is_disabled() {
        let mut d = defaults();
        d.delegation.enabled = false;
        d.delegation.subagent_model = Some("org/sub-model".to_string());
        assert_eq!(d.subagent_model_label(), None);
    }

    #[test]
    fn subagent_model_label_carries_the_configured_model_when_delegation_is_on() {
        let mut d = defaults();
        d.delegation.enabled = true;
        d.delegation.subagent_model = Some("org/sub-model".to_string());
        assert_eq!(d.subagent_model_label(), Some("org/sub-model".to_string()));
    }

    #[test]
    fn http_enabled_overrides_only_the_enabled_flag() {
        let mut d = defaults();
        d.http.rate_limit_per_host_per_min = 7;
        let spec = build_spec(&d, Path::new("/tmp"), "t".to_string(), false);
        assert!(!spec.agent.http.enabled);
        assert_eq!(spec.agent.http.rate_limit_per_host_per_min, 7);
    }

    #[test]
    fn build_spec_carries_the_title_and_root_through() {
        let d = defaults();
        let spec = build_spec(&d, Path::new("/work/api"), "api".to_string(), true);
        assert_eq!(spec.title, "api");
        assert_eq!(spec.root, PathBuf::from("/work/api"));
        assert_eq!(spec.agent.model, "org/model");
    }

    #[test]
    fn build_spec_folds_a_discovered_agents_md_into_the_preamble() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "Run `just test`.").unwrap();

        let d = defaults();
        let spec = build_spec(&d, tmp.path(), "t".to_string(), true);
        assert!(
            spec.agent
                .preamble
                .contains("Project instructions (AGENTS.md):\nRun `just test`."),
            "preamble: {}",
            spec.agent.preamble
        );
    }

    #[test]
    fn build_spec_omits_agents_md_when_disabled_in_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "Run `just test`.").unwrap();

        let mut d = defaults();
        d.agents_md_enabled = false;
        let spec = build_spec(&d, tmp.path(), "t".to_string(), true);
        assert!(!spec.agent.preamble.contains("Project instructions"));
    }

    #[test]
    fn build_tool_ctx_carries_the_discovered_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "Run `just test`.").unwrap();

        let ctx = build_tool_ctx(tmp.path().to_path_buf(), 1_000_000, true, 32_768);
        let source = ctx
            .agents_md
            .expect("AGENTS.md should have been discovered");
        assert_eq!(source.filename, "AGENTS.md");
        assert_eq!(source.content, "Run `just test`.");
    }
}
