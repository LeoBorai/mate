use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

use crate::components::organisms::sidebar::Sidebar;
use crate::components::organisms::spec_content::SpecContent;
use crate::components::templates::app_shell::AppShell;

#[component]
pub fn SpecPage() -> impl IntoView {
    let params = use_params_map();
    let id = Signal::derive(move || params.get().get("id").unwrap_or_default());

    view! {
        <AppShell>
            <Sidebar />
            <SpecContent id=id />
        </AppShell>
    }
}
