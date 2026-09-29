use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::openspec::{artifact_rank, requirement_count, task_progress};
use crate::domain::{DocKind, SectionKind, Spec, SpecId, SpecMeta, SpecTree, SpecTreeNode};
use crate::infra::sandbox::SandboxedRoot;

#[derive(Debug, thiserror::Error)]
pub enum SpecRepositoryError {
    #[error("spec not found: {0}")]
    NotFound(SpecId),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait SpecRepository: Send + Sync {
    async fn walk_specs(&self) -> Result<SpecTree, SpecRepositoryError>;
    async fn read_spec(&self, id: &SpecId) -> Result<Spec, SpecRepositoryError>;
}

pub struct FsSpecRepository {
    sandbox: Arc<SandboxedRoot>,
}

impl FsSpecRepository {
    pub fn new(sandbox: Arc<SandboxedRoot>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl SpecRepository for FsSpecRepository {
    /// One walk per present source, merged in source order: the plain
    /// section, then the three OpenSpec sections.
    async fn walk_specs(&self) -> Result<SpecTree, SpecRepositoryError> {
        let sandbox = self.sandbox.clone();
        let nodes = tokio::task::spawn_blocking(move || {
            let mut nodes = Vec::new();
            for source in sandbox.sources() {
                match source.kind {
                    DocKind::Plain => nodes.push(SpecTreeNode::Section {
                        kind: SectionKind::Plain,
                        children: walk_dir(&sandbox, &source.path)?,
                    }),
                    DocKind::OpenSpec => nodes.extend(walk_openspec(&sandbox, &source.path)?),
                }
            }
            Ok::<_, SpecRepositoryError>(nodes)
        })
        .await
        .expect("walk_specs blocking task panicked")?;

        Ok(SpecTree { nodes })
    }

    async fn read_spec(&self, id: &SpecId) -> Result<Spec, SpecRepositoryError> {
        let (kind, path) = self
            .sandbox
            .resolve(id)
            .map_err(|_| SpecRepositoryError::NotFound(id.clone()))?;

        let raw_markdown = tokio::fs::read_to_string(&path).await?;
        let title = derive_title(&raw_markdown, id);

        Ok(Spec {
            meta: SpecMeta {
                id: id.clone(),
                title,
                relative_path: PathBuf::from(id.as_str()),
            },
            kind,
            raw_markdown,
        })
    }
}

/// Plain-source walk: mirrors the directory layout, directories first, both
/// sorted, empty directories dropped. Symlinks are skipped (`DirEntry`'s
/// file type doesn't follow them), so nothing outside the source is listed.
fn walk_dir(sandbox: &SandboxedRoot, dir: &Path) -> Result<Vec<SpecTreeNode>, SpecRepositoryError> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            dirs.push(path);
        } else if file_type.is_file() && is_markdown(&path) {
            files.push(path);
        }
    }

    dirs.sort();
    files.sort();

    let mut nodes = Vec::with_capacity(dirs.len() + files.len());

    for dir_path in dirs {
        let children = walk_dir(sandbox, &dir_path)?;
        if children.is_empty() {
            continue;
        }
        nodes.push(SpecTreeNode::Dir {
            name: file_name(&dir_path),
            children,
        });
    }

    for file_path in files {
        let (meta, _) = read_meta(sandbox, &file_path)?;
        nodes.push(SpecTreeNode::File(meta));
    }

    Ok(nodes)
}

/// OpenSpec-source walk: always yields the Capabilities, Changes, and
/// Archive sections (possibly empty), in that order. Archive entries are
/// newest first — their names are date-prefixed (`2026-01-01-add-x`), so
/// that's a reverse sort.
fn walk_openspec(
    sandbox: &SandboxedRoot,
    openspec: &Path,
) -> Result<[SpecTreeNode; 3], SpecRepositoryError> {
    let capabilities = walk_capabilities(sandbox, &openspec.join("specs"))?;

    let changes_dir = openspec.join("changes");
    let mut changes = Vec::new();
    for (name, path) in subdirs(&changes_dir)? {
        if name == "archive" {
            continue;
        }
        changes.push(change_node(sandbox, name, &path)?);
    }

    let mut archive = Vec::new();
    let mut archived = subdirs(&changes_dir.join("archive"))?;
    archived.reverse();
    for (name, path) in archived {
        archive.push(change_node(sandbox, name, &path)?);
    }

    Ok([
        SpecTreeNode::Section {
            kind: SectionKind::Capabilities,
            children: capabilities,
        },
        SpecTreeNode::Section {
            kind: SectionKind::Changes,
            children: changes,
        },
        SpecTreeNode::Section {
            kind: SectionKind::Archive,
            children: archive,
        },
    ])
}

/// Every `spec.md` under `openspec/specs/`, at any depth, as a capability
/// named by its directory's path relative to `openspec/specs/`.
fn walk_capabilities(
    sandbox: &SandboxedRoot,
    specs_dir: &Path,
) -> Result<Vec<SpecTreeNode>, SpecRepositoryError> {
    let mut nodes = Vec::new();
    for file in markdown_files(specs_dir)? {
        if file.file_name().is_none_or(|n| n != "spec.md") {
            continue;
        }
        let Some(parent) = file.parent() else {
            continue;
        };
        let path = slash_path(parent.strip_prefix(specs_dir).unwrap_or(parent));
        if path.is_empty() {
            continue;
        }
        let (meta, raw) = read_meta(sandbox, &file)?;
        nodes.push(SpecTreeNode::Capability {
            path,
            meta,
            requirements: requirement_count(&raw),
        });
    }
    Ok(nodes)
}

/// One change directory: its Markdown files in artifact order, labelled by
/// their path inside the change (every delta spec's H1 is the same
/// boilerplate, so the H1 would make them indistinguishable), plus task
/// progress from its top-level `tasks.md`.
fn change_node(
    sandbox: &SandboxedRoot,
    name: String,
    dir: &Path,
) -> Result<SpecTreeNode, SpecRepositoryError> {
    let mut files: Vec<(String, PathBuf)> = markdown_files(dir)?
        .into_iter()
        .map(|path| (slash_path(path.strip_prefix(dir).unwrap_or(&path)), path))
        .collect();
    files.sort_by(|(a, _), (b, _)| (artifact_rank(a), a).cmp(&(artifact_rank(b), b)));

    let mut progress = None;
    let mut children = Vec::with_capacity(files.len());
    for (relative, path) in files {
        let (mut meta, raw) = read_meta(sandbox, &path)?;
        if relative == "tasks.md" {
            progress = task_progress(&raw);
        }
        meta.title = artifact_label(&relative);
        children.push(SpecTreeNode::File(meta));
    }

    Ok(SpecTreeNode::Change {
        name,
        progress,
        children,
    })
}

/// `specs/foo/spec.md` -> `specs/foo`; anything else loses its `.md`.
fn artifact_label(relative: &str) -> String {
    relative
        .strip_suffix("/spec.md")
        .or_else(|| relative.strip_suffix(".md"))
        .unwrap_or(relative)
        .to_owned()
}

/// Reads one file found by a walk into its `SpecMeta` plus raw contents.
fn read_meta(sandbox: &SandboxedRoot, path: &Path) -> Result<(SpecMeta, String), SpecRepositoryError> {
    let id = sandbox
        .id_for(path)
        .ok_or_else(|| SpecRepositoryError::NotFound(SpecId::new(path.to_string_lossy())))?;
    let raw = std::fs::read_to_string(path)?;
    let title = derive_title(&raw, &id);
    Ok((
        SpecMeta {
            relative_path: PathBuf::from(id.as_str()),
            id,
            title,
        },
        raw,
    ))
}

/// Direct subdirectories of `dir` as `(name, path)`, sorted by name; empty
/// if `dir` doesn't exist.
fn subdirs(dir: &Path) -> Result<Vec<(String, PathBuf)>, SpecRepositoryError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// Every Markdown file under `dir`, recursively, in no particular order;
/// empty if `dir` doesn't exist. Symlinks are skipped.
fn markdown_files(dir: &Path) -> Result<Vec<PathBuf>, SpecRepositoryError> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && is_markdown(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "md")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn derive_title(markdown: &str, id: &SpecId) -> String {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            Path::new(id.as_str())
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.as_str().to_owned())
        })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    async fn tree_json(root: &Path) -> Value {
        let sandbox = Arc::new(SandboxedRoot::new(root).unwrap());
        let tree = FsSpecRepository::new(sandbox).walk_specs().await.unwrap();
        serde_json::to_value(tree).unwrap()
    }

    #[tokio::test]
    async fn walks_nested_plain_specs_and_derives_titles() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "specs/root.md", "# Root Spec\n");
        write(tmp.path(), "specs/auth/login.md", "no heading here");

        let sandbox = Arc::new(SandboxedRoot::new(tmp.path()).unwrap());
        let repo = FsSpecRepository::new(sandbox);

        let tree = serde_json::to_value(repo.walk_specs().await.unwrap()).unwrap();
        assert_eq!(
            tree["nodes"][0]["kind"], "plain",
            "a specs/ dir yields one plain section"
        );
        assert_eq!(
            tree["nodes"][0]["children"].as_array().unwrap().len(),
            2,
            "one dir (`auth`) plus one file (`root.md`)"
        );

        let spec = repo
            .read_spec(&SpecId::new("specs/auth/login.md"))
            .await
            .unwrap();
        assert_eq!(spec.meta.title, "login", "no H1, so the file stem is the title");
        assert_eq!(spec.kind, DocKind::Plain, "specs/ files are plain docs");
    }

    #[tokio::test]
    async fn only_plain_specs_yields_no_openspec_sections() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "specs/a.md", "# A\n");

        let tree = tree_json(tmp.path()).await;
        assert_eq!(
            tree["nodes"].as_array().unwrap().len(),
            1,
            "only the plain section, no Capabilities/Changes/Archive"
        );
    }

    #[tokio::test]
    async fn both_sources_yield_all_four_sections_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "specs/a.md", "# A\n");
        std::fs::create_dir_all(tmp.path().join("openspec")).unwrap();

        let tree = tree_json(tmp.path()).await;
        let kinds: Vec<_> = tree["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["kind"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            kinds,
            vec!["plain", "capabilities", "changes", "archive"],
            "plain first, then the three OpenSpec sections, empty ones included"
        );
    }

    #[tokio::test]
    async fn neither_source_yields_an_empty_tree() {
        let tmp = tempfile::tempdir().unwrap();

        let tree = tree_json(tmp.path()).await;
        assert_eq!(tree, json!({"nodes": []}), "no sources, empty tree, no error");
    }

    #[tokio::test]
    async fn splits_active_and_archived_changes() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "openspec/changes/add-x/proposal.md", "# P\n");
        write(tmp.path(), "openspec/changes/add-y/proposal.md", "# P\n");
        write(
            tmp.path(),
            "openspec/changes/archive/2026-01-01-add-z/proposal.md",
            "# P\n",
        );

        let tree = tree_json(tmp.path()).await;
        let names = |section: &Value| -> Vec<String> {
            section["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            names(&tree["nodes"][1]),
            vec!["add-x", "add-y"],
            "archive/ is not an active change"
        );
        assert_eq!(
            names(&tree["nodes"][2]),
            vec!["2026-01-01-add-z"],
            "archived changes land in the Archive section"
        );
    }

    #[tokio::test]
    async fn orders_change_artifacts_and_reads_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let change = "openspec/changes/add-x";
        write(tmp.path(), &format!("{change}/tasks.md"), "- [x] a\n- [ ] b\n");
        write(tmp.path(), &format!("{change}/design.md"), "# D\n");
        write(tmp.path(), &format!("{change}/proposal.md"), "# P\n");
        write(tmp.path(), &format!("{change}/specs/foo/spec.md"), "# Spec Delta\n");
        write(tmp.path(), &format!("{change}/research.md"), "# R\n");

        let tree = tree_json(tmp.path()).await;
        let change = &tree["nodes"][1]["children"][0];
        let titles: Vec<_> = change["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["title"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            titles,
            vec!["proposal", "specs/foo", "design", "tasks", "research"],
            "standard artifacts in order, extras after"
        );
        assert_eq!(
            change["progress"],
            json!({"done": 1, "total": 2}),
            "progress comes from tasks.md"
        );
    }

    #[tokio::test]
    async fn a_change_without_tasks_has_no_progress() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "openspec/changes/add-x/proposal.md", "# P\n");

        let tree = tree_json(tmp.path()).await;
        assert_eq!(
            tree["nodes"][1]["children"][0]["progress"],
            Value::Null,
            "no tasks.md, no badge"
        );
    }

    #[tokio::test]
    async fn lists_nested_capabilities_with_requirement_counts() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "openspec/specs/identity/user-auth/spec.md",
            "# User auth\n\n### Requirement: A\n\n### Requirement: B\n",
        );

        let tree = tree_json(tmp.path()).await;
        let capability = &tree["nodes"][0]["children"][0];
        assert_eq!(
            capability["path"], "identity/user-auth",
            "the capability path is the spec's dir relative to openspec/specs/"
        );
        assert_eq!(capability["requirements"], 2, "two `### Requirement:` headings");
        assert_eq!(
            capability["id"], "openspec/specs/identity/user-auth/spec.md",
            "ids are root-relative"
        );
    }

    #[tokio::test]
    async fn empty_capabilities_section_is_still_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("openspec/specs")).unwrap();

        let tree = tree_json(tmp.path()).await;
        assert_eq!(
            tree["nodes"][0],
            json!({"type": "section", "kind": "capabilities", "children": []}),
            "an OpenSpec source always shows its Capabilities section"
        );
    }

    #[tokio::test]
    async fn same_file_name_in_both_sources_gets_distinct_ids() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "specs/overview.md", "# Plain\n");
        write(tmp.path(), "openspec/specs/overview/spec.md", "# OpenSpec\n");

        let sandbox = Arc::new(SandboxedRoot::new(tmp.path()).unwrap());
        let repo = FsSpecRepository::new(sandbox);

        let plain = repo.read_spec(&SpecId::new("specs/overview.md")).await.unwrap();
        let openspec = repo
            .read_spec(&SpecId::new("openspec/specs/overview/spec.md"))
            .await
            .unwrap();
        assert_eq!(plain.meta.title, "Plain", "the plain doc resolves to itself");
        assert_eq!(
            openspec.meta.title, "OpenSpec",
            "the OpenSpec doc resolves to itself"
        );
        assert_eq!(openspec.kind, DocKind::OpenSpec, "and carries the OpenSpec kind");
    }
}
