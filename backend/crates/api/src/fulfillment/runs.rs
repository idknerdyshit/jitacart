use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use domain::{ListStatus, RunMarketRef, RunSummary};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{errors::ApiError, extract::CurrentGroup, state::AppState};

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    destination_label: Option<String>,
    status: ListStatus,
    created_at: DateTime<Utc>,
    total_estimate_isk: Decimal,
    items_open: i64,
    items_claimed: i64,
    items_bought: i64,
    items_delivered: i64,
    items_settled: i64,
    claimed_by_me: bool,
    my_active_claim_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct RunMarketRow {
    list_id: Uuid,
    market_id: Uuid,
    short_label: Option<String>,
    is_primary: bool,
}

pub(super) async fn runs(
    State(state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
) -> Result<Json<Vec<RunSummary>>, ApiError> {
    let rows: Vec<RunRow> = sqlx::query_as(
        r#"
        WITH item_agg AS (
            SELECT
                li.list_id,
                COUNT(*) FILTER (WHERE li.status = 'open')      AS items_open,
                COUNT(*) FILTER (WHERE li.status = 'claimed')   AS items_claimed,
                COUNT(*) FILTER (WHERE li.status = 'bought')    AS items_bought,
                COUNT(*) FILTER (WHERE li.status = 'delivered') AS items_delivered,
                COUNT(*) FILTER (WHERE li.status = 'settled')   AS items_settled
            FROM list_items li
            JOIN lists l ON l.id = li.list_id
            WHERE l.group_id = $1 AND l.status = 'open'
            GROUP BY li.list_id
        ),
        my_claim AS (
            SELECT DISTINCT ON (c.list_id)
                   c.list_id,
                   c.id AS my_active_claim_id
            FROM claims c
            JOIN lists l ON l.id = c.list_id
            WHERE l.group_id = $1 AND l.status = 'open'
              AND c.hauler_user_id = $2 AND c.status = 'active'
            ORDER BY c.list_id, c.created_at DESC, c.id DESC
        )
        SELECT
            l.id,
            l.destination_label,
            l.status,
            l.created_at,
            l.total_estimate_isk,
            COALESCE(ia.items_open,      0) AS items_open,
            COALESCE(ia.items_claimed,   0) AS items_claimed,
            COALESCE(ia.items_bought,    0) AS items_bought,
            COALESCE(ia.items_delivered, 0) AS items_delivered,
            COALESCE(ia.items_settled,   0) AS items_settled,
            (mc.my_active_claim_id IS NOT NULL) AS claimed_by_me,
            mc.my_active_claim_id            AS my_active_claim_id
        FROM lists l
        LEFT JOIN item_agg ia ON ia.list_id = l.id
        LEFT JOIN my_claim mc ON mc.list_id = l.id
        WHERE l.group_id = $1
          AND l.status = 'open'
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;

    if rows.is_empty() {
        return Ok(Json(vec![]));
    }

    let list_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let market_rows: Vec<RunMarketRow> = sqlx::query_as(
        r#"
        SELECT lm.list_id, m.id AS market_id, m.short_label, lm.is_primary
        FROM list_markets lm
        JOIN markets m ON m.id = lm.market_id
        WHERE lm.list_id = ANY($1::uuid[])
        ORDER BY lm.list_id, lm.is_primary DESC, m.short_label
        "#,
    )
    .bind(&list_ids)
    .fetch_all(&state.pool)
    .await?;

    let mut markets_by_list: std::collections::HashMap<Uuid, Vec<RunMarketRef>> =
        std::collections::HashMap::new();
    for mr in market_rows {
        markets_by_list
            .entry(mr.list_id)
            .or_default()
            .push(RunMarketRef {
                market_id: mr.market_id,
                short_label: mr.short_label,
                is_primary: mr.is_primary,
            });
    }

    let summaries: Vec<RunSummary> = rows
        .into_iter()
        .map(|r| RunSummary {
            list_id: r.id,
            destination_label: r.destination_label,
            status: r.status,
            created_at: r.created_at,
            accepted_markets: markets_by_list.remove(&r.id).unwrap_or_default(),
            items_open: r.items_open,
            items_claimed: r.items_claimed,
            items_bought: r.items_bought,
            items_delivered: r.items_delivered,
            items_settled: r.items_settled,
            total_estimate_isk: r.total_estimate_isk,
            claimed_by_me: r.claimed_by_me,
            my_active_claim_id: r.my_active_claim_id,
        })
        .collect();

    Ok(Json(summaries))
}
