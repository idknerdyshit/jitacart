//! Durable backlog drain that resolves citadel name/region/system/structure_type.
//!
//! Two driving queries:
//!  - **B (refresh)**: tracked citadels with stale or missing details, run first.
//!  - **A (backfill)**: any discovered citadel still missing details, capped per
//!    tick so the worker doesn't hammer ESI for tens of thousands of structures.
//!
//! Both share the same per-row resolver. A row that fails for every candidate
//! character is left with `details_synced_at = NULL`; the search UI hides
//! those because we have no name to match against.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use nea_esi::{EsiClient, EsiStructureInfo};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::jobs::csa::{self, AccessDimension};
use crate::Ctx;

const REFRESH_BATCH: i64 = 50;
const BACKFILL_BATCH: i64 = 25;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping citadel_details tick"
        );
        return Ok(());
    }

    let refresh_secs = ctx.config.esi.poll_intervals_secs.citadel_details as i64;

    // B: tracked-and-stale, oldest first.
    let refresh_rows: Vec<TargetRow> = sqlx::query_as(
        r#"
        SELECT m.id, m.esi_location_id
        FROM markets m
        WHERE m.kind='public_structure' AND m.is_public=true
          AND m.id IN (SELECT market_id FROM group_tracked_markets)
          AND (m.details_synced_at IS NULL
               OR m.details_synced_at < now() - make_interval(secs => $1::double precision))
        ORDER BY m.details_synced_at NULLS FIRST
        LIMIT $2
        "#,
    )
    .bind(refresh_secs as f64)
    .bind(REFRESH_BATCH)
    .fetch_all(&ctx.pool)
    .await?;

    // A: backlog — all discovered but undetailed, oldest first.
    let backfill_rows: Vec<TargetRow> = sqlx::query_as(
        r#"
        SELECT id, esi_location_id FROM markets
        WHERE kind='public_structure' AND is_public=true AND details_synced_at IS NULL
        ORDER BY created_at
        LIMIT $1
        "#,
    )
    .bind(BACKFILL_BATCH)
    .fetch_all(&ctx.pool)
    .await?;

    let total = refresh_rows.len() + backfill_rows.len();
    if total == 0 {
        return Ok(());
    }
    tracing::info!(
        refresh = refresh_rows.len(),
        backfill = backfill_rows.len(),
        "citadel_details tick"
    );

    let sem = Arc::new(Semaphore::new(
        ctx.config.worker.citadel_details_concurrency,
    ));
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for row in refresh_rows.into_iter().chain(backfill_rows) {
        let permit = sem.clone().acquire_owned().await?;
        let pool = ctx.pool.clone();
        let token_store = ctx.token_store.clone();
        let budget = ctx.budget.clone();
        let anon = Arc::clone(&ctx.esi_anon);
        let access_backoff_secs =
            ctx.config.esi.poll_intervals_secs.structure_access_backoff as i64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = resolve_one(
                &pool,
                &token_store,
                &budget,
                &anon,
                row,
                access_backoff_secs,
            )
            .await
            {
                tracing::warn!(error = ?e, "citadel detail resolve failed");
            }
        }));
    }

    for h in tasks {
        let _ = h.await;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    id: Uuid,
    esi_location_id: i64,
}

async fn resolve_one(
    pool: &sqlx::PgPool,
    token_store: &auth_tokens::CharacterTokenStore,
    budget: &auth_tokens::EsiBudgetGuard,
    anon: &EsiClient,
    row: TargetRow,
    access_backoff_secs: i64,
) -> anyhow::Result<()> {
    let candidates = csa::select_candidates(
        pool,
        row.id,
        "esi-universe.read_structures.v1",
        AccessDimension::Details,
        access_backoff_secs,
        false,
    )
    .await?;

    if candidates.is_empty() {
        // Bootstrap state: nobody has the scope yet. Quietly no-op.
        tracing::debug!(
            structure_id = row.esi_location_id,
            "no upgrade-eligible character available"
        );
        return Ok(());
    }

    let mut info: Option<EsiStructureInfo> = None;
    let mut last_err: Option<String> = None;

    for cid in &candidates {
        let client = match token_store.authed_client_for(*cid).await {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(format!("client build for {cid}: {e}"));
                continue;
            }
        };
        match client.get_structure(row.esi_location_id).await {
            Ok(s) => {
                info = Some(s);
                csa::upsert_access(pool, *cid, row.id, "ok", AccessDimension::Details).await?;
                let _ = token_store.persist_rotations(*cid).await;
                break;
            }
            Err(e) => {
                let msg = format!("{e}");
                budget.record_non_2xx();
                if msg.contains("403") || msg.to_ascii_lowercase().contains("forbidden") {
                    csa::upsert_access(pool, *cid, row.id, "forbidden", AccessDimension::Details)
                        .await?;
                }
                last_err = Some(msg);
                continue;
            }
        }
    }

    let Some(info) = info else {
        tracing::debug!(structure_id = row.esi_location_id, error = ?last_err, "no character could fetch structure details");
        return Ok(());
    };

    let region_id = resolve_region_for_system(anon, info.solar_system_id).await?;
    let short = derive_short_label(&info.name);

    sqlx::query(
        r#"
        UPDATE markets
        SET name = $1,
            short_label = $2,
            region_id = $3,
            solar_system_id = $4,
            structure_type_id = $5,
            details_synced_at = now()
        WHERE id = $6
        "#,
    )
    .bind(&info.name)
    .bind(&short)
    .bind(region_id)
    .bind(info.solar_system_id as i64)
    .bind(info.type_id)
    .bind(row.id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Walk system → constellation → region. Both endpoints are public and the
/// nea-esi cache layer ETags them, so this is essentially free after the
/// first hit per system.
async fn resolve_region_for_system(esi: &EsiClient, system_id: i32) -> anyhow::Result<i64> {
    static CACHE: tokio::sync::OnceCell<tokio::sync::RwLock<HashMap<i32, i64>>> =
        tokio::sync::OnceCell::const_new();
    let cache = CACHE
        .get_or_init(|| async { tokio::sync::RwLock::new(HashMap::new()) })
        .await;
    if let Some(r) = cache.read().await.get(&system_id) {
        return Ok(*r);
    }

    let sys = esi
        .get_system(system_id)
        .await
        .map_err(|e| anyhow!("get_system({system_id}): {e}"))?;
    let con = esi
        .get_constellation(sys.constellation_id)
        .await
        .map_err(|e| anyhow!("get_constellation({}): {e}", sys.constellation_id))?;
    let region_id = con.region_id as i64;
    cache.write().await.insert(system_id, region_id);
    Ok(region_id)
}

fn derive_short_label(name: &str) -> String {
    // Examples: "1DQ1-A - 1-st Imperial Palace" → "1DQ1-A"; otherwise the
    // first 23 chars plus an ellipsis or up to the first " - ".
    if let Some((pre, _)) = name.split_once(" - ") {
        let pre = pre.trim();
        if pre.chars().count() <= 24 {
            return pre.to_string();
        }
    }
    let trimmed = name.trim();
    if trimmed.chars().count() <= 24 {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(23).collect();
        format!("{prefix}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_label_from_dash() {
        assert_eq!(
            derive_short_label("1DQ1-A - 1-st Imperial Palace"),
            "1DQ1-A"
        );
    }

    #[test]
    fn short_label_truncates_long_name() {
        let s = derive_short_label("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA Keepstar");
        // 23 ASCII chars + 3-byte ellipsis = 26 bytes, but only 24 graphemes.
        assert!(s.chars().count() <= 24);
    }

    #[test]
    fn short_label_truncates_non_ascii_name() {
        let s = derive_short_label("あいうえおかきくけこさしすせそたちつてとなにぬねの Keepstar");
        assert!(s.chars().count() <= 24);
        assert!(s.ends_with('…'));
    }
}
