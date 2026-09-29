use leptos::prelude::*;

/// Sidebar + content layout skeleton — no data, just structure.
#[component]
pub fn AppShell(children: Children) -> impl IntoView {
    view! { <div class="flex h-screen w-screen bg-white text-slate-900">{children()}</div> }
}
