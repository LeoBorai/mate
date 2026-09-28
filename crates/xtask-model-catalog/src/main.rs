//! Regenerates `crates/mate-core/src/model_catalog/generated.rs` from a pinned commit of
//! `anomalyco/models.dev`. Run it with `just gen-model-catalog`; it is a dev-time tool only and
//! is never invoked by `cargo build`, so building mate needs no network access.
//!
//! One HTTPS request downloads the pinned commit's tarball (via `curl`); every file is then read
//! straight out of the archive with `tar`, so nothing is unpacked onto disk. For each model file
//! under `providers/huggingface/models/**` and `providers/google/models/**` the provider's own
//! fields (`cost`, and anything it overrides) are layered over the shared metadata in
//! `models/<base_model>.toml` (display name, modalities, tool support).
//!
//! Only models an agent can actually use are kept: they must support tool calls, produce plain
//! text, and not be deprecated.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use toml::{Table, Value};

/// The `anomalyco/models.dev` commit the catalog is generated from. A fixed commit — not the
/// moving `dev` branch — keeps reruns deterministic and the resulting diff reviewable. Bump it
/// deliberately, then rerun `just gen-model-catalog`.
const PINNED_REF: &str = "552ba9d996a628e38a721ff68e9a5c7f7e5fcfd9";

const ARCHIVE_URL: &str = "https://codeload.github.com/anomalyco/models.dev/tar.gz";

/// Where the generated file lands, relative to this crate's manifest directory.
const OUTPUT: &str = "../mate-core/src/model_catalog/generated.rs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Backend {
    Huggingface,
    Gemini,
}

impl Backend {
    const ALL: [Backend; 2] = [Backend::Huggingface, Backend::Gemini];

    /// Directory under `providers/` whose model files feed this backend.
    fn provider_dir(self) -> &'static str {
        match self {
            Self::Huggingface => "huggingface",
            Self::Gemini => "google",
        }
    }

    /// The matching `mate_core::model_catalog::CatalogBackend` variant name.
    fn variant(self) -> &'static str {
        match self {
            Self::Huggingface => "Huggingface",
            Self::Gemini => "Gemini",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    backend: Backend,
    id: String,
    display_name: String,
    /// `(input, output)` USD per million tokens.
    pricing: Option<(f64, f64)>,
}

/// A downloaded models.dev tarball on disk, deleted on drop.
struct Archive {
    path: PathBuf,
    /// The archive's single top-level directory (`models.dev-<sha>`).
    root: String,
    members: BTreeSet<String>,
}

impl Archive {
    fn download() -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "mate-models-dev-{PINNED_REF}-{}.tar.gz",
            std::process::id()
        ));
        let status = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--retry", "3", "--output"])
            .arg(&path)
            .arg(format!("{ARCHIVE_URL}/{PINNED_REF}"))
            .status()
            .context("running `curl` (is it installed?)")?;
        if !status.success() {
            bail!("downloading models.dev@{PINNED_REF} failed: {status}");
        }
        let archive = Self {
            path,
            root: String::new(),
            members: BTreeSet::new(),
        };
        archive.index()
    }

    fn index(mut self) -> anyhow::Result<Self> {
        let output = Command::new("tar")
            .arg("-tzf")
            .arg(&self.path)
            .stderr(Stdio::inherit())
            .output()
            .context("running `tar` (is it installed?)")?;
        if !output.status.success() {
            bail!("listing the models.dev archive failed: {}", output.status);
        }
        let listing = String::from_utf8(output.stdout).context("archive listing is not UTF-8")?;
        for line in listing.lines() {
            let Some((root, rest)) = line.split_once('/') else {
                continue;
            };
            if self.root.is_empty() {
                self.root = root.to_string();
            }
            if !rest.is_empty() && !rest.ends_with('/') {
                self.members.insert(rest.to_string());
            }
        }
        if self.root.is_empty() {
            bail!("the models.dev archive is empty");
        }
        Ok(self)
    }

    /// Model files under `providers/<dir>/models/`, as archive-relative paths.
    fn provider_models(&self, dir: &str) -> Vec<String> {
        let prefix = format!("providers/{dir}/models/");
        self.members
            .iter()
            .filter(|m| m.starts_with(&prefix) && m.ends_with(".toml"))
            .cloned()
            .collect()
    }

    fn read_toml(&self, member: &str) -> anyhow::Result<Option<Table>> {
        if !self.members.contains(member) {
            return Ok(None);
        }
        let output = Command::new("tar")
            .arg("-xzOf")
            .arg(&self.path)
            .arg(format!("{}/{member}", self.root))
            .stderr(Stdio::inherit())
            .output()
            .context("running `tar`")?;
        if !output.status.success() {
            bail!(
                "reading {member} from the archive failed: {}",
                output.status
            );
        }
        let text =
            String::from_utf8(output.stdout).with_context(|| format!("{member} is not UTF-8"))?;
        let table = text
            .parse::<Table>()
            .with_context(|| format!("parsing {member}"))?;
        Ok(Some(table))
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `path` looked up in the provider's own file first, then in the shared base model's.
fn lookup<'a>(provider: &'a Table, base: Option<&'a Table>, path: &[&str]) -> Option<&'a Value> {
    fn descend<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Value> {
        let (first, rest) = path.split_first()?;
        let value = table.get(*first)?;
        if rest.is_empty() {
            return Some(value);
        }
        descend(value.as_table()?, rest)
    }
    descend(provider, path).or_else(|| base.and_then(|b| descend(b, path)))
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Float(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

/// Builds one catalog entry, or `None` if the model isn't usable by an agent: no tool-call
/// support, non-text output, or a `deprecated` status. Output modalities that a file doesn't
/// declare at all are given the benefit of the doubt.
fn build_entry(
    backend: Backend,
    id: &str,
    provider: &Table,
    base: Option<&Table>,
) -> Option<Entry> {
    let field = |path: &[&str]| lookup(provider, base, path);

    if field(&["tool_call"]).and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if field(&["status"]).and_then(Value::as_str) == Some("deprecated") {
        return None;
    }
    let output_is_text = field(&["modalities", "output"])
        .and_then(Value::as_array)
        .is_none_or(|kinds| kinds.iter().all(|kind| kind.as_str() == Some("text")));
    if !output_is_text {
        return None;
    }

    let display_name = field(&["name"])
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let pricing = number(field(&["cost", "input"])).zip(number(field(&["cost", "output"])));

    Some(Entry {
        backend,
        id: id.to_string(),
        display_name,
        pricing,
    })
}

fn collect(archive: &Archive) -> anyhow::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for backend in Backend::ALL {
        let dir = backend.provider_dir();
        let prefix = format!("providers/{dir}/models/");
        for member in archive.provider_models(dir) {
            let id = member
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".toml"))
                .with_context(|| format!("unexpected model path {member}"))?;
            let provider = archive
                .read_toml(&member)?
                .with_context(|| format!("{member} vanished from the archive"))?;
            let base = match provider.get("base_model").and_then(Value::as_str) {
                Some(base_model) => archive.read_toml(&format!("models/{base_model}.toml"))?,
                None => None,
            };
            entries.extend(build_entry(backend, id, &provider, base.as_ref()));
        }
    }
    entries.sort_by(|a, b| (a.backend, &a.id).cmp(&(b.backend, &b.id)));
    Ok(entries)
}

/// Renders the `generated.rs` source. Kept `rustfmt`-stable by construction and additionally
/// marked `#[rustfmt::skip]`, so `cargo fmt --check` never disagrees with a fresh regeneration.
fn render(entries: &[Entry]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "// @generated by `just gen-model-catalog` (crates/xtask-model-catalog) from\n\
         // anomalyco/models.dev@{PINNED_REF}. Do not edit by hand: rerun the recipe instead.\n\
         \n\
         use super::{{CatalogBackend, ModelEntry}};\n\
         use crate::cost::ModelRate;\n\
         \n\
         #[rustfmt::skip]\n\
         pub(super) const CATALOG: &[ModelEntry] = &[\n"
    ));
    for entry in entries {
        out.push_str("    ModelEntry {\n");
        out.push_str(&format!("        id: {:?},\n", entry.id));
        out.push_str(&format!(
            "        display_name: {:?},\n",
            entry.display_name
        ));
        out.push_str(&format!(
            "        backend: CatalogBackend::{},\n",
            entry.backend.variant()
        ));
        match entry.pricing {
            Some((input, output)) => out.push_str(&format!(
                "        pricing: Some(ModelRate {{\n            input_per_million: {input:?},\n            output_per_million: {output:?},\n        }}),\n"
            )),
            None => out.push_str("        pricing: None,\n"),
        }
        out.push_str("    },\n");
    }
    out.push_str("];\n");
    out
}

fn main() -> anyhow::Result<()> {
    let archive = Archive::download()?;
    let entries = collect(&archive)?;
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join(OUTPUT);
    std::fs::write(&output, render(&entries))
        .with_context(|| format!("writing {}", output.display()))?;
    println!(
        "wrote {} entries to {} (models.dev@{PINNED_REF})",
        entries.len(),
        output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().expect("test fixture is valid TOML")
    }

    #[test]
    fn provider_cost_is_layered_over_the_base_models_name() {
        let provider =
            table("base_model = \"alibaba/qwen3-32b\"\n[cost]\ninput = 0.29\noutput = 0.59");
        let base =
            table("name = \"Qwen3 32B\"\ntool_call = true\n[modalities]\noutput = [\"text\"]");

        let entry = build_entry(
            Backend::Huggingface,
            "Qwen/Qwen3-32B",
            &provider,
            Some(&base),
        )
        .expect("a tool-calling text model is kept");

        assert_eq!(
            entry.display_name, "Qwen3 32B",
            "name comes from the base model file"
        );
        assert_eq!(
            entry.pricing,
            Some((0.29, 0.59)),
            "cost comes from the provider file"
        );
    }

    #[test]
    fn a_provider_field_overrides_the_same_field_on_the_base_model() {
        let provider = table("name = \"Provider Name\"\ntool_call = true");
        let base = table("name = \"Base Name\"\ntool_call = false");

        let entry = build_entry(Backend::Gemini, "m", &provider, Some(&base))
            .expect("the provider's tool_call = true wins over the base's false");

        assert_eq!(
            entry.display_name, "Provider Name",
            "provider value shadows base value"
        );
    }

    #[test]
    fn integer_costs_become_floats() {
        let provider = table("tool_call = true\n[cost]\ninput = 2\noutput = 12");

        let entry = build_entry(Backend::Gemini, "m", &provider, None).unwrap();

        assert_eq!(
            entry.pricing,
            Some((2.0, 12.0)),
            "TOML integers widen to f64"
        );
    }

    #[test]
    fn a_model_with_no_cost_block_has_no_pricing() {
        let provider = table("tool_call = true");

        let entry = build_entry(Backend::Gemini, "m", &provider, None).unwrap();

        assert_eq!(entry.pricing, None, "no [cost] means unpriced, never zero");
    }

    #[test]
    fn a_missing_name_falls_back_to_the_id() {
        let provider = table("tool_call = true");

        let entry = build_entry(Backend::Gemini, "gemini-x", &provider, None).unwrap();

        assert_eq!(
            entry.display_name, "gemini-x",
            "id doubles as the display name"
        );
    }

    #[test]
    fn models_without_tool_call_support_are_dropped() {
        let embedding = table("tool_call = false");
        let undeclared = table("name = \"x\"");

        assert!(
            build_entry(Backend::Gemini, "e", &embedding, None).is_none(),
            "explicit false"
        );
        assert!(
            build_entry(Backend::Gemini, "u", &undeclared, None).is_none(),
            "absent means unsupported"
        );
    }

    #[test]
    fn non_text_output_and_deprecated_models_are_dropped() {
        let image = table("tool_call = true\n[modalities]\noutput = [\"text\", \"image\"]");
        let deprecated = table("tool_call = true\nstatus = \"deprecated\"");

        assert!(
            build_entry(Backend::Gemini, "i", &image, None).is_none(),
            "image output"
        );
        assert!(
            build_entry(Backend::Gemini, "d", &deprecated, None).is_none(),
            "deprecated"
        );
    }

    #[test]
    fn undeclared_output_modalities_are_given_the_benefit_of_the_doubt() {
        let provider = table("tool_call = true");

        assert!(
            build_entry(Backend::Gemini, "m", &provider, None).is_some(),
            "no modalities table at all is not evidence of non-text output"
        );
    }

    #[test]
    fn render_emits_a_const_slice_with_every_entry() {
        let entries = [
            Entry {
                backend: Backend::Huggingface,
                id: "org/model".to_string(),
                display_name: "Model".to_string(),
                pricing: Some((0.3, 1.2)),
            },
            Entry {
                backend: Backend::Gemini,
                id: "gemini-x".to_string(),
                display_name: "Gemini \"X\"".to_string(),
                pricing: None,
            },
        ];

        let source = render(&entries);

        assert!(
            source.contains("pub(super) const CATALOG: &[ModelEntry] = &["),
            "declares the const"
        );
        assert!(source.contains("id: \"org/model\","), "quotes the id");
        assert!(
            source.contains("backend: CatalogBackend::Huggingface,"),
            "maps the backend"
        );
        assert!(
            source.contains("input_per_million: 0.3,"),
            "prints the input rate"
        );
        assert!(
            source.contains("output_per_million: 1.2,"),
            "prints the output rate"
        );
        assert!(
            source.contains("display_name: \"Gemini \\\"X\\\"\","),
            "escapes quotes in names"
        );
        assert!(
            source.contains("pricing: None,"),
            "unpriced entries stay unpriced"
        );
        assert!(
            source.contains(PINNED_REF),
            "records which upstream commit it came from"
        );
    }

    #[test]
    fn whole_number_rates_keep_their_decimal_point() {
        let entries = [Entry {
            backend: Backend::Gemini,
            id: "m".to_string(),
            display_name: "m".to_string(),
            pricing: Some((2.0, 12.0)),
        }];

        let source = render(&entries);

        assert!(
            source.contains("input_per_million: 2.0,"),
            "an f64 literal needs its `.0`"
        );
    }
}
