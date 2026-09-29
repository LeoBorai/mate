use std::path::{Path, PathBuf};

use crate::domain::{DocKind, SpecId};

#[derive(Debug, thiserror::Error)]
#[error("path escapes sandbox root")]
pub struct OutsideSandbox;

/// One spec source present under the served root.
#[derive(Debug, Clone)]
pub struct SourceDir {
    pub kind: DocKind,
    /// `<root>/<kind.dir_name()>` as seen from the root — what gets walked
    /// and watched, so every path found under it strips cleanly back to a
    /// root-relative `SpecId`.
    pub path: PathBuf,
    /// `path`, canonicalized — the actual containment boundary.
    canonical: PathBuf,
}

/// The only directories `specviz` is allowed to read from: the source
/// directories (`specs/`, `openspec/`) present under one served root. Every
/// path handed to the client (a `SpecId`) is resolved back to a real path
/// through here — nothing else ever touches the filesystem with a path that
/// didn't come from this guard.
pub struct SandboxedRoot {
    /// Canonicalized served root.
    root: PathBuf,
    sources: Vec<SourceDir>,
}

impl SandboxedRoot {
    /// Canonicalizes `dir` once and records which sources exist under it.
    /// Sources are detected here only: a `specs/` or `openspec/` created
    /// later is picked up on the next start, not live.
    pub fn new(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = dir.as_ref().canonicalize()?;
        let mut sources = Vec::new();
        for kind in [DocKind::Plain, DocKind::OpenSpec] {
            let path = root.join(kind.dir_name());
            if path.is_dir() {
                let canonical = path.canonicalize()?;
                sources.push(SourceDir {
                    kind,
                    path,
                    canonical,
                });
            }
        }
        Ok(Self { root, sources })
    }

    /// The canonicalized served root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every source present at startup, `specs/` before `openspec/`.
    pub fn sources(&self) -> &[SourceDir] {
        &self.sources
    }

    /// Resolves a `SpecId` to a canonical path and the source it lives in,
    /// rejecting anything that isn't a `.md` file inside a source directory
    /// after `..` and symlink resolution.
    pub fn resolve(&self, id: &SpecId) -> Result<(DocKind, PathBuf), OutsideSandbox> {
        let candidate = self.root.join(id.as_str());
        if candidate.extension().is_none_or(|ext| ext != "md") {
            return Err(OutsideSandbox);
        }
        let canonical = candidate.canonicalize().map_err(|_| OutsideSandbox)?;
        if !canonical.is_file() {
            return Err(OutsideSandbox);
        }
        self.sources
            .iter()
            .find(|source| canonical.starts_with(&source.canonical))
            .map(|source| (source.kind, canonical.clone()))
            .ok_or(OutsideSandbox)
    }

    /// The root-relative `SpecId` for a path found while walking or watching
    /// a source, or `None` if it isn't under the root.
    pub fn id_for(&self, path: &Path) -> Option<SpecId> {
        let relative = path.strip_prefix(&self.root).ok()?;
        Some(SpecId::new(
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("specs")).unwrap();
        std::fs::write(tmp.path().join("secret.md"), "nope").unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        assert!(
            sandbox.resolve(&SpecId::new("specs/../secret.md")).is_err(),
            "a `..` that lands outside every source is refused"
        );
    }

    #[test]
    fn rejects_a_file_outside_every_source() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("specs")).unwrap();
        std::fs::write(tmp.path().join("README.md"), "# root").unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        assert!(
            sandbox.resolve(&SpecId::new("README.md")).is_err(),
            "the served root itself is not a source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_escaping_every_source() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("specs")).unwrap();
        std::fs::write(outside.path().join("secret.md"), "nope").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            tmp.path().join("specs/link.md"),
        )
        .unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        assert!(
            sandbox.resolve(&SpecId::new("specs/link.md")).is_err(),
            "a symlink's target, not its location, decides containment"
        );
    }

    #[test]
    fn rejects_non_markdown_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("openspec")).unwrap();
        std::fs::write(tmp.path().join("openspec/config.yaml"), "schema: x").unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        assert!(
            sandbox.resolve(&SpecId::new("openspec/config.yaml")).is_err(),
            "only Markdown is served"
        );
    }

    #[test]
    fn resolves_files_in_each_source_with_their_kind() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("specs")).unwrap();
        std::fs::create_dir_all(tmp.path().join("openspec/specs/overview")).unwrap();
        std::fs::write(tmp.path().join("specs/overview.md"), "# A").unwrap();
        std::fs::write(tmp.path().join("openspec/specs/overview/spec.md"), "# B").unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        let (plain, _) = sandbox.resolve(&SpecId::new("specs/overview.md")).unwrap();
        let (openspec, _) = sandbox
            .resolve(&SpecId::new("openspec/specs/overview/spec.md"))
            .unwrap();
        assert_eq!(plain, DocKind::Plain, "a file under specs/ is a plain doc");
        assert_eq!(
            openspec,
            DocKind::OpenSpec,
            "a file under openspec/ is an OpenSpec doc"
        );
    }

    #[test]
    fn detects_only_the_sources_that_exist() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("openspec")).unwrap();

        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();

        let kinds: Vec<_> = sandbox.sources().iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![DocKind::OpenSpec], "no specs/ dir, so no plain source");
    }
}
