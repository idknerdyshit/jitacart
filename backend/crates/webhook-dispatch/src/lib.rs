//! Discord webhook configuration storage and fire-and-forget delivery.
//!
//! This crate is shared by the API (which exposes the CRUD HTTP routes and
//! triggers events from request handlers) and the worker (which fires
//! settlement events after background jobs commit). Both call into the same
//! `do_*` helpers and `dispatch_webhook` so the embed format and toggle
//! semantics stay in lockstep.

use domain::GroupRole;
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    BadRequest(String),
    #[error("forbidden")]
    Forbidden,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Send(anyhow::Error),
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

async fn fetch_row(
    executor: impl PgExecutor<'_>,
    group_id: Uuid,
) -> Result<Option<WebhookRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT webhook_url, notify_list_created, notify_list_claimed, \
                notify_list_delivered, notify_reimbursement_settled \
         FROM group_discord_webhooks WHERE group_id = $1",
    )
    .bind(group_id)
    .fetch_optional(executor)
    .await
}

pub async fn do_get_webhook(
    executor: impl PgExecutor<'_>,
    group_id: Uuid,
    role: GroupRole,
) -> Result<Option<WebhookConfig>, Error> {
    if role != GroupRole::Owner {
        return Err(Error::Forbidden);
    }
    Ok(fetch_row(executor, group_id).await?.map(|r| WebhookConfig {
        webhook_url: r.webhook_url,
        notify_list_created: r.notify_list_created,
        notify_list_claimed: r.notify_list_claimed,
        notify_list_delivered: r.notify_list_delivered,
        notify_reimbursement_settled: r.notify_reimbursement_settled,
    }))
}

pub async fn do_upsert_webhook(
    executor: impl PgExecutor<'_>,
    group_id: Uuid,
    role: GroupRole,
    body: WebhookConfig,
) -> Result<WebhookConfig, Error> {
    if role != GroupRole::Owner {
        return Err(Error::Forbidden);
    }
    if body.webhook_url.len() > 512 {
        return Err(Error::BadRequest("webhook_url is too long".into()));
    }
    if !body
        .webhook_url
        .starts_with("https://discord.com/api/webhooks/")
        && !body
            .webhook_url
            .starts_with("https://discordapp.com/api/webhooks/")
    {
        return Err(Error::BadRequest(
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
    .execute(executor)
    .await?;

    Ok(body)
}

pub async fn do_delete_webhook(
    executor: impl PgExecutor<'_>,
    group_id: Uuid,
    role: GroupRole,
) -> Result<(), Error> {
    if role != GroupRole::Owner {
        return Err(Error::Forbidden);
    }
    sqlx::query("DELETE FROM group_discord_webhooks WHERE group_id = $1")
        .bind(group_id)
        .execute(executor)
        .await?;
    Ok(())
}

// ── Fire-and-forget webhook delivery ────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
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

/// Pluggable HTTP layer so tests can capture payloads without hitting the
/// network. The real implementation is [`ReqwestSender`].
#[async_trait::async_trait]
pub trait WebhookSender: Send + Sync {
    async fn send(&self, url: &str, payload: serde_json::Value) -> anyhow::Result<()>;
}

pub struct ReqwestSender(reqwest::Client);

impl ReqwestSender {
    pub fn new(client: reqwest::Client) -> Self {
        Self(client)
    }
}

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
    executor: impl PgExecutor<'_>,
    sender: &dyn WebhookSender,
    group_id: Uuid,
    event: &WebhookEvent,
) -> Result<(), Error> {
    let row = match fetch_row(executor, group_id).await? {
        Some(r) => r,
        None => return Ok(()),
    };
    if !event.is_enabled(&row) {
        return Ok(());
    }
    sender
        .send(&row.webhook_url, build_payload(event))
        .await
        .map_err(Error::Send)?;
    Ok(())
}
