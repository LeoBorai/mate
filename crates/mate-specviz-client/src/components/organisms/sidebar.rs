use leptos::prelude::*;

use crate::api::{self, SectionKind, SpecTreeNode};
use crate::components::atoms::badge::Badge;
use crate::components::molecules::tree_item::TreeItem;
use crate::live_reload::SpecChangedNotice;

#[component]
pub fn Sidebar() -> impl IntoView {
    let spec_changed = use_context::<ReadSignal<Option<SpecChangedNotice>>>();

    let tree = LocalResource::new(move || {
        if let Some(notice) = spec_changed {
            notice.get().map(|n| n.index_seq());
        }
        async move { api::list_specs().await }
    });

    view! {
        <nav class="w-72 shrink-0 border-r border-slate-200 p-2 overflow-y-auto">
            <Suspense fallback=|| view! { <p class="p-2 text-sm text-slate-400">"Loading specs..."</p> }>
                {move || {
                    tree.get()
                        .map(|result| match result {
                            Ok(tree) if tree.nodes.is_empty() => {
                                view! {
                                    <p class="p-2 text-sm text-slate-400">
                                        "No specs found in ./specs or ./openspec"
                                    </p>
                                }
                                    .into_any()
                            }
                            Ok(tree) => tree.nodes.into_iter().map(render_node).collect_view().into_any(),
                            Err(_) => {
                                view! { <p class="p-2 text-sm text-red-500">"Failed to load specs"</p> }
                                    .into_any()
                            }
                        })
                }}
            </Suspense>
        </nav>
    }
}

fn render_node(node: SpecTreeNode) -> AnyView {
    match node {
        SpecTreeNode::Section { kind, children } => render_section(kind, children),
        SpecTreeNode::File { id, title } => {
            view! { <TreeItem label=title href=format!("/spec/{id}") /> }.into_any()
        }
        SpecTreeNode::Capability {
            path,
            id,
            requirements,
            ..
        } => view! {
            <TreeItem label=path href=format!("/spec/{id}") badge=requirements.to_string() />
        }
        .into_any(),
        SpecTreeNode::Change {
            name,
            progress,
            children,
        } => view! {
            <details open class="pl-1">
                <summary class="flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-sm font-medium hover:bg-slate-100">
                    <span class="flex-1 truncate">{name}</span>
                    {progress.map(|p| view! { <Badge text={format!("{}/{}", p.done, p.total)} /> })}
                </summary>
                <div class="pl-3">{children.into_iter().map(render_node).collect_view()}</div>
            </details>
        }
        .into_any(),
        SpecTreeNode::Dir { name, children } => view! {
            <div class="pl-2">
                <p class="px-2 py-1 text-xs font-semibold uppercase text-slate-400">{name}</p>
                {children.into_iter().map(render_node).collect_view()}
            </div>
        }
        .into_any(),
    }
}

/// A top-level group. Archive starts collapsed — it only grows, and is
/// rarely what you opened the viewer for. An empty section still renders,
/// marked empty, so it's clear the source was found.
fn render_section(kind: SectionKind, children: Vec<SpecTreeNode>) -> AnyView {
    let empty = children.is_empty();
    let body = if empty {
        view! { <p class="px-2 py-1 text-xs italic text-slate-400">"empty"</p> }.into_any()
    } else {
        children.into_iter().map(render_node).collect_view().into_any()
    };
    view! {
        <details open={kind != SectionKind::Archive} class="mb-2">
            <summary class="cursor-pointer px-2 py-1 text-xs font-semibold uppercase tracking-wide text-slate-500">
                {kind.label()}
            </summary>
            {body}
        </details>
    }
    .into_any()
}
