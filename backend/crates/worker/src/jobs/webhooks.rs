use settlement::ContractSettledReimbursement;
use sqlx::PgPool;
use webhook_dispatch::{dispatch_webhook, ReqwestSender, WebhookEvent};

pub async fn fire_settlement_webhooks(
    pool: &PgPool,
    http: &reqwest::Client,
    settled: Vec<ContractSettledReimbursement>,
) {
    let sender = ReqwestSender(http.clone());
    let sends = settled.into_iter().map(|info| {
        let sender = &sender;
        async move {
            let event = WebhookEvent::ReimbursementSettled {
                list_destination: info.list_destination.clone(),
                requester_name: info.requester_name.clone(),
                hauler_name: info.hauler_name.clone(),
                total_isk: info.total_isk.to_string(),
            };
            if let Err(e) = dispatch_webhook(pool, sender, info.group_id, &event).await {
                tracing::warn!(
                    group_id = %info.group_id,
                    error = ?e,
                    "webhook delivery failed (contract settlement)"
                );
            }
        }
    });
    futures_util::future::join_all(sends).await;
}
