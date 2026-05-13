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

use crate::{db::Tx, extract::CurrentGroup, state::AppState};

pub use webhook_dispatch::{
    do_delete_webhook, do_get_webhook, do_upsert_webhook, WebhookConfig, WebhookEvent,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/webhook", get(get_webhook))
        .route("/groups/{id}/webhook", put(upsert_webhook))
        .route("/groups/{id}/webhook", delete(delete_webhook))
}

async fn get_webhook(
    State(_state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    tx: Tx,
) -> Result<Json<Option<WebhookConfig>>, WebhookError> {
    let mut conn = tx.acquire().await;
    Ok(Json(do_get_webhook(&mut **conn, group_id, role).await?))
}

async fn upsert_webhook(
    State(_state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    tx: Tx,
    Json(body): Json<WebhookConfig>,
) -> Result<Json<WebhookConfig>, WebhookError> {
    let mut conn = tx.acquire().await;
    Ok(Json(
        do_upsert_webhook(&mut **conn, group_id, role, body).await?,
    ))
}

async fn delete_webhook(
    State(_state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    tx: Tx,
) -> Result<StatusCode, WebhookError> {
    let mut conn = tx.acquire().await;
    do_delete_webhook(&mut **conn, group_id, role).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Enqueue a webhook event into `pending_webhooks` for the worker to drain.
///
/// Must run on the *request transaction*: writing through `state.pool`
/// directly grabs a fresh connection without `app.current_user_id` set, and
/// RLS on `pending_webhooks` would silently reject the insert. The worker
/// (BYPASSRLS) reads the row and fires the HTTP request — the api never
/// touches `group_discord_webhooks` directly here, so RLS on that table is
/// not a concern. The same atomicity property that contract settlement uses
/// applies: a 4xx/5xx handler response rolls back the enqueue along with
/// every other change, so callers can fire webhooks early without worrying
/// about partial commits.
pub async fn fire_webhook(
    executor: impl sqlx::PgExecutor<'_>,
    group_id: Uuid,
    event: &WebhookEvent,
) -> Result<(), sqlx::Error> {
    let payload = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = ?e, "failed to serialize webhook event");
            return Ok(());
        }
    };
    sqlx::query("INSERT INTO pending_webhooks (group_id, payload) VALUES ($1, $2)")
        .bind(group_id)
        .bind(payload)
        .execute(executor)
        .await?;
    Ok(())
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
