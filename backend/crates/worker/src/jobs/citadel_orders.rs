//! Per-citadel order fetcher.
//!
//! The driver picks tracked citadels with active list-items requesting prices
//! and a `last_orders_synced_at` older than the configured cadence. For each
//! match it pulls one candidate character from the group's membership pool
//! (must hold `esi-markets.structure_markets.v1`) and runs
//! `market::refresh_many_for_citadel`. On access denial the candidate is marked
//! forbidden and the next one is tried; old forbidden rows are retried after
//! the structure-access backoff so recovered docking access is eventually seen.

use std::sync::Arc;

use anyhow::anyhow;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::jobs::csa::{self, AccessDimension};
use crate::Ctx;

const BATCH: i64 = 50;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping citadel_orders tick"
        );
        return Ok(());
    }

    let cadence_secs = ctx.config.esi.poll_intervals_secs.citadel_orders as i64;

    let rows: Vec<DriverRow> = sqlx::query_as(
        r#"
        SELECT m.id          AS market_id,
               m.esi_location_id,
               array_agg(DISTINCT li.type_id) AS type_ids
        FROM markets m
        JOIN list_markets lm ON lm.market_id = m.id
        JOIN lists l         ON l.id = lm.list_id AND l.status = 'open'
        JOIN list_items li   ON li.list_id = l.id AND li.status IN ('open','claimed')
        WHERE m.kind = 'public_structure'
          AND m.is_public = true
          AND (m.untrackable_until IS NULL OR m.untrackable_until < now())
          AND m.id IN (SELECT market_id FROM group_tracked_markets)
          AND (m.last_orders_synced_at IS NULL
               OR m.last_orders_synced_at < now() - make_interval(secs => $1::double precision))
        GROUP BY m.id, m.esi_location_id
        ORDER BY m.last_orders_synced_at NULLS FIRST
        LIMIT $2
        "#,
    )
    .bind(cadence_secs as f64)
    .bind(BATCH)
    .fetch_all(&ctx.pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }
    tracing::info!(count = rows.len(), "citadel_orders tick");

    let sem = Arc::new(Semaphore::new(ctx.config.worker.citadel_orders_concurrency));
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for row in rows {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        let backoff_secs = ctx.config.esi.poll_intervals_secs.structure_access_backoff as i64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = refresh_one(&pool, &token_store, &budget, &row, backoff_secs).await {
                tracing::warn!(error = ?e, market_id = %row.market_id, "citadel_orders refresh failed");
            }
        }));
    }
    for h in tasks {
        let _ = h.await;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct DriverRow {
    market_id: Uuid,
    esi_location_id: i64,
    type_ids: Vec<i64>,
}

async fn refresh_one(
    pool: &sqlx::PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    row: &DriverRow,
    backoff_secs: i64,
) -> anyhow::Result<()> {
    let candidates = csa::select_candidates(
        pool,
        row.market_id,
        "esi-markets.structure_markets.v1",
        AccessDimension::Market,
        backoff_secs,
        true,
    )
    .await?;

    if candidates.is_empty() {
        tracing::debug!(market_id = %row.market_id, "no upgrade-eligible character; skipping");
        return Ok(());
    }

    let mut succeeded = false;
    let mut access_denied_count = 0usize;
    let mut transient_count = 0usize;
    let mut last_status: Option<String> = None;

    for cid in &candidates {
        let client = match token_store.authed_client_for(*cid).await {
            Ok(c) => c,
            Err(e) => {
                transient_count += 1;
                last_status = Some(format!("client build for {cid}: {e}"));
                continue;
            }
        };
        match market::refresh_many_for_citadel(
            pool,
            &client,
            row.market_id,
            row.esi_location_id,
            &row.type_ids,
        )
        .await
        {
            Ok(out) => {
                tracing::info!(
                    market_id = %row.market_id,
                    structure_id = row.esi_location_id,
                    types = row.type_ids.len(),
                    orders = out.total_orders,
                    "citadel orders refreshed"
                );
                csa::upsert_access(pool, *cid, row.market_id, "ok", AccessDimension::Market)
                    .await?;
                let _ = token_store.persist_rotations(*cid).await;
                succeeded = true;
                break;
            }
            Err(e) => {
                let msg = format!("{e}");
                budget.record_non_2xx();
                let lower = msg.to_ascii_lowercase();
                if lower.contains("403") || lower.contains("forbidden") {
                    access_denied_count += 1;
                    csa::upsert_access(
                        pool,
                        *cid,
                        row.market_id,
                        "forbidden",
                        AccessDimension::Market,
                    )
                    .await?;
                } else if lower.contains("404") {
                    sqlx::query("UPDATE markets SET is_public = false WHERE id = $1")
                        .bind(row.market_id)
                        .execute(pool)
                        .await?;
                    return Err(anyhow!("structure 404, marked non-public"));
                } else {
                    transient_count += 1;
                }
                last_status = Some(msg);
                continue;
            }
        }
    }

    if !succeeded && access_denied_count > 0 && transient_count == 0 {
        sqlx::query(
            "UPDATE markets SET untrackable_until = now() + make_interval(secs => $1::double precision) \
             WHERE id = $2",
        )
        .bind(backoff_secs as f64)
        .bind(row.market_id)
        .execute(pool)
        .await?;
        tracing::info!(
            market_id = %row.market_id,
            last_status = ?last_status,
            backoff_secs,
            "citadel paused: no candidate could fetch orders"
        );
    } else if !succeeded {
        tracing::info!(
            market_id = %row.market_id,
            last_status = ?last_status,
            access_denied_count,
            transient_count,
            "citadel orders deferred after transient failure"
        );
    }
    Ok(())
}
