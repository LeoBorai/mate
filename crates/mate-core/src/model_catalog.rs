//! A static, in-binary catalog of which models each supported backend offers, with the display
//! name and per-million-token pricing the onboarding flow (`mate-tui`) needs to let a user pick
//! one without already knowing an exact model id.
//!
//! The entries themselves live in `model_catalog/generated.rs`, a committed source file
//! emitted by `just gen-model-catalog` (`crates/xtask-model-catalog`) from a pinned commit of
//! `anomalyco/models.dev`. It is plain Rust data — no runtime parsing, no network at build time.
//! Regenerate it deliberately; never edit it by hand.
//!
//! [`CatalogBackend`] is deliberately not `mate-cli`'s `BackendKind`: that one derives
//! `clap::ValueEnum`, and `mate-core` takes no `clap` dependency. `mate-cli` converts between
//! the two at the one place that needs to.

use crate::cost::ModelRate;

mod generated;

/// Which provider a catalog entry belongs to — the two backends `mate` can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogBackend {
    Huggingface,
    Gemini,
}

impl CatalogBackend {
    /// Every backend, in the order onboarding lists them.
    pub const ALL: [CatalogBackend; 2] = [Self::Huggingface, Self::Gemini];

    /// Human-readable name for pickers.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Huggingface => "Hugging Face",
            Self::Gemini => "Google Gemini",
        }
    }
}

/// One selectable model. `id` is exactly what `Config.model` expects for `backend` — the
/// `Org/Model` shape on Hugging Face, a bare `gemini-…` name on Gemini — so it can be handed
/// to that backend with no reformatting. `pricing` is `None` when the source data carries no
/// price for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub backend: CatalogBackend,
    pub pricing: Option<ModelRate>,
}

/// Every catalog entry, all backends together.
pub fn all() -> &'static [ModelEntry] {
    generated::CATALOG
}

/// The entries belonging to `backend`, in catalog order.
pub fn models_for(backend: CatalogBackend) -> impl Iterator<Item = &'static ModelEntry> {
    all().iter().filter(move |entry| entry.backend == backend)
}

/// The entry for `id` on `backend`, if the catalog lists it.
pub fn find(backend: CatalogBackend, id: &str) -> Option<&'static ModelEntry> {
    models_for(backend).find(|entry| entry.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn models_for_returns_only_the_requested_backends_entries() {
        for backend in CatalogBackend::ALL {
            let entries: Vec<_> = models_for(backend).collect();
            assert!(
                !entries.is_empty(),
                "{backend:?} must offer at least one model"
            );
            for entry in entries {
                assert_eq!(
                    entry.backend, backend,
                    "models_for({backend:?}) must never leak {} from another backend",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn the_per_backend_lists_partition_the_whole_catalog() {
        let total: usize = CatalogBackend::ALL
            .iter()
            .map(|backend| models_for(*backend).count())
            .sum();
        assert_eq!(
            total,
            all().len(),
            "every entry belongs to exactly one listed backend"
        );
    }

    #[test]
    fn every_id_is_non_empty_and_huggingface_ids_are_org_slash_model() {
        for entry in all() {
            assert!(!entry.id.is_empty(), "{entry:?} has an empty id");
            assert!(
                !entry.display_name.is_empty(),
                "{} has an empty display name",
                entry.id
            );
            if entry.backend == CatalogBackend::Huggingface {
                assert!(
                    entry.id.contains('/'),
                    "{} is not the Org/Model shape Backend::huggingface expects",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn gemini_ids_are_bare_model_names_with_no_org_prefix() {
        for entry in models_for(CatalogBackend::Gemini) {
            assert!(
                !entry.id.contains('/'),
                "{} would not be accepted as a Gemini model id",
                entry.id
            );
        }
    }

    #[test]
    fn ids_are_unique_within_a_backend() {
        for backend in CatalogBackend::ALL {
            let mut seen = HashSet::new();
            for entry in models_for(backend) {
                assert!(
                    seen.insert(entry.id),
                    "{} appears twice for {backend:?}",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn find_is_scoped_to_the_given_backend() {
        let hf = models_for(CatalogBackend::Huggingface)
            .next()
            .expect("catalog lists at least one Hugging Face model");

        assert_eq!(
            find(CatalogBackend::Huggingface, hf.id),
            Some(hf),
            "an id is found on its own backend"
        );
        assert_eq!(
            find(CatalogBackend::Gemini, hf.id),
            None,
            "the same id is not found on the other backend"
        );
    }

    #[test]
    fn a_priced_entry_carries_a_non_negative_rate() {
        for entry in all() {
            if let Some(rate) = entry.pricing {
                assert!(
                    rate.input_per_million >= 0.0 && rate.output_per_million >= 0.0,
                    "{} has a negative price: {rate:?}",
                    entry.id
                );
            }
        }
    }
}
