use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::openspec::TaskProgress;
use super::spec::SpecMeta;

/// Which top-level group of the sidebar a [`SpecTreeNode::Section`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// Everything under `<root>/specs/`, mirroring its directory layout.
    Plain,
    /// One entry per `openspec/specs/<capability-path>/spec.md`.
    Capabilities,
    /// One entry per active change under `openspec/changes/`.
    Changes,
    /// One entry per change under `openspec/changes/archive/`.
    Archive,
}

/// A node in the spec tree. The top level is always a list of `Section`s,
/// one per discovered source group.
///
/// Serializes to the client's wire contract, tagged by `type`:
/// `section {kind, children}`, `dir {name, children}`, `file {id, title}`,
/// `capability {path, id, title, requirements}`,
/// `change {name, progress: {done, total} | null, children}` — deliberately
/// narrower than `SpecMeta` (no `relative_path` leaks to the client).
#[derive(Debug, Clone)]
pub enum SpecTreeNode {
    Section {
        kind: SectionKind,
        children: Vec<SpecTreeNode>,
    },
    Dir {
        name: String,
        children: Vec<SpecTreeNode>,
    },
    File(SpecMeta),
    Capability {
        /// Path of the capability relative to `openspec/specs/`, e.g.
        /// `"identity/user-auth"`.
        path: String,
        meta: SpecMeta,
        requirements: usize,
    },
    Change {
        name: String,
        progress: Option<TaskProgress>,
        children: Vec<SpecTreeNode>,
    },
}

impl Serialize for SpecTreeNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SpecTreeNode::Section { kind, children } => {
                let mut s = serializer.serialize_struct("SpecTreeNode", 3)?;
                s.serialize_field("type", "section")?;
                s.serialize_field("kind", kind)?;
                s.serialize_field("children", children)?;
                s.end()
            }
            SpecTreeNode::Dir { name, children } => {
                let mut s = serializer.serialize_struct("SpecTreeNode", 3)?;
                s.serialize_field("type", "dir")?;
                s.serialize_field("name", name)?;
                s.serialize_field("children", children)?;
                s.end()
            }
            SpecTreeNode::File(meta) => {
                let mut s = serializer.serialize_struct("SpecTreeNode", 3)?;
                s.serialize_field("type", "file")?;
                s.serialize_field("id", meta.id.as_str())?;
                s.serialize_field("title", &meta.title)?;
                s.end()
            }
            SpecTreeNode::Capability {
                path,
                meta,
                requirements,
            } => {
                let mut s = serializer.serialize_struct("SpecTreeNode", 5)?;
                s.serialize_field("type", "capability")?;
                s.serialize_field("path", path)?;
                s.serialize_field("id", meta.id.as_str())?;
                s.serialize_field("title", &meta.title)?;
                s.serialize_field("requirements", requirements)?;
                s.end()
            }
            SpecTreeNode::Change {
                name,
                progress,
                children,
            } => {
                let mut s = serializer.serialize_struct("SpecTreeNode", 4)?;
                s.serialize_field("type", "change")?;
                s.serialize_field("name", name)?;
                s.serialize_field("progress", progress)?;
                s.serialize_field("children", children)?;
                s.end()
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SpecTree {
    pub nodes: Vec<SpecTreeNode>,
}

impl SpecTree {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::domain::SpecId;

    fn meta(id: &str, title: &str) -> SpecMeta {
        SpecMeta {
            id: SpecId::new(id),
            title: title.to_owned(),
            relative_path: PathBuf::from(id),
        }
    }

    #[test]
    fn serializes_every_variant_to_the_wire_shape() {
        let tree = SpecTree {
            nodes: vec![
                SpecTreeNode::Section {
                    kind: SectionKind::Plain,
                    children: vec![SpecTreeNode::Dir {
                        name: "auth".into(),
                        children: vec![SpecTreeNode::File(meta("specs/auth/login.md", "Login"))],
                    }],
                },
                SpecTreeNode::Section {
                    kind: SectionKind::Capabilities,
                    children: vec![SpecTreeNode::Capability {
                        path: "identity/user-auth".into(),
                        meta: meta("openspec/specs/identity/user-auth/spec.md", "User auth"),
                        requirements: 4,
                    }],
                },
                SpecTreeNode::Section {
                    kind: SectionKind::Changes,
                    children: vec![
                        SpecTreeNode::Change {
                            name: "add-x".into(),
                            progress: Some(TaskProgress { done: 13, total: 21 }),
                            children: vec![SpecTreeNode::File(meta(
                                "openspec/changes/add-x/proposal.md",
                                "proposal",
                            ))],
                        },
                        SpecTreeNode::Change {
                            name: "add-y".into(),
                            progress: None,
                            children: vec![],
                        },
                    ],
                },
                SpecTreeNode::Section {
                    kind: SectionKind::Archive,
                    children: vec![],
                },
            ],
        };

        assert_eq!(
            serde_json::to_value(&tree).unwrap(),
            json!({
                "nodes": [
                    {"type": "section", "kind": "plain", "children": [
                        {"type": "dir", "name": "auth", "children": [
                            {"type": "file", "id": "specs/auth/login.md", "title": "Login"}
                        ]}
                    ]},
                    {"type": "section", "kind": "capabilities", "children": [
                        {"type": "capability", "path": "identity/user-auth",
                         "id": "openspec/specs/identity/user-auth/spec.md",
                         "title": "User auth", "requirements": 4}
                    ]},
                    {"type": "section", "kind": "changes", "children": [
                        {"type": "change", "name": "add-x",
                         "progress": {"done": 13, "total": 21},
                         "children": [
                            {"type": "file", "id": "openspec/changes/add-x/proposal.md", "title": "proposal"}
                         ]},
                        {"type": "change", "name": "add-y", "progress": null, "children": []}
                    ]},
                    {"type": "section", "kind": "archive", "children": []}
                ]
            }),
            "the client's api.rs deserializes exactly this shape"
        );
    }
}
