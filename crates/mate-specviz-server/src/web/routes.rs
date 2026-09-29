use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::application::queries::{ListSpecs, RenderSpec};
use crate::domain::SpecId;
use crate::web::assets::static_asset;
use crate::web::sse;
use crate::web::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/specs", get(list_specs))
        .route("/api/specs/{*id}", get(get_spec))
        .route("/events", get(sse::events))
        .fallback(get(static_asset))
        .with_state(state)
}

async fn list_specs(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.queries.list_specs(ListSpecs))
}

async fn get_spec(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let query = RenderSpec(SpecId::new(id));
    match state.queries.render_spec(query).await {
        Ok(rendered) => Json(rendered).into_response(),
        Err(err) if err.is_not_found() => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
