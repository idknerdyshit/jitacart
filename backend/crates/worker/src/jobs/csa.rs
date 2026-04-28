//! Helpers shared by the citadel_details and citadel_orders jobs.
//!
//! Both jobs walk `character_structure_access` along two parallel dimensions:
//! one for ESI universe details, one for market orders. They use the same
//! candidate-selection ranking and the same upsert shape.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Copy, Clone)]
pub(crate) enum AccessDimension {
    Details,
    Market,
}

impl AccessDimension {
    fn columns(self) -> (&'static str, &'static str) {
        match self {
            Self::Details => ("details_status", "details_checked_at"),
            Self::Market => ("market_status", "market_checked_at"),
        }
    }
}

pub(crate) async fn upsert_access(
    pool: &PgPool,
    character_id: Uuid,
    market_id: Uuid,
    status: &str,
    dim: AccessDimension,
) -> anyhow::Result<()> {
    let (status_col, ts_col) = dim.columns();
    let sql = format!(
        r#"
        INSERT INTO character_structure_access (
            character_id, market_id, {status_col}, {ts_col}
        ) VALUES ($1, $2, $3::structure_access_status, now())
        ON CONFLICT (character_id, market_id) DO UPDATE SET
            {status_col} = EXCLUDED.{status_col},
            {ts_col}     = EXCLUDED.{ts_col}
        "#
    );
    sqlx::query(&sql)
        .bind(character_id)
        .bind(market_id)
        .bind(status)
        .execute(pool)
        .await?;
    Ok(())
}

/// Pick up to 5 candidate characters for `market_id`, preferring `'ok'` >
/// `'unknown'` rows; old `'forbidden'` rows are retried after `backoff_secs`.
/// When `require_group_tracking` is true, restrict to characters that belong
/// to a group which has tracked the market — used by citadel_orders so the
/// per-group access proof actually matches the citadel being refreshed.
pub(crate) async fn select_candidates(
    pool: &PgPool,
    market_id: Uuid,
    scope: &str,
    dim: AccessDimension,
    backoff_secs: i64,
    require_group_tracking: bool,
) -> sqlx::Result<Vec<Uuid>> {
    let (status_col, ts_col) = dim.columns();
    let group_joins = if require_group_tracking {
        "JOIN group_memberships gm        ON gm.user_id = c.user_id \
         JOIN group_tracked_markets gtm   ON gtm.group_id = gm.group_id AND gtm.market_id = $1"
    } else {
        ""
    };
    let sql = format!(
        r#"
        SELECT id FROM (
            SELECT DISTINCT
                   c.id,
                   CASE COALESCE(csa.{status_col}, 'unknown')
                     WHEN 'ok' THEN 0 WHEN 'unknown' THEN 1 ELSE 2 END AS status_rank,
                   csa.{ts_col} AS checked_at
            FROM characters c
            {group_joins}
            LEFT JOIN character_structure_access csa
                   ON csa.character_id = c.id AND csa.market_id = $1
            WHERE c.scopes @> ARRAY[$3]
              AND (
                  COALESCE(csa.{status_col}, 'unknown') <> 'forbidden'
                  OR csa.{ts_col} IS NULL
                  OR csa.{ts_col} < now() - make_interval(secs => $2::double precision)
              )
        ) ranked
        ORDER BY status_rank, checked_at NULLS FIRST, id
        LIMIT 5
        "#
    );
    sqlx::query_scalar(&sql)
        .bind(market_id)
        .bind(backoff_secs as f64)
        .bind(scope)
        .fetch_all(pool)
        .await
}
