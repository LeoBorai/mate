use leptos::prelude::*;

#[component]
pub fn Icon(#[prop(into)] glyph: String) -> impl IntoView {
    view! { <span class="inline-block w-4 h-4 text-slate-400" aria-hidden="true">{glyph}</span> }
}
