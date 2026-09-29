//! Client-side live reload: subscribes to the server's `/events` SSE
//! stream and republishes each `spec-changed` message as reactive state
//! for other components to react to.

use gloo_net::eventsource::futures::EventSource;
use leptos::prelude::*;

use crate::api::SpecChangedKind;

/// One `spec-changed` message, tagged with a strictly increasing sequence
/// number so repeated events for the same spec still trigger a reactive
/// update (a Leptos signal only notifies subscribers when the value
/// changes, and two consecutive edits can carry an identical `kind`).
#[derive(Debug, Clone)]
pub struct SpecChangedNotice {
    seq: u64,
    kind: SpecChangedKind,
}

impl SpecChangedNotice {
    /// The notice's sequence number if it's an `Index` event, `0` otherwise.
    pub fn index_seq(&self) -> u64 {
        match self.kind {
            SpecChangedKind::Index => self.seq,
            SpecChangedKind::Spec { .. } => 0,
        }
    }

    /// The notice's sequence number if it's a `Spec` event for `id`, `0`
    /// otherwise.
    pub fn spec_seq(&self, id: &str) -> u64 {
        match &self.kind {
            SpecChangedKind::Spec { id: changed } if changed == id => self.seq,
            _ => 0,
        }
    }
}

/// Opens one `EventSource` against `/events` for the page's lifetime and
/// writes each parsed `spec-changed` message into `set_notice`. Silently
/// stops on a connection error — the browser's own `EventSource`
/// auto-reconnect handles transient drops before that point.
pub fn connect(set_notice: WriteSignal<Option<SpecChangedNotice>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(mut source) = EventSource::new("/events") else {
            return;
        };
        let Ok(mut stream) = source.subscribe("spec-changed") else {
            return;
        };

        let mut seq = 0u64;
        while let Some(Ok((_, message))) = futures::StreamExt::next(&mut stream).await {
            let Some(data) = message.data().as_string() else {
                continue;
            };
            let Ok(kind) = serde_json::from_str::<SpecChangedKind>(&data) else {
                continue;
            };
            seq += 1;
            set_notice.set(Some(SpecChangedNotice { seq, kind }));
        }
    });
}
