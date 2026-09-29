use leptos::prelude::*;

#[component]
pub fn Breadcrumbs(#[prop(into)] segments: Vec<String>) -> impl IntoView {
    view! {
        <nav class="text-sm text-slate-500 flex gap-1">
            {segments
                .into_iter()
                .map(|s| view! { <span>{s}" / "</span> })
                .collect_view()}
        </nav>
    }
}
