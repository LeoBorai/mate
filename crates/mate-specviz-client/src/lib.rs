//! The specviz web UI: a Leptos CSR app compiled to `wasm32-unknown-unknown`
//! by Trunk and embedded into `mate-specviz-server` via `rust-embed`. Gated to
//! wasm32 so host workspace builds see an empty crate.
#![cfg(target_arch = "wasm32")]

use leptos::prelude::*;
use leptos_meta::*;
use leptos_router::{components::*, path};

pub mod api;
pub mod components;
pub mod live_reload;

use crate::components::pages::spec_page::SpecPage;
use crate::live_reload::SpecChangedNotice;

/// Router root: provides meta context, opens the live-reload SSE
/// connection, and renders the spec page for any `/spec/*id` route (and
/// the empty-state root route).
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let (spec_changed, set_spec_changed) = signal(None::<SpecChangedNotice>);
    live_reload::connect(set_spec_changed);
    provide_context(spec_changed);

    view! {
        <Html attr:lang="en" attr:dir="ltr" />
        <Title text="specviz" />
        <Meta charset="UTF-8" />
        <Meta name="viewport" content="width=device-width, initial-scale=1.0" />

        <Router>
            <Routes fallback=|| view! { <p class="p-4">"Not found"</p> }>
                <Route path=path!("/") view=SpecPage />
                <Route path=path!("/spec/*id") view=SpecPage />
            </Routes>
        </Router>
    }
}
