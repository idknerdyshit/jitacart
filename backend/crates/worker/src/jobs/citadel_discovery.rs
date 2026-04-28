//! Discover all public structures via `/universe/structures/`.
//!
//! New ids are upserted with `kind='public_structure'` and detail-fields left
//! NULL until `citadel_details` resolves them. Existing rows whose owner has
//! taken them private accumulate `missing_poll_count`; once over the
//! threshold, `is_public=false` flips and the chip picker greys them out.

use anyhow::anyhow;
use uuid::Uuid;

use crate::Ctx;

pub async fn run(ctx: &Ctx) -> anyhow::Result<()> {
    if !ctx.budget.has_budget() {
        tracing::warn!(
            remaining = ctx.budget.remaining(),
            "esi budget low; skipping citadel_discovery tick"
        );
        return Ok(());
    }

    let ids: Vec<i64> = match ctx.esi_anon.list_public_structure_ids().await {
        Ok(v) => v,
        Err(e) => {
            ctx.budget.record_non_2xx();
            return Err(anyhow!("list_public_structure_ids: {e}"));
        }
    };
    tracing::info!(count = ids.len(), "discovered public structures");

    let mut tx = ctx.pool.begin().await?;

    // Upsert: existing rows have `last_seen_public_at` bumped and missing_poll_count zeroed;
    // new rows are inserted with NULL details.
    sqlx::query(
        r#"
        INSERT INTO markets (
            kind, esi_location_id, region_id, name, short_label, is_hub, is_public,
            last_seen_public_at, missing_poll_count
        )
        SELECT 'public_structure', id, NULL::bigint, NULL::text, NULL::text, false, true, now(), 0
        FROM UNNEST($1::bigint[]) AS t(id)
        ON CONFLICT (esi_location_id) DO UPDATE SET
            last_seen_public_at = now(),
            missing_poll_count  = 0,
            is_public           = true
        "#,
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;

    let threshold = ctx.config.worker.citadel_discovery_missing_threshold;

    // Bump missing_poll_count for citadels not in this response.
    sqlx::query(
        r#"
        UPDATE markets m
        SET missing_poll_count = missing_poll_count + 1
        WHERE m.kind = 'public_structure'
          AND m.is_public = true
          AND NOT (m.esi_location_id = ANY($1::bigint[]))
        "#,
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;

    // Soft-disable any structures that have been missing for too long.
    let disabled: Vec<(Uuid, i64)> = sqlx::query_as(
        r#"
        UPDATE markets
        SET is_public = false
        WHERE kind = 'public_structure'
          AND is_public = true
          AND missing_poll_count >= $1
        RETURNING id, esi_location_id
        "#,
    )
    .bind(threshold)
    .fetch_all(&mut *tx)
    .await?;

    if !disabled.is_empty() {
        tracing::info!(
            count = disabled.len(),
            "soft-disabled stale public structures"
        );
    }

    tx.commit().await?;
    Ok(())
}
