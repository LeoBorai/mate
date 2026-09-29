//! Wires the domain/application/infra layers into a running app: sandboxes
//! the served root, builds the initial spec index, starts the filesystem
//! watcher, and returns the axum `Router`. Used by [`crate::viewer`] and by
//! integration tests, so both exercise identical wiring.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use notify::EventKind;
use notify::event::ModifyKind;

use crate::application::commands::{InvalidateSpec, RefreshSpecIndex};
use crate::application::index::SpecIndex;
use crate::application::{CommandBus, QueryBus};
use crate::domain::{DocKind, SpecId};
use crate::infra::markdown::PulldownRenderer;
use crate::infra::sandbox::SandboxedRoot;
use crate::infra::spec_repository::FsSpecRepository;
use crate::infra::watcher::{self, SpecFsEvent, SpecWatcher};
use crate::viewer::ViewerError;
use crate::web::routes::router;
use crate::web::state::AppState;

/// A running app's axum router plus the handles that keep it alive. The
/// filesystem watcher must not be dropped or the watch stops — hold this
/// struct for as long as the router is served.
pub struct App {
    pub router: Router,
    pub commands: Arc<CommandBus>,
    _watcher: SpecWatcher,
}

/// Builds the full app for `root`: sandboxes it, builds the initial spec
/// index, and starts watching every present source directory.
pub async fn build(root: &Path) -> Result<App, ViewerError> {
    let sandbox = Arc::new(SandboxedRoot::new(root).map_err(|source| ViewerError::Root {
        path: root.to_path_buf(),
        source,
    })?);
    let repo = Arc::new(FsSpecRepository::new(sandbox.clone()));
    let renderer = Arc::new(PulldownRenderer::new());
    let index = Arc::new(SpecIndex::empty());

    let commands = Arc::new(CommandBus::new(repo.clone(), index.clone()));
    let queries = Arc::new(QueryBus::new(repo, index, renderer));

    refresh_index(&commands).await;

    let dirs: Vec<&Path> = sandbox.sources().iter().map(|s| s.path.as_path()).collect();
    let (watcher, mut fs_events) = watcher::watch(&dirs)?;

    {
        let commands = commands.clone();
        let sandbox = sandbox.clone();
        tokio::spawn(async move {
            while let Some(event) = fs_events.recv().await {
                apply_fs_event(&commands, &sandbox, event).await;
            }
        });
    }

    let router = router(AppState {
        commands: commands.clone(),
        queries,
    });

    Ok(App {
        router,
        commands,
        _watcher: watcher,
    })
}

/// Rebuilds the spec index, logging (not propagating) a failure: a spec
/// that can't be read shouldn't take the viewer down, and nothing here may
/// write to stdout/stderr while the TUI owns the terminal.
async fn refresh_index(commands: &CommandBus) {
    if let Err(err) = commands.refresh_spec_index(RefreshSpecIndex).await {
        tracing::warn!(error = %err, "specviz: failed to index specs");
    }
}

/// What a raw filesystem event should do to the index.
#[derive(Debug, PartialEq, Eq)]
enum FsAction {
    /// Re-walk everything — the tree's shape or a derived count may change.
    Refresh,
    /// Only this document's rendered content changed.
    Invalidate(SpecId),
}

/// A content-only modification of a single still-existing document gets the
/// cheap `InvalidateSpec` — unless it's an OpenSpec `tasks.md` or `spec.md`,
/// whose contents feed the tree's progress badges and requirement counts.
/// Anything else (create/remove/rename, or a batch of paths) re-walks, since
/// it may reshape the tree.
fn classify_fs_event(sandbox: &SandboxedRoot, event: &SpecFsEvent) -> FsAction {
    let EventKind::Modify(ModifyKind::Data(_)) = event.kind else {
        return FsAction::Refresh;
    };
    let [path] = event.paths.as_slice() else {
        return FsAction::Refresh;
    };
    let Some(id) = sandbox.id_for(path) else {
        return FsAction::Refresh;
    };
    let Ok((kind, _)) = sandbox.resolve(&id) else {
        return FsAction::Refresh;
    };
    let feeds_tree = kind == DocKind::OpenSpec
        && path
            .file_name()
            .is_some_and(|name| name == "tasks.md" || name == "spec.md");
    if feeds_tree {
        FsAction::Refresh
    } else {
        FsAction::Invalidate(id)
    }
}

async fn apply_fs_event(commands: &CommandBus, sandbox: &SandboxedRoot, event: SpecFsEvent) {
    match classify_fs_event(sandbox, &event) {
        FsAction::Invalidate(id) => commands.invalidate_spec(InvalidateSpec(id)),
        FsAction::Refresh => refresh_index(commands).await,
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, DataChange};

    use super::*;

    fn modify(path: &Path) -> SpecFsEvent {
        SpecFsEvent {
            kind: EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            paths: vec![path.to_path_buf()],
        }
    }

    fn fixture() -> (tempfile::TempDir, SandboxedRoot) {
        let tmp = tempfile::tempdir().unwrap();
        for file in [
            "specs/a.md",
            "openspec/changes/add-x/tasks.md",
            "openspec/changes/add-x/proposal.md",
            "openspec/specs/cap/spec.md",
        ] {
            let path = tmp.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "# x\n").unwrap();
        }
        let sandbox = SandboxedRoot::new(tmp.path()).unwrap();
        (tmp, sandbox)
    }

    #[test]
    fn a_plain_content_edit_invalidates_one_spec() {
        let (_tmp, sandbox) = fixture();
        let path = sandbox.root().join("specs/a.md");
        assert_eq!(
            classify_fs_event(&sandbox, &modify(&path)),
            FsAction::Invalidate(SpecId::new("specs/a.md")),
            "editing a plain doc only re-renders that doc"
        );
    }

    #[test]
    fn an_openspec_proposal_edit_invalidates_one_spec() {
        let (_tmp, sandbox) = fixture();
        let path = sandbox.root().join("openspec/changes/add-x/proposal.md");
        assert_eq!(
            classify_fs_event(&sandbox, &modify(&path)),
            FsAction::Invalidate(SpecId::new("openspec/changes/add-x/proposal.md")),
            "a proposal feeds no tree count"
        );
    }

    #[test]
    fn editing_tasks_or_a_capability_spec_refreshes_the_tree() {
        let (_tmp, sandbox) = fixture();
        for file in ["openspec/changes/add-x/tasks.md", "openspec/specs/cap/spec.md"] {
            let path = sandbox.root().join(file);
            assert_eq!(
                classify_fs_event(&sandbox, &modify(&path)),
                FsAction::Refresh,
                "{file} feeds a progress badge or requirement count"
            );
        }
    }

    #[test]
    fn a_create_refreshes_the_tree() {
        let (_tmp, sandbox) = fixture();
        let event = SpecFsEvent {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![sandbox.root().join("specs/a.md")],
        };
        assert_eq!(
            classify_fs_event(&sandbox, &event),
            FsAction::Refresh,
            "a create can reshape the tree"
        );
    }
}
