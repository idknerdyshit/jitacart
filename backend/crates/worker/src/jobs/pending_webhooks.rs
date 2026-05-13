//! Drainer for `pending_webhooks`.
//!
//! Settlement transactions enqueue webhook payloads here so neither side can
//! drift if the worker dies between commit and HTTP send. This job picks up
//! ready rows, fires them, deletes on success, and bumps `next_attempt_at`
//! with exponential backoff on failure.

use webhook_dispatch::{dispatch_webhook, ReqwestSender, WebhookEvent};

use super::{JobFuture, JobSlot};
use crate::{Ctx, WorkerConfig};

const BATCH: i64 = 50;
const INTERVAL_SECS: u64 = 30;
const MAX_ATTEMPTS: i32 = 8;

pub struct Job;

impl JobSlot for Job {
    fn name(&self) -> &'static str {
        "pending_webhooks"
    }
    fn interval_secs(&self, _cfg: &WorkerConfig) -> u64 {
        INTERVAL_SECS
    }
    fn run<'a>(&'a self, ctx: &'a Ctx) -> JobFuture<'a> {
        Box::pin(run(ctx))
    }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: uuid::Uuid,
        group_id: uuid::Uuid,
        payload: serde_json::Value,
        attempts: i32,
    }

    // Lease window: bump next_attempt_at far enough into the future that no
    // other worker drain (or a self-overlapping tick) picks the same row up
    // while we're dispatching. On success we DELETE; on failure the
    // exponential-backoff branch overwrites this lease with the real retry
    // time. FOR UPDATE SKIP LOCKED makes the lease-claim contention-safe.
    const LEASE_SECS: i64 = 600;

    let mut tx = ctx.pool.begin().await?;
    let rows: Vec<Row> = sqlx::query_as(
        "WITH due AS (\
             SELECT id FROM pending_webhooks \
             WHERE next_attempt_at <= now() \
             ORDER BY next_attempt_at \
             LIMIT $1 \
             FOR UPDATE SKIP LOCKED\
         ) \
         UPDATE pending_webhooks p \
         SET next_attempt_at = now() + make_interval(secs => $2::double precision) \
         FROM due \
         WHERE p.id = due.id \
         RETURNING p.id, p.group_id, p.payload, p.attempts",
    )
    .bind(BATCH)
    .bind(LEASE_SECS as f64)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    if rows.is_empty() {
        return Ok(());
    }

    let sender = ReqwestSender::new(ctx.webhook_http.clone());
    for row in rows {
        let event: WebhookEvent = match serde_json::from_value(row.payload.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    pending_id = %row.id,
                    "pending_webhook payload is malformed; dropping"
                );
                let _ = sqlx::query("DELETE FROM pending_webhooks WHERE id = $1")
                    .bind(row.id)
                    .execute(&ctx.pool)
                    .await;
                continue;
            }
        };

        match dispatch_webhook(&ctx.pool, &sender, row.group_id, &event).await {
            Ok(()) => {
                sqlx::query("DELETE FROM pending_webhooks WHERE id = $1")
                    .bind(row.id)
                    .execute(&ctx.pool)
                    .await?;
            }
            Err(e) => {
                let new_attempts = row.attempts + 1;
                if new_attempts >= MAX_ATTEMPTS {
                    tracing::warn!(
                        error = ?e,
                        pending_id = %row.id,
                        attempts = new_attempts,
                        "pending_webhook gave up after max attempts"
                    );
                    sqlx::query("DELETE FROM pending_webhooks WHERE id = $1")
                        .bind(row.id)
                        .execute(&ctx.pool)
                        .await?;
                } else {
                    // Exponential backoff: 2^attempts seconds, capped at 1 hour.
                    let delay = 2_i64.pow(new_attempts as u32).min(3600);
                    sqlx::query(
                        "UPDATE pending_webhooks \
                         SET attempts = $1, next_attempt_at = now() + make_interval(secs => $2::double precision) \
                         WHERE id = $3",
                    )
                    .bind(new_attempts)
                    .bind(delay as f64)
                    .bind(row.id)
                    .execute(&ctx.pool)
                    .await?;
                    tracing::warn!(
                        error = ?e,
                        pending_id = %row.id,
                        attempts = new_attempts,
                        delay_secs = delay,
                        "pending_webhook delivery failed; will retry"
                    );
                }
            }
        }
    }
    Ok(())
}
