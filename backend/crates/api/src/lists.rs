//! Lists, list items, and list-market sets.
//!
//! Input limits enforced on every endpoint that fans out to ESI:
//! - `multibuy` body: <= [`MAX_MULTIBUY_BYTES`] bytes
//! - parsed lines (post blank-strip):  <= [`MAX_PARSED_LINES`]
//! - distinct dedup'd type names:      <= [`MAX_DISTINCT_NAMES`]
//! - `market_ids`:                     <= [`MAX_MARKET_IDS`]
//!
//! Each `market_id` must point to either a public NPC hub or a citadel that
//! the calling group has explicitly tracked (enforced by
//! [`validate_markets_for_group`]).

mod crud;
mod detail;
mod items;
mod pricing;

pub(crate) use detail::load_list_detail;

use std::collections::HashSet;

use axum::{
    routing::{get, patch, post},
    Router,
};
use domain::{GroupRole, ListStatus, Market, MarketKind};
use uuid::Uuid;

use crate::{errors::ApiError, markets::MarketRow, state::AppState};

pub const MAX_MULTIBUY_BYTES: usize = 64 * 1024;
pub const MAX_PARSED_LINES: usize = 5000;
pub const MAX_DISTINCT_NAMES: usize = 1000;
pub const MAX_MARKET_IDS: usize = 8;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/lists/preview", post(crud::preview))
        .route(
            "/groups/{id}/lists",
            post(crud::create).get(crud::list_for_group),
        )
        .route(
            "/lists/{id}",
            get(crud::detail)
                .patch(crud::patch_list)
                .delete(crud::delete_list),
        )
        .route("/lists/{id}/markets", post(crud::replace_markets))
        .route("/lists/{id}/items", post(items::add_items))
        .route(
            "/lists/{id}/items/{item_id}",
            patch(items::patch_item).delete(items::delete_item),
        )
}

fn validate_multibuy_size(mb: &str) -> Result<(), ApiError> {
    if mb.len() > MAX_MULTIBUY_BYTES {
        return Err(ApiError::BadRequest(format!(
            "multibuy too large ({} bytes); max {}",
            mb.len(),
            MAX_MULTIBUY_BYTES
        )));
    }
    Ok(())
}

fn validate_market_ids_size(ids: &[Uuid]) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Err(ApiError::BadRequest("market_ids must not be empty".into()));
    }
    if ids.len() > MAX_MARKET_IDS {
        return Err(ApiError::BadRequest(format!(
            "too many market_ids ({}); max {}",
            ids.len(),
            MAX_MARKET_IDS
        )));
    }
    let dedup: HashSet<Uuid> = ids.iter().copied().collect();
    if dedup.len() != ids.len() {
        return Err(ApiError::BadRequest(
            "market_ids contains duplicates".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn load_markets(state: &AppState, ids: &[Uuid]) -> Result<Vec<Market>, ApiError> {
    let rows: Vec<MarketRow> = sqlx::query_as(
        "SELECT id, kind, esi_location_id, region_id, name, short_label, is_hub, is_public \
         FROM markets WHERE id = ANY($1::uuid[])",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;

    if rows.len() != ids.len() {
        return Err(ApiError::BadRequest(
            "one or more market_ids do not exist".into(),
        ));
    }
    Ok(rows.into_iter().map(MarketRow::into_market).collect())
}

/// Allow if every market is either an NPC hub or present in
/// `group_tracked_markets` for the given group.
pub(crate) async fn validate_markets_for_group(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    markets: &[Market],
) -> Result<(), ApiError> {
    if markets.is_empty() {
        return Ok(());
    }
    let citadel_ids: Vec<Uuid> = markets
        .iter()
        .filter(|m| m.kind == MarketKind::PublicStructure)
        .map(|m| m.id)
        .collect();

    let tracked: std::collections::HashSet<Uuid> = if citadel_ids.is_empty() {
        std::collections::HashSet::new()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT market_id FROM group_tracked_markets \
             WHERE group_id = $1 AND market_id = ANY($2::uuid[])",
        )
        .bind(group_id)
        .bind(&citadel_ids)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect()
    };

    for m in markets {
        let label = m.short_label.as_deref().unwrap_or("(unnamed)");
        if !m.is_public {
            return Err(ApiError::BadRequest(format!(
                "market {label} is not public"
            )));
        }
        match m.kind {
            MarketKind::NpcHub => {
                if !m.is_hub {
                    return Err(ApiError::BadRequest(format!(
                        "market {label} is marked as NPC hub but is_hub=false"
                    )));
                }
            }
            MarketKind::PublicStructure => {
                if !tracked.contains(&m.id) {
                    return Err(ApiError::BadRequest(format!(
                        "citadel '{label}' is not tracked by this group"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Apply a status transition to a list, checking that the caller is the
/// list's creator or a group owner. Extracted so tests can drive the archive
/// transition without going through the extractor stack.
pub async fn do_patch_list_status(
    pool: &sqlx::PgPool,
    list_id: Uuid,
    user_id: Uuid,
    new_status: ListStatus,
) -> Result<(), ApiError> {
    let row: Option<(Uuid, Option<GroupRole>)> = sqlx::query_as(
        "SELECT l.created_by_user_id, gm.role \
         FROM lists l \
         LEFT JOIN group_memberships gm \
           ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE l.id = $2",
    )
    .bind(user_id)
    .bind(list_id)
    .fetch_optional(pool)
    .await?;

    let (created_by, role) = row.ok_or_else(ApiError::not_found)?;
    let role = role.ok_or_else(ApiError::forbidden)?;
    if user_id != created_by && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    sqlx::query("UPDATE lists SET status = $1, updated_at = now() WHERE id = $2")
        .bind(new_status)
        .bind(list_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Enforce `limits.lists_per_group`. Counts non-archived lists; archive
/// is the existing escape hatch for groups that hit the ceiling.
pub async fn check_lists_quota(
    pool: &sqlx::PgPool,
    group_id: Uuid,
    cap: i64,
) -> Result<(), ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lists WHERE group_id = $1 AND status != 'archived'",
    )
    .bind(group_id)
    .fetch_one(pool)
    .await?;
    if count >= cap {
        return Err(ApiError::QuotaExceeded(format!(
            "this group already has {count} active lists (cap {cap}); archive some before adding more"
        )));
    }
    Ok(())
}

async fn require_lists_quota(state: &AppState, group_id: Uuid) -> Result<(), ApiError> {
    check_lists_quota(&state.pool, group_id, state.config.limits.lists_per_group).await
}
