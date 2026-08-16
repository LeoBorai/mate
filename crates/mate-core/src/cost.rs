//! Cost estimation from a [`UsageRollup`] (§9.5, `M11-6`). HuggingFace routes chat completions
//! through partner providers and bills the account at each partner's own rate — Rig 0.41.0's
//! completion response carries only `usage: Usage` off the parsed JSON body
//! (`rig::completion::CompletionResponse::raw_response` is the provider's raw JSON, not the HTTP
//! response — no header access), and the JSON body itself (OpenAI-compatible `usage` object:
//! `prompt_tokens`/`completion_tokens`/`total_tokens`, plus timing) carries no billing or cost
//! field either. So there is no billing metadata to prefer over a user-maintained price table —
//! this is the *only* path, not a fallback for one that doesn't exist yet.
//!
//! `ModelRate` deliberately isn't `mate-cli`'s `PricingEntry` — `mate-core` can't depend on the
//! CLI-facing config shape (config.md), so it defines the minimal pair of numbers cost math
//! actually needs and leaves the TOML-facing type and its loading to `mate-cli`.

use std::collections::HashMap;

use rig::completion::Usage;

use crate::streaming::UsageRollup;

/// USD per 1M tokens for one model, the shape [`estimate_cost`] needs — not `mate-cli`'s
/// `PricingEntry` (see module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRate {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// A session's estimated spend (§9.5). `known: false` — never a silently-zero `total_usd` —
/// is what a model missing from `pricing` reports, so the widget can show `~$?` instead of a
/// number that looks real but isn't.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    pub total_usd: f64,
    /// Cost per completed root turn, over the whole session — the number that predicts what
    /// the *next* question costs, not a lifetime total nobody mid-task is asking for.
    pub per_turn_avg: f64,
    pub known: bool,
}

fn turn_cost(usage: &Usage, rate: ModelRate) -> f64 {
    (usage.input_tokens as f64 / 1_000_000.0) * rate.input_per_million
        + (usage.output_tokens as f64 / 1_000_000.0) * rate.output_per_million
}

/// Estimates `rollup`'s cost against `pricing`. `subagent_model` is only consulted when the
/// rollup actually carries subagent usage — a session that never delegated shouldn't report
/// `known: false` just because its (unused) subagent model has no price entry.
pub fn estimate_cost(
    rollup: &UsageRollup,
    root_model: &str,
    subagent_model: &str,
    pricing: &HashMap<String, ModelRate>,
) -> CostEstimate {
    let root_rate = pricing.get(root_model).copied();
    let has_subagent_usage = rollup.subagents.total_tokens > 0;
    let subagent_rate = if has_subagent_usage {
        pricing.get(subagent_model).copied()
    } else {
        None
    };

    let known = root_rate.is_some() && (!has_subagent_usage || subagent_rate.is_some());
    if !known {
        return CostEstimate {
            total_usd: 0.0,
            per_turn_avg: 0.0,
            known: false,
        };
    }

    let mut total = turn_cost(&rollup.root, root_rate.expect("checked by `known` above"));
    if let Some(rate) = subagent_rate {
        total += turn_cost(&rollup.subagents, rate);
    }
    let per_turn_avg = if rollup.turns > 0 {
        total / rollup.turns as f64
    } else {
        0.0
    };

    CostEstimate {
        total_usd: total,
        per_turn_avg,
        known: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    fn pricing() -> HashMap<String, ModelRate> {
        HashMap::from([(
            "root-model".to_string(),
            ModelRate {
                input_per_million: 0.40,
                output_per_million: 1.60,
            },
        )])
    }

    #[test]
    fn a_priced_model_with_no_subagent_usage_computes_total_and_average() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(usage(1_000_000, 500_000));
        rollup.record_root_turn(usage(1_000_000, 500_000));

        let estimate = estimate_cost(&rollup, "root-model", "sub-model", &pricing());

        assert!(estimate.known);
        // Two turns: (1.0 * 0.40 + 0.5 * 1.60) * 2 = (0.40 + 0.80) * 2 = 2.40
        assert!(
            (estimate.total_usd - 2.40).abs() < 1e-9,
            "total_usd was {}",
            estimate.total_usd
        );
        assert!(
            (estimate.per_turn_avg - 1.20).abs() < 1e-9,
            "per_turn_avg was {}",
            estimate.per_turn_avg
        );
    }

    #[test]
    fn an_unpriced_root_model_reports_known_false_never_a_silent_zero_that_looks_real() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(usage(1_000_000, 500_000));

        let estimate = estimate_cost(&rollup, "no-such-model", "sub-model", &pricing());

        assert!(!estimate.known);
        assert_eq!(estimate.total_usd, 0.0);
    }

    #[test]
    fn an_unpriced_subagent_model_reports_known_false_only_once_subagents_actually_ran() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(usage(1_000_000, 0));

        // No subagent usage recorded yet: the unpriced subagent model must not poison a
        // session that never delegated.
        let estimate = estimate_cost(&rollup, "root-model", "no-such-model", &pricing());
        assert!(estimate.known);

        rollup.record_subagent_turn(usage(1_000_000, 0));
        let estimate = estimate_cost(&rollup, "root-model", "no-such-model", &pricing());
        assert!(
            !estimate.known,
            "once a subagent actually ran on an unpriced model, the estimate can't be trusted"
        );
    }

    #[test]
    fn subagent_usage_is_priced_at_the_subagent_models_rate_not_the_roots() {
        let mut rollup = UsageRollup::default();
        rollup.record_root_turn(usage(1_000_000, 0));
        rollup.record_subagent_turn(usage(1_000_000, 0));

        let mut pricing = pricing();
        pricing.insert(
            "sub-model".to_string(),
            ModelRate {
                input_per_million: 0.10,
                output_per_million: 0.30,
            },
        );

        let estimate = estimate_cost(&rollup, "root-model", "sub-model", &pricing);

        // root: 1.0 * 0.40 = 0.40; subagents: 1.0 * 0.10 = 0.10; total = 0.50
        assert!(
            (estimate.total_usd - 0.50).abs() < 1e-9,
            "total_usd was {}",
            estimate.total_usd
        );
    }

    #[test]
    fn zero_turns_reports_a_zero_average_rather_than_dividing_by_zero() {
        let rollup = UsageRollup::default();
        let estimate = estimate_cost(&rollup, "root-model", "sub-model", &pricing());
        assert!(estimate.known);
        assert_eq!(estimate.total_usd, 0.0);
        assert_eq!(estimate.per_turn_avg, 0.0);
    }
}
