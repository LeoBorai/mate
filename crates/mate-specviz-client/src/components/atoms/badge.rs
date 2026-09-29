use leptos::prelude::*;

#[component]
pub fn Badge(#[prop(into)] text: String) -> impl IntoView {
    view! {
        <span class="inline-flex items-center rounded-full bg-slate-100 px-2 py-0.5 text-xs text-slate-600">
            {text}
        </span>
    }
}
