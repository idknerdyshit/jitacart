//! Axum routing and response shims for Discord webhook config. The shared
//! storage and delivery logic lives in the `webhook-dispatch` crate.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, put},
    Json, Router,
};
use uuid::Uuid;

use crate::{extract::CurrentGroup, state::AppState};

pub use webhook_dispatch::{
    build_payload, dispatch_webhook, do_delete_webhook, do_get_webhook, do_upsert_webhook,
    ReqwestSender, WebhookConfig, WebhookEvent, WebhookSender,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/webhook", get(get_webhook))
        .route("/groups/{id}/webhook", put(upsert_webhook))
        .route("/groups/{id}/webhook", delete(delete_webhook))
}

async fn get_webhook(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<Option<WebhookConfig>>, WebhookError> {
    Ok(Json(do_get_webhook(&state.pool, group_id, role).await?))
}

async fn upsert_webhook(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    Json(body): Json<WebhookConfig>,
) -> Result<Json<WebhookConfig>, WebhookError> {
    Ok(Json(
        do_upsert_webhook(&state.pool, group_id, role, body).await?,
    ))
}

async fn delete_webhook(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<StatusCode, WebhookError> {
    do_delete_webhook(&state.pool, group_id, role).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn fire_webhook(state: &AppState, group_id: Uuid, event: WebhookEvent) {
    let pool = state.pool.clone();
    let sender = ReqwestSender(state.webhook_http.clone());
    tokio::spawn(async move {
        if let Err(e) = dispatch_webhook(&pool, &sender, group_id, &event).await {
            tracing::warn!(group_id = %group_id, error = ?e, "webhook delivery failed");
        }
    });
}

#[derive(Debug)]
pub struct WebhookError(pub webhook_dispatch::Error);

impl From<webhook_dispatch::Error> for WebhookError {
    fn from(e: webhook_dispatch::Error) -> Self {
        WebhookError(e)
    }
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        match self.0 {
            webhook_dispatch::Error::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, msg).into_response()
            }
            webhook_dispatch::Error::Forbidden => {
                (StatusCode::FORBIDDEN, "owner only").into_response()
            }
            webhook_dispatch::Error::Db(e) => {
                tracing::error!(error = ?e, "webhook handler db error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
            webhook_dispatch::Error::Send(e) => {
                tracing::error!(error = ?e, "webhook handler send error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
