use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::application::events::SpecChanged;
use crate::web::state::AppState;

/// `GET /events` — the live-reload feed the client subscribes to via
/// `EventSource`.
pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.commands.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| {
        let payload = match msg.ok()? {
            SpecChanged::IndexRefreshed => json!({ "kind": "index" }),
            SpecChanged::Spec(id) => json!({ "kind": "spec", "id": id.as_str() }),
        };
        Some(Ok(Event::default()
            .event("spec-changed")
            .json_data(payload)
            .ok()?))
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
