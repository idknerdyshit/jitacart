//! Discord webhook configuration and fire-and-forget delivery.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, put},
    Json, Router,
};
use domain::GroupRole;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{extract::CurrentGroup, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/webhook", get(get_webhook))
        .route("/groups/{id}/webhook", put(upsert_webhook))
        .route("/groups/{id}/webhook", delete(delete_webhook))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WebhookConfig {
    pub webhook_url: String,
    pub notify_list_created: bool,
    pub notify_list_claimed: bool,
    pub notify_list_delivered: bool,
    pub notify_reimbursement_settled: bool,
}

#[derive(sqlx::FromRow)]
struct WebhookRow {
    webhook_url: String,
    notify_list_created: bool,
    notify_list_claimed: bool,
    notify_list_delivered: bool,
    notify_reimbursement_settled: bool,
}

async fn get_webhook(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<Option<WebhookConfig>>, WebhookError> {
    Ok(Json(do_get_webhook(&state.pool, group_id, role).await?))
}

pub async fn do_get_webhook(
    pool: &PgPool,
    group_id: Uuid,
    role: GroupRole,
) -> Result<Option<WebhookConfig>, WebhookError> {
    if role != GroupRole::Owner {
        return Err(WebhookError::Forbidden);
    }
    let row: Option<WebhookRow> = sqlx::query_as(
        "SELECT webhook_url, notify_list_created, notify_list_claimed, \
                notify_list_delivered, notify_reimbursement_settled \
         FROM group_discord_webhooks WHERE group_id = $1",
    )
    .bind(group_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    Ok(row.map(|r| WebhookConfig {
        webhook_url: r.webhook_url,
        notify_list_created: r.notify_list_created,
        notify_list_claimed: r.notify_list_claimed,
        notify_list_delivered: r.notify_list_delivered,
        notify_reimbursement_settled: r.notify_reimbursement_settled,
    }))
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

pub async fn do_upsert_webhook(
    pool: &PgPool,
    group_id: Uuid,
    role: GroupRole,
    body: WebhookConfig,
) -> Result<WebhookConfig, WebhookError> {
    if role != GroupRole::Owner {
        return Err(WebhookError::Forbidden);
    }
    if !body
        .webhook_url
        .starts_with("https://discord.com/api/webhooks/")
        && !body
            .webhook_url
            .starts_with("https://discordapp.com/api/webhooks/")
    {
        return Err(WebhookError::BadRequest(
            "webhook_url must be a Discord webhook URL".into(),
        ));
    }

    sqlx::query(
        r#"
        INSERT INTO group_discord_webhooks
            (group_id, webhook_url, notify_list_created, notify_list_claimed,
             notify_list_delivered, notify_reimbursement_settled)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (group_id) DO UPDATE SET
            webhook_url = EXCLUDED.webhook_url,
            notify_list_created = EXCLUDED.notify_list_created,
            notify_list_claimed = EXCLUDED.notify_list_claimed,
            notify_list_delivered = EXCLUDED.notify_list_delivered,
            notify_reimbursement_settled = EXCLUDED.notify_reimbursement_settled,
            updated_at = now()
        "#,
    )
    .bind(group_id)
    .bind(&body.webhook_url)
    .bind(body.notify_list_created)
    .bind(body.notify_list_claimed)
    .bind(body.notify_list_delivered)
    .bind(body.notify_reimbursement_settled)
    .execute(pool)
    .await
    .map_err(internal)?;

    Ok(body)
}

async fn delete_webhook(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<StatusCode, WebhookError> {
    do_delete_webhook(&state.pool, group_id, role).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn do_delete_webhook(
    pool: &PgPool,
    group_id: Uuid,
    role: GroupRole,
) -> Result<(), WebhookError> {
    if role != GroupRole::Owner {
        return Err(WebhookError::Forbidden);
    }
    sqlx::query("DELETE FROM group_discord_webhooks WHERE group_id = $1")
        .bind(group_id)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(())
}

// ── Fire-and-forget webhook delivery ────────────────────────────────────────

#[derive(Clone)]
pub enum WebhookEvent {
    ListCreated {
        list_destination: String,
        item_count: i64,
        estimate_isk: String,
        creator_name: String,
    },
    ListClaimed {
        list_destination: String,
        hauler_name: String,
        item_count: usize,
    },
    ListDelivered {
        list_destination: String,
        hauler_name: String,
    },
    ReimbursementSettled {
        list_destination: String,
        requester_name: String,
        hauler_name: String,
        total_isk: String,
    },
}

impl WebhookEvent {
    fn is_enabled(&self, row: &WebhookRow) -> bool {
        match self {
            WebhookEvent::ListCreated { .. } => row.notify_list_created,
            WebhookEvent::ListClaimed { .. } => row.notify_list_claimed,
            WebhookEvent::ListDelivered { .. } => row.notify_list_delivered,
            WebhookEvent::ReimbursementSettled { .. } => row.notify_reimbursement_settled,
        }
    }

    fn embed_color(&self) -> u32 {
        match self {
            WebhookEvent::ListCreated { .. } => 0x2f81f7,
            WebhookEvent::ListClaimed { .. } => 0xd29922,
            WebhookEvent::ListDelivered { .. } => 0x3fb950,
            WebhookEvent::ReimbursementSettled { .. } => 0x8b949e,
        }
    }

    fn title(&self) -> String {
        match self {
            WebhookEvent::ListCreated {
                list_destination, ..
            } => {
                format!("New list: {list_destination}")
            }
            WebhookEvent::ListClaimed {
                list_destination,
                hauler_name,
                ..
            } => format!("{hauler_name} claimed items on {list_destination}"),
            WebhookEvent::ListDelivered {
                list_destination,
                hauler_name,
                ..
            } => format!("{hauler_name} delivered {list_destination}"),
            WebhookEvent::ReimbursementSettled {
                list_destination, ..
            } => format!("Reimbursement settled: {list_destination}"),
        }
    }

    fn description(&self) -> String {
        match self {
            WebhookEvent::ListCreated {
                item_count,
                estimate_isk,
                creator_name,
                ..
            } => format!(
                "{creator_name} created a list with {item_count} items (est. {estimate_isk} ISK)"
            ),
            WebhookEvent::ListClaimed { item_count, .. } => {
                format!("{item_count} item(s) claimed")
            }
            WebhookEvent::ListDelivered { .. } => "All items delivered/settled".into(),
            WebhookEvent::ReimbursementSettled {
                requester_name,
                hauler_name,
                total_isk,
                ..
            } => format!("{requester_name} → {hauler_name}: {total_isk} ISK"),
        }
    }
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

/// Pluggable HTTP layer so tests can capture payloads without hitting the
/// network. The real implementation is [`ReqwestSender`].
#[async_trait::async_trait]
pub trait WebhookSender: Send + Sync {
    async fn send(&self, url: &str, payload: serde_json::Value) -> anyhow::Result<()>;
}

pub struct ReqwestSender(pub reqwest::Client);

#[async_trait::async_trait]
impl WebhookSender for ReqwestSender {
    async fn send(&self, url: &str, payload: serde_json::Value) -> anyhow::Result<()> {
        self.0
            .post(url)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

pub fn build_payload(event: &WebhookEvent) -> serde_json::Value {
    serde_json::json!({
        "embeds": [{
            "title": event.title(),
            "description": event.description(),
            "color": event.embed_color(),
        }]
    })
}

pub async fn dispatch_webhook(
    pool: &PgPool,
    sender: &dyn WebhookSender,
    group_id: Uuid,
    event: &WebhookEvent,
) -> anyhow::Result<()> {
    let row: Option<WebhookRow> = sqlx::query_as(
        "SELECT webhook_url, notify_list_created, notify_list_claimed, \
                notify_list_delivered, notify_reimbursement_settled \
         FROM group_discord_webhooks WHERE group_id = $1",
    )
    .bind(group_id)
    .fetch_optional(pool)
    .await?;

    let row = match row {
        Some(r) => r,
        None => return Ok(()),
    };
    if !event.is_enabled(&row) {
        return Ok(());
    }
    sender.send(&row.webhook_url, build_payload(event)).await?;
    Ok(())
}

// ── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WebhookError {
    BadRequest(String),
    Forbidden,
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> WebhookError {
    WebhookError::Internal(e.into())
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        match self {
            WebhookError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            WebhookError::Forbidden => (StatusCode::FORBIDDEN, "owner only").into_response(),
            WebhookError::Internal(e) => {
                tracing::error!(error = ?e, "webhook handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
