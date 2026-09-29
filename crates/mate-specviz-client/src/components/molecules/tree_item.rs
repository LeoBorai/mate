use leptos::prelude::*;

use crate::components::atoms::badge::Badge;
use crate::components::atoms::icon::Icon;

/// One clickable sidebar row, with an optional trailing badge (a capability's
/// requirement count).
#[component]
pub fn TreeItem(
    #[prop(into)] label: String,
    #[prop(into)] href: String,
    #[prop(optional, into)] badge: Option<String>,
) -> impl IntoView {
    view! {
        <a href=href class="flex items-center gap-2 rounded px-2 py-1 text-sm hover:bg-slate-100">
            <Icon glyph="•" />
            <span class="flex-1 truncate">{label}</span>
            {badge.map(|text| view! { <Badge text=text /> })}
        </a>
    }
}
