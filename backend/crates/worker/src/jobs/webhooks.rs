use settlement::ContractSettledReimbursement;
use sqlx::PgPool;

#[derive(sqlx::FromRow)]
struct WebhookRow {
    webhook_url: String,
    notify_reimbursement_settled: bool,
}

pub async fn fire_settlement_webhooks(
    pool: &PgPool,
    http: &reqwest::Client,
    settled: Vec<ContractSettledReimbursement>,
) {
    for info in settled {
        if let Err(e) = fire_one(pool, http, info.group_id, &info).await {
            tracing::warn!(
                group_id = %info.group_id,
                error = ?e,
                "webhook delivery failed (contract settlement)"
            );
        }
    }
}

async fn fire_one(
    pool: &PgPool,
    http: &reqwest::Client,
    group_id: uuid::Uuid,
    info: &ContractSettledReimbursement,
) -> anyhow::Result<()> {
    let row: Option<WebhookRow> = sqlx::query_as(
        "SELECT webhook_url, notify_reimbursement_settled \
         FROM group_discord_webhooks WHERE group_id = $1",
    )
    .bind(group_id)
    .fetch_optional(pool)
    .await?;

    let row = match row {
        Some(r) if r.notify_reimbursement_settled => r,
        _ => return Ok(()),
    };

    let payload = serde_json::json!({
        "embeds": [{
            "title": format!("Reimbursement settled: {}", info.list_destination),
            "description": format!(
                "{} → {}: {} ISK",
                info.requester_name, info.hauler_name, info.total_isk
            ),
            "color": 0x8b949eu32,
        }]
    });

    let resp = http.post(&row.webhook_url).json(&payload).send().await?;
    resp.error_for_status()?;
    Ok(())
}
