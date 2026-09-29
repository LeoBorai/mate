use leptos::prelude::*;

use crate::api;
use crate::components::molecules::breadcrumbs::Breadcrumbs;
use crate::live_reload::SpecChangedNotice;

#[component]
pub fn SpecContent(#[prop(into)] id: Signal<String>) -> impl IntoView {
    let spec_changed = use_context::<ReadSignal<Option<SpecChangedNotice>>>();

    let spec = LocalResource::new(move || {
        let id = id.get();
        if let Some(notice) = spec_changed {
            notice.get().map(|n| n.spec_seq(&id));
        }
        async move {
            if id.is_empty() {
                None
            } else {
                api::get_spec(&id).await.ok()
            }
        }
    });

    view! {
        <article class="flex-1 overflow-y-auto p-8 prose max-w-3xl">
            <Suspense fallback=|| view! { <p class="text-slate-400">"Loading..."</p> }>
                {move || match spec.get() {
                    Some(Some(rendered)) => {
                        let segments = rendered.path.split('/').map(str::to_owned).collect::<Vec<_>>();
                        view! {
                            <Breadcrumbs segments=segments />
                            <div inner_html=rendered.html />
                        }
                        .into_any()
                    }
                    Some(None) => view! { <p class="text-slate-400">"Select a spec from the sidebar."</p> }
                        .into_any(),
                    None => view! { <p class="text-slate-400">"Loading..."</p> }.into_any(),
                }}
            </Suspense>
        </article>
    }
}
