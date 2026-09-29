//! Thin `fetch` wrappers over the server's JSON API.

use serde::Deserialize;

/// Which top-level sidebar group a [`SpecTreeNode::Section`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    Plain,
    Capabilities,
    Changes,
    Archive,
}

impl SectionKind {
    pub fn label(self) -> &'static str {
        match self {
            SectionKind::Plain => "Specs",
            SectionKind::Capabilities => "Capabilities",
            SectionKind::Changes => "Changes",
            SectionKind::Archive => "Archive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct TaskProgress {
    pub done: usize,
    pub total: usize,
}

/// Mirrors the server's `SpecTreeNode` wire shape: the top level is always
/// a list of `Section`s.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpecTreeNode {
    Section {
        kind: SectionKind,
        children: Vec<SpecTreeNode>,
    },
    Dir {
        name: String,
        children: Vec<SpecTreeNode>,
    },
    File {
        id: String,
        title: String,
    },
    Capability {
        path: String,
        id: String,
        title: String,
        requirements: usize,
    },
    Change {
        name: String,
        progress: Option<TaskProgress>,
        children: Vec<SpecTreeNode>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpecTree {
    pub nodes: Vec<SpecTreeNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenderedSpec {
    pub title: String,
    pub html: String,
    pub path: String,
}

/// Payload of a `spec-changed` SSE message from `/events`: either the
/// whole tree was re-walked, or one spec's cached render was invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpecChangedKind {
    Index,
    Spec { id: String },
}

pub async fn list_specs() -> Result<SpecTree, String> {
    request("/api/specs").await
}

pub async fn get_spec(id: &str) -> Result<RenderedSpec, String> {
    request(&format!("/api/specs/{id}")).await
}

async fn request<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, String> {
    gloo_net::http::Request::get(path)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<T>()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    fn deserializes_every_tree_node_variant() {
        let json = r#"{"nodes": [
            {"type": "section", "kind": "plain", "children": [
                {"type": "dir", "name": "auth", "children": [
                    {"type": "file", "id": "specs/auth/login.md", "title": "Login"}
                ]}
            ]},
            {"type": "section", "kind": "capabilities", "children": [
                {"type": "capability", "path": "identity/user-auth",
                 "id": "openspec/specs/identity/user-auth/spec.md", "title": "User auth", "requirements": 4}
            ]},
            {"type": "section", "kind": "changes", "children": [
                {"type": "change", "name": "add-x", "progress": {"done": 13, "total": 21}, "children": []},
                {"type": "change", "name": "add-y", "progress": null, "children": []}
            ]},
            {"type": "section", "kind": "archive", "children": []}
        ]}"#;

        let tree: SpecTree = serde_json::from_str(json).unwrap();

        assert_eq!(tree.nodes.len(), 4, "one node per section");
        assert_eq!(
            tree.nodes[2],
            SpecTreeNode::Section {
                kind: SectionKind::Changes,
                children: vec![
                    SpecTreeNode::Change {
                        name: "add-x".into(),
                        progress: Some(TaskProgress { done: 13, total: 21 }),
                        children: vec![],
                    },
                    SpecTreeNode::Change {
                        name: "add-y".into(),
                        progress: None,
                        children: vec![],
                    },
                ],
            },
            "change rows carry optional progress"
        );
        assert!(
            matches!(&tree.nodes[1], SpecTreeNode::Section { children, .. }
                if matches!(&children[0], SpecTreeNode::Capability { requirements: 4, .. })),
            "capability rows carry their requirement count"
        );
    }
}
