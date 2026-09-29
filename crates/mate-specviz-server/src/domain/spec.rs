use std::fmt;
use std::path::PathBuf;

/// Slash-joined path of a spec relative to the served root, e.g.
/// `"specs/auth/login.md"` or `"openspec/changes/add-x/tasks.md"`. Root-relative
/// (not source-relative) so documents from different sources never collide.
/// The wire format the client uses to address a spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecId(String);

impl SpecId {
    pub fn new(relative_path: impl Into<String>) -> Self {
        Self(relative_path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpecId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct SpecMeta {
    pub id: SpecId,
    /// First `# H1` in the file, or the filename if none is present.
    pub title: String,
    pub relative_path: PathBuf,
}

/// Which source a document came from — decides whether OpenSpec-aware
/// rendering applies. A `## ADDED Requirements` heading only means something
/// inside `openspec/`; in a plain `specs/` doc it's just a heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// `<root>/specs/`.
    Plain,
    /// `<root>/openspec/`.
    OpenSpec,
}

impl DocKind {
    /// The directory under the served root this source lives in.
    pub fn dir_name(self) -> &'static str {
        match self {
            DocKind::Plain => "specs",
            DocKind::OpenSpec => "openspec",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub meta: SpecMeta,
    pub kind: DocKind,
    pub raw_markdown: String,
}
