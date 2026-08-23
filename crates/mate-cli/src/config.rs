//! Layered config loading (§10): flags → env → `./.mate.toml` →
//! `~/.config/mate/config.toml` → defaults, via `figment`.
//!
//! `API_TOKEN` is deliberately not a field on `Config` — it is read from the environment only,
//! by [`api_token`], and never flows through the figment chain, so it can never be written to
//! or read back from a config file.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use mate_core::config::{DelegationPolicy, HttpAccessPolicy, HttpPolicy};
use serde::{Deserialize, Serialize};

use crate::cli::Cli;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub sub_provider: Option<String>,
    pub max_sessions: usize,
    pub max_turns: usize,
    pub delegation: DelegationPolicy,
    pub tools: ToolsConfig,
    pub panel: PanelConfig,
    pub pricing: HashMap<String, PricingEntry>,
    pub http: HttpPolicy,
    pub agents_md: AgentsMdConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "Qwen/Qwen3-Coder-30B-A3B-Instruct".to_string(),
            sub_provider: None,
            max_sessions: 8,
            max_turns: 12,
            delegation: DelegationPolicy::default(),
            tools: ToolsConfig::default(),
            panel: PanelConfig::default(),
            pricing: HashMap::from([
                (
                    "Qwen/Qwen3-Coder-30B-A3B-Instruct".to_string(),
                    PricingEntry {
                        input: 0.40,
                        output: 1.60,
                    },
                ),
                (
                    "Qwen/Qwen3-32B".to_string(),
                    PricingEntry {
                        input: 0.10,
                        output: 0.30,
                    },
                ),
            ]),
            http: HttpPolicy::default(),
            agents_md: AgentsMdConfig::default(),
        }
    }
}

/// Project-instructions file support (`AGENTS.md`, `CLAUDE.md`, and other agent frameworks'
/// filenames for the same concept — `mate_core::agents_md::discover_agents_md`). `max_bytes`
/// bounds the section's size in the rendered preamble, not the file on disk: a larger file is
/// read in full and then truncated, matching `truncate_with_notice`'s contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsMdConfig {
    pub enabled: bool,
    pub max_bytes: usize,
}

impl Default for AgentsMdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: 32_768,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub max_output_bytes: usize,
    pub deny: Vec<String>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            max_output_bytes: 262_144,
            deny: vec![
                ".env".to_string(),
                "*.pem".to_string(),
                "id_rsa*".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelConfig {
    pub visible: bool,
    pub width_ratio: [u32; 2],
    pub network_log_len: usize,
    pub documents_log_len: usize,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            visible: true,
            width_ratio: [3, 12],
            network_log_len: 50,
            documents_log_len: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PricingEntry {
    pub input: f64,
    pub output: f64,
}

/// `API_TOKEN`, read fresh from the environment every call. Never persisted, never part of
/// `Config` — the one exemption from the figment chain, by design.
pub fn api_token() -> Option<String> {
    std::env::var("API_TOKEN").ok()
}

fn user_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("mate/config.toml"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/mate/config.toml"))
}

/// Load config with precedence flags → env (`MATE_*`) → `./.mate.toml` (or `--config`) →
/// `~/.config/mate/config.toml` → defaults. Missing files are silently skipped, not errors.
pub fn load(cli: &Cli) -> anyhow::Result<Config> {
    let mut fig = Figment::from(Serialized::defaults(Config::default()));

    if let Some(path) = user_config_path() {
        fig = fig.merge(Toml::file(path));
    }

    let project_path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(".mate.toml"));
    fig = fig.merge(Toml::file(project_path));

    fig = fig.merge(Env::prefixed("MATE_"));

    let mut config: Config = fig.extract().context("failed to load config")?;
    apply_flags(&mut config, cli);
    Ok(config)
}

fn apply_flags(config: &mut Config, cli: &Cli) {
    if let Some(model) = &cli.model {
        config.model = model.clone();
    }
    if let Some(provider) = &cli.provider {
        config.sub_provider = Some(provider.clone());
    }
    if let Some(subagent_model) = &cli.subagent_model {
        config.delegation.subagent_model = Some(subagent_model.clone());
    }
    if let Some(n) = cli.max_sessions {
        config.max_sessions = n;
    }
    if let Some(n) = cli.max_turns {
        config.max_turns = n;
    }
    if let Some(n) = cli.max_subagents {
        config.delegation.max_concurrent = n;
    }
    if cli.no_http {
        config.http.enabled = false;
    }
    if cli.no_delegate {
        config.delegation.enabled = false;
    }
    if cli.http_allow_localhost {
        config.http.policy = HttpAccessPolicy::AllowLocalhost;
    }
}

#[cfg(test)]
// `Jail::expect_with` fixes its closure's error to `figment::Error`, which clippy flags as
// large; not ours to box, since `Jail` decides the closure signature.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use clap::Parser;
    use figment::Jail;

    fn cli(args: &[&str]) -> Cli {
        let mut argv = vec!["mate"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv)
    }

    #[test]
    fn defaults_when_nothing_else_present() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);

            let config = load(&cli(&[])).unwrap();
            assert_eq!(config, Config::default());
            Ok(())
        });
    }

    #[test]
    fn user_config_file_overrides_defaults() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            std::fs::create_dir_all(jail.directory().join(".config/mate")).unwrap();
            jail.create_file(
                ".config/mate/config.toml",
                r#"
                model = "from-user-file"
                max_sessions = 3
                "#,
            )?;

            let config = load(&cli(&[])).unwrap();
            assert_eq!(config.model, "from-user-file");
            assert_eq!(config.max_sessions, 3);
            Ok(())
        });
    }

    #[test]
    fn project_config_file_overrides_user_file() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            std::fs::create_dir_all(jail.directory().join(".config/mate")).unwrap();
            jail.create_file(
                ".config/mate/config.toml",
                r#"
                model = "from-user-file"
                max_sessions = 3
                "#,
            )?;
            jail.create_file(
                ".mate.toml",
                r#"
                model = "from-project-file"
                "#,
            )?;

            let config = load(&cli(&[])).unwrap();
            assert_eq!(config.model, "from-project-file");
            // Untouched by the project file, still inherited from the user file.
            assert_eq!(config.max_sessions, 3);
            Ok(())
        });
    }

    #[test]
    fn env_overrides_project_file() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.create_file(
                ".mate.toml",
                r#"
                model = "from-project-file"
                "#,
            )?;
            jail.set_env("MATE_MODEL", "from-env");

            let config = load(&cli(&[])).unwrap();
            assert_eq!(config.model, "from-env");
            Ok(())
        });
    }

    #[test]
    fn flags_override_env() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.set_env("MATE_MODEL", "from-env");

            let config = load(&cli(&["--model", "from-flag"])).unwrap();
            assert_eq!(config.model, "from-flag");
            Ok(())
        });
    }

    #[test]
    fn explicit_config_flag_replaces_project_file_lookup() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.create_file(
                "custom.toml",
                r#"
                model = "from-custom-file"
                "#,
            )?;

            let config = load(&cli(&["--config", "custom.toml"])).unwrap();
            assert_eq!(config.model, "from-custom-file");
            Ok(())
        });
    }

    #[test]
    fn agents_md_max_bytes_is_overridable_via_project_file() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.create_file(
                ".mate.toml",
                r#"
                [agents_md]
                max_bytes = 4096
                "#,
            )?;

            let config = load(&cli(&[])).unwrap();
            assert_eq!(config.agents_md.max_bytes, 4096);
            assert!(config.agents_md.enabled, "unset fields keep their default");
            Ok(())
        });
    }

    #[test]
    fn api_token_is_env_only_and_not_a_config_field() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.create_file(
                ".mate.toml",
                r#"
                api_token = "should-be-ignored"
                "#,
            )?;
            jail.set_env("API_TOKEN", "secret-token");

            let config = load(&cli(&[])).unwrap();
            // Serializing Config back out must never surface a token.
            let round_trip = toml::to_string(&config).unwrap();
            assert!(!round_trip.contains("secret-token"));
            assert!(!round_trip.to_lowercase().contains("api_token"));

            assert_eq!(api_token().as_deref(), Some("secret-token"));
            Ok(())
        });
    }
}
