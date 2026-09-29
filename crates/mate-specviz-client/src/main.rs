#[cfg(target_arch = "wasm32")]
fn main() {
    use leptos::prelude::*;
    use mate_specviz_client::App;

    _ = console_log::init_with_level(log::Level::Debug);
    console_error_panic_hook::set_once();

    mount_to_body(|| view! { <App /> });
}

/// Host builds only exist so `cargo clippy --workspace` has something to
/// check; the real entry point is the wasm32 `main` above, built by Trunk.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
