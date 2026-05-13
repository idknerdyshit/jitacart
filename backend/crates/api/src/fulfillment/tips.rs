use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use domain::GroupRole;
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    db::Tx,
    errors::ApiError,
    extract::{CurrentGroup, CurrentList},
    lists::load_list_detail,
    state::AppState,
};

#[derive(Deserialize)]
pub(super) struct SetTipBody {
    pub(super) tip_pct: Decimal,
}

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: Uuid,
    name: String,
    invite_code: String,
    created_by_user_id: Uuid,
    created_at: DateTime<Utc>,
    default_tip_pct: Decimal,
}

impl GroupRow {
    fn into_group(self) -> domain::Group {
        domain::Group {
            id: self.id,
            name: self.name,
            invite_code: self.invite_code,
            created_by_user_id: self.created_by_user_id,
            created_at: self.created_at,
            default_tip_pct: self.default_tip_pct,
        }
    }
}

pub(super) async fn set_list_tip(
    State(state): State<AppState>,
    cur: CurrentList,
    tx: Tx,
    Json(body): Json<SetTipBody>,
) -> Result<Json<domain::ListDetail>, ApiError> {
    cur.require_mutable()?;
    let CurrentList {
        list_id,
        user_id,
        role,
        created_by_user_id,
        ..
    } = cur;
    super::validate_tip_pct(body.tip_pct)?;

    let is_creator = user_id == created_by_user_id;
    if !is_creator && role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }

    let mut conn = tx.acquire().await;
    super::lock_list(&mut **conn, list_id).await?;

    // Creator is locked out once any fulfillment exists; owner can always edit.
    // Read inside the tx so a concurrent fulfillment cannot slip past the check.
    if is_creator && role != GroupRole::Owner {
        let has_fulfillments: bool = sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM fulfillments f \
                JOIN list_items li ON li.id = f.list_item_id \
                WHERE li.list_id = $1 AND f.reversed_at IS NULL \
            )",
        )
        .bind(list_id)
        .fetch_one(&mut **conn)
        .await?;
        if has_fulfillments {
            return Err(ApiError::forbidden());
        }
    }

    sqlx::query("UPDATE lists SET tip_pct = $1 WHERE id = $2")
        .bind(body.tip_pct)
        .bind(list_id)
        .execute(&mut **conn)
        .await?;

    // Recompute all pending reimbursements for this list
    sqlx::query(
        "UPDATE reimbursements \
         SET tip_isk   = subtotal_isk * $1, \
             total_isk = subtotal_isk * (1 + $1), \
             updated_at = now() \
         WHERE list_id = $2 AND status = 'pending'",
    )
    .bind(body.tip_pct)
    .bind(list_id)
    .execute(&mut **conn)
    .await?;

    drop(conn);
    let detail = load_list_detail(&state, &tx, list_id, user_id, role).await?;
    Ok(Json(detail))
}

pub(super) async fn set_group_default_tip(
    State(_state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
    tx: Tx,
    Json(body): Json<SetTipBody>,
) -> Result<Json<domain::Group>, ApiError> {
    if role != GroupRole::Owner {
        return Err(ApiError::forbidden());
    }
    super::validate_tip_pct(body.tip_pct)?;

    let mut conn = tx.acquire().await;
    let row: Option<GroupRow> = sqlx::query_as(
        "UPDATE groups SET default_tip_pct = $1 WHERE id = $2 \
         RETURNING id, name, invite_code, created_by_user_id, created_at, default_tip_pct",
    )
    .bind(body.tip_pct)
    .bind(group_id)
    .fetch_optional(&mut **conn)
    .await?;

    let group = row.ok_or_else(ApiError::not_found)?.into_group();
    Ok(Json(group))
}
