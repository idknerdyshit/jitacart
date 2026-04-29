//! Settlement helpers shared between the API (manual settle) and the worker
//! (contract-driven settle / unbind). All functions take a `&mut Transaction`
//! and assume the caller already holds whatever locks they need.
//!
//! The two flavours of settle differ in their precondition:
//!
//! - [`settle_manual`] requires items to be in `delivered` status (the
//!   requester has acknowledged hand-off).
//! - [`settle_via_contract`] accepts items in `bought` *or* `delivered`,
//!   because the contract finishing in EVE is itself proof of delivery.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SettlementError {
    #[error("settlement target not found")]
    NotFound,
    #[error("not in pending state: {0}")]
    NotPending(String),
    #[error("items still owe delivery: {count}")]
    NotDelivered { count: i64 },
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Inputs for [`upsert_contract`]. Translated from the upstream ESI payload
/// at the worker's call-site so this crate stays free of nea-esi types.
#[derive(Debug, Clone)]
pub struct ContractUpsert {
    pub esi_contract_id: i64,
    pub issuer_character_id: i64,
    pub issuer_user_id: Option<Uuid>,
    pub assignee_character_id: Option<i64>,
    pub assignee_user_id: Option<Uuid>,
    pub contract_type: String,
    pub status: String,
    pub price_isk: Decimal,
    pub reward_isk: Decimal,
    pub collateral_isk: Decimal,
    pub date_issued: DateTime<Utc>,
    pub date_expired: Option<DateTime<Utc>>,
    pub date_accepted: Option<DateTime<Utc>>,
    pub date_completed: Option<DateTime<Utc>>,
    pub start_location_id: Option<i64>,
    pub end_location_id: Option<i64>,
    pub raw_json: Value,
}

#[derive(Debug, Clone)]
pub struct ContractUpsertOutcome {
    pub contract_id: Uuid,
    /// Previous status row before the upsert, if the contract was already known.
    pub prior_status: Option<String>,
    /// Status persisted after the upsert.
    pub current_status: String,
    /// Items have not yet been fetched into `contract_items`.
    pub needs_items: bool,
}

impl ContractUpsertOutcome {
    pub fn status_changed(&self) -> bool {
        self.prior_status.as_deref() != Some(self.current_status.as_str())
    }
}

pub async fn upsert_contract(
    tx: &mut Transaction<'_, Postgres>,
    upsert: &ContractUpsert,
) -> Result<ContractUpsertOutcome, SettlementError> {
    let prior: Option<(Uuid, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, status, items_synced_at FROM contracts \
         WHERE esi_contract_id = $1 FOR UPDATE",
    )
    .bind(upsert.esi_contract_id)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some((id, prior_status, items_synced_at)) = prior {
        sqlx::query(
            r#"
            UPDATE contracts SET
                issuer_character_id   = $2,
                issuer_user_id        = $3,
                assignee_character_id = $4,
                assignee_user_id      = $5,
                contract_type         = $6,
                status                = $7,
                price_isk             = $8,
                reward_isk            = $9,
                collateral_isk        = $10,
                date_issued           = $11,
                date_expired          = $12,
                date_accepted         = $13,
                date_completed        = $14,
                start_location_id     = $15,
                end_location_id       = $16,
                raw_json              = $17,
                updated_at            = now()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(upsert.issuer_character_id)
        .bind(upsert.issuer_user_id)
        .bind(upsert.assignee_character_id)
        .bind(upsert.assignee_user_id)
        .bind(&upsert.contract_type)
        .bind(&upsert.status)
        .bind(upsert.price_isk)
        .bind(upsert.reward_isk)
        .bind(upsert.collateral_isk)
        .bind(upsert.date_issued)
        .bind(upsert.date_expired)
        .bind(upsert.date_accepted)
        .bind(upsert.date_completed)
        .bind(upsert.start_location_id)
        .bind(upsert.end_location_id)
        .bind(&upsert.raw_json)
        .execute(&mut **tx)
        .await?;

        Ok(ContractUpsertOutcome {
            contract_id: id,
            prior_status: Some(prior_status),
            current_status: upsert.status.clone(),
            needs_items: items_synced_at.is_none(),
        })
    } else {
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO contracts (
                esi_contract_id, issuer_character_id, issuer_user_id,
                assignee_character_id, assignee_user_id,
                contract_type, status,
                price_isk, reward_isk, collateral_isk,
                date_issued, date_expired, date_accepted, date_completed,
                start_location_id, end_location_id, raw_json
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17
            )
            RETURNING id
            "#,
        )
        .bind(upsert.esi_contract_id)
        .bind(upsert.issuer_character_id)
        .bind(upsert.issuer_user_id)
        .bind(upsert.assignee_character_id)
        .bind(upsert.assignee_user_id)
        .bind(&upsert.contract_type)
        .bind(&upsert.status)
        .bind(upsert.price_isk)
        .bind(upsert.reward_isk)
        .bind(upsert.collateral_isk)
        .bind(upsert.date_issued)
        .bind(upsert.date_expired)
        .bind(upsert.date_accepted)
        .bind(upsert.date_completed)
        .bind(upsert.start_location_id)
        .bind(upsert.end_location_id)
        .bind(&upsert.raw_json)
        .fetch_one(&mut **tx)
        .await?;

        Ok(ContractUpsertOutcome {
            contract_id: id,
            prior_status: None,
            current_status: upsert.status.clone(),
            needs_items: true,
        })
    }
}

/// Refresh `contracts.expected_total_isk` from the bound reimbursements'
/// `subtotal_isk + tip_isk`. SUM over zero rows is NULL, which is what we want:
/// "nothing bound" is distinct from a zero-cost contract.
pub async fn recompute_contract_expected_total(
    tx: &mut Transaction<'_, Postgres>,
    contract_id: Uuid,
) -> Result<(), SettlementError> {
    sqlx::query(
        r#"
        UPDATE contracts
        SET expected_total_isk = (
            SELECT SUM(subtotal_isk + tip_isk)
            FROM reimbursements WHERE contract_id = $1
        )
        WHERE id = $1
        "#,
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Promote items that are `bought` *or* `delivered` to `settled` for every
/// reimbursement bound to `contract_id`, mark those reimbursements settled,
/// and write the aggregate delta. The contract's `date_completed` is used as
/// the settlement timestamp; `settled_by_user_id` is left NULL because no
/// human pressed the button.
pub async fn settle_via_contract(
    tx: &mut Transaction<'_, Postgres>,
    contract_id: Uuid,
) -> Result<usize, SettlementError> {
    let date_completed: Option<Option<DateTime<Utc>>> =
        sqlx::query_scalar("SELECT date_completed FROM contracts WHERE id = $1 FOR UPDATE")
            .bind(contract_id)
            .fetch_optional(&mut **tx)
            .await?;
    let date_completed = date_completed.ok_or(SettlementError::NotFound)?;

    let settled_at = date_completed.unwrap_or_else(Utc::now);

    let settled = sqlx::query(
        "UPDATE reimbursements \
         SET status = 'settled', settled_at = $1, settled_by_user_id = NULL, updated_at = now() \
         WHERE contract_id = $2 AND status = 'pending'",
    )
    .bind(settled_at)
    .bind(contract_id)
    .execute(&mut **tx)
    .await?
    .rows_affected() as usize;

    if settled == 0 {
        return Ok(0);
    }

    // Bulk-flip every list_item that any reimbursement bound to this contract
    // covers. Mirrors flip_items_to_settled's per-triple guard but lifted into
    // a single set-based UPDATE joining the contract's reimbursements.
    sqlx::query(
        r#"
        UPDATE list_items li
        SET status = 'settled'
        FROM reimbursements r
        WHERE r.contract_id = $1
          AND li.list_id = r.list_id
          AND li.requested_by_user_id = r.requester_user_id
          AND li.status IN ('bought', 'delivered')
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = r.hauler_user_id
                AND f.reversed_at IS NULL
          )
          AND NOT EXISTS (
              SELECT 1 FROM reimbursements r2
              JOIN fulfillments f2
                ON f2.list_item_id = li.id
               AND f2.hauler_user_id = r2.hauler_user_id
               AND f2.reversed_at IS NULL
              WHERE r2.list_id = r.list_id
                AND r2.requester_user_id = r.requester_user_id
                AND r2.hauler_user_id <> r.hauler_user_id
                AND r2.status = 'pending'
          )
        "#,
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE contracts
        SET settlement_delta_isk = price_isk - COALESCE(expected_total_isk, 0),
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?;

    Ok(settled)
}

/// Promote items to `settled` for a `(list, requester, hauler)` triple,
/// guarding against premature settlement when another pending reimbursement
/// from a different hauler still covers the same item. With `accept_bought`,
/// items in `bought` status are also accepted (contract-finishing acts as
/// delivery confirmation); otherwise only `delivered` items qualify.
async fn flip_items_to_settled(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
    requester_user_id: Uuid,
    hauler_user_id: Uuid,
    accept_bought: bool,
) -> Result<(), SettlementError> {
    let status_filter = if accept_bought {
        "li.status IN ('bought', 'delivered')"
    } else {
        "li.status = 'delivered'"
    };
    let sql = format!(
        r#"
        UPDATE list_items li
        SET status = 'settled'
        WHERE li.list_id = $1
          AND li.requested_by_user_id = $2
          AND {status_filter}
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = $3
                AND f.reversed_at IS NULL
          )
          AND NOT EXISTS (
              SELECT 1 FROM reimbursements r
              JOIN fulfillments f2
                ON f2.list_item_id = li.id
               AND f2.hauler_user_id = r.hauler_user_id
               AND f2.reversed_at IS NULL
              WHERE r.list_id = $1
                AND r.requester_user_id = $2
                AND r.hauler_user_id <> $3
                AND r.status = 'pending'
          )
        "#
    );
    sqlx::query(&sql)
        .bind(list_id)
        .bind(requester_user_id)
        .bind(hauler_user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Manual-path settle. Mirrors the original [`crate::api::fulfillment`] logic:
/// items must be in `delivered` status (the requester confirmed hand-off).
pub async fn settle_manual(
    tx: &mut Transaction<'_, Postgres>,
    reimbursement_id: Uuid,
    settled_by_user_id: Uuid,
) -> Result<(), SettlementError> {
    let row: Option<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT list_id, requester_user_id, hauler_user_id, status \
         FROM reimbursements WHERE id = $1 FOR UPDATE",
    )
    .bind(reimbursement_id)
    .fetch_optional(&mut **tx)
    .await?;

    let (list_id, requester_user_id, hauler_user_id, status) =
        row.ok_or(SettlementError::NotFound)?;

    if status != "pending" {
        return Err(SettlementError::NotPending(status));
    }

    let not_delivered: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(DISTINCT li.id)
        FROM list_items li
        WHERE li.list_id = $1
          AND li.requested_by_user_id = $2
          AND li.status NOT IN ('delivered', 'settled')
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = $3
                AND f.reversed_at IS NULL
          )
        "#,
    )
    .bind(list_id)
    .bind(requester_user_id)
    .bind(hauler_user_id)
    .fetch_one(&mut **tx)
    .await?;

    if not_delivered > 0 {
        return Err(SettlementError::NotDelivered {
            count: not_delivered,
        });
    }

    sqlx::query(
        "UPDATE reimbursements \
         SET status = 'settled', settled_at = now(), settled_by_user_id = $1, updated_at = now() \
         WHERE id = $2",
    )
    .bind(settled_by_user_id)
    .bind(reimbursement_id)
    .execute(&mut **tx)
    .await?;

    flip_items_to_settled(tx, list_id, requester_user_id, hauler_user_id, false).await?;

    Ok(())
}

/// Detach all reimbursements bound to a failed/cancelled contract, returning
/// them to `pending` (and `contract_id = NULL`). Pending suggestions for the
/// contract are marked `superseded` so they don't reappear in the tray.
pub async fn unbind_contract(
    tx: &mut Transaction<'_, Postgres>,
    contract_id: Uuid,
) -> Result<usize, SettlementError> {
    let unbound = sqlx::query(
        "UPDATE reimbursements \
         SET contract_id = NULL, updated_at = now() \
         WHERE contract_id = $1 AND status = 'pending'",
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?
    .rows_affected() as usize;

    sqlx::query(
        "UPDATE contract_match_suggestions \
         SET state = 'superseded', decided_at = now() \
         WHERE contract_id = $1 AND state = 'pending'",
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE contracts \
         SET expected_total_isk = NULL, updated_at = now() \
         WHERE id = $1",
    )
    .bind(contract_id)
    .execute(&mut **tx)
    .await?;

    Ok(unbound)
}
