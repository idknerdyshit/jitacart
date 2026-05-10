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
    /// Deprecated: NULL for corp contracts. Use `issuer_principal_id`.
    pub issuer_user_id: Option<Uuid>,
    pub assignee_character_id: Option<i64>,
    /// Deprecated: NULL for corp-assignee contracts. Use `assignee_principal_id`.
    pub assignee_user_id: Option<Uuid>,
    // Principal-id fields (may be None if resolution failed).
    pub issuer_principal_id: Option<Uuid>,
    pub assignee_principal_id: Option<Uuid>,
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
    /// Corp that discovered this contract via the corp contracts endpoint.
    /// None for character-discovered contracts.
    pub source_corp_id: Option<Uuid>,
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
                issuer_principal_id   = COALESCE($18, issuer_principal_id),
                assignee_principal_id = COALESCE($19, assignee_principal_id),
                source_corp_id        = COALESCE($20, source_corp_id),
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
        .bind(upsert.issuer_principal_id)
        .bind(upsert.assignee_principal_id)
        .bind(upsert.source_corp_id)
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
                issuer_principal_id, assignee_principal_id,
                contract_type, status,
                price_isk, reward_isk, collateral_isk,
                date_issued, date_expired, date_accepted, date_completed,
                start_location_id, end_location_id, raw_json, source_corp_id
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                $13, $14, $15, $16, $17, $18, $19, $20
            )
            RETURNING id
            "#,
        )
        .bind(upsert.esi_contract_id)
        .bind(upsert.issuer_character_id)
        .bind(upsert.issuer_user_id)
        .bind(upsert.assignee_character_id)
        .bind(upsert.assignee_user_id)
        .bind(upsert.issuer_principal_id)
        .bind(upsert.assignee_principal_id)
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
        .bind(upsert.source_corp_id)
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

/// Info about a reimbursement that was just settled via contract, for webhook
/// delivery by the caller after the transaction commits.
#[derive(Debug, Clone)]
pub struct ContractSettledReimbursement {
    pub group_id: Uuid,
    pub list_destination: String,
    pub requester_name: String,
    pub hauler_name: String,
    pub total_isk: Decimal,
}

/// Promote items that are `bought` *or* `delivered` to `settled` for every
/// reimbursement bound to `contract_id`, mark those reimbursements settled,
/// and write the aggregate delta. The contract's `date_completed` is used as
/// the settlement timestamp; `settled_by_user_id` is left NULL because no
/// human pressed the button.
pub async fn settle_via_contract(
    tx: &mut Transaction<'_, Postgres>,
    contract_id: Uuid,
) -> Result<Vec<ContractSettledReimbursement>, SettlementError> {
    let date_completed: Option<Option<DateTime<Utc>>> =
        sqlx::query_scalar("SELECT date_completed FROM contracts WHERE id = $1 FOR UPDATE")
            .bind(contract_id)
            .fetch_optional(&mut **tx)
            .await?;
    let date_completed = date_completed.ok_or(SettlementError::NotFound)?;

    let settled_at = date_completed.unwrap_or_else(Utc::now);

    let settled_rows: Vec<(Uuid, Uuid, Option<Uuid>, Uuid, Decimal)> = sqlx::query_as(
        "UPDATE reimbursements \
         SET status = 'settled', settled_at = $1, settled_by_user_id = NULL, updated_at = now() \
         WHERE contract_id = $2 AND status = 'pending' \
         RETURNING id, list_id, requester_user_id, hauler_user_id, total_isk",
    )
    .bind(settled_at)
    .bind(contract_id)
    .fetch_all(&mut **tx)
    .await?;

    if settled_rows.is_empty() {
        return Ok(vec![]);
    }

    // Bulk-flip every list_item that any reimbursement bound to this contract
    // covers. Mode-aware:
    // - Personal (is_corp_funded=false): filter by requested_by_user_id = requester.
    // - Corp-funded (is_corp_funded=true): drop that filter (all items on the list).
    sqlx::query(
        r#"
        UPDATE list_items li
        SET status = 'settled'
        FROM reimbursements r
        LEFT JOIN principals rp ON rp.id = r.requester_principal_id AND rp.kind = 'user'
        WHERE r.contract_id = $1
          AND li.list_id = r.list_id
          AND (
              r.is_corp_funded = true
              OR li.requested_by_user_id = rp.user_id
          )
          AND li.status IN ('bought', 'delivered')
          AND EXISTS (
              SELECT 1 FROM fulfillments f
              JOIN principals hp ON hp.id = r.hauler_principal_id AND hp.kind = 'user'
              WHERE f.list_item_id = li.id
                AND f.hauler_user_id = hp.user_id
                AND f.reversed_at IS NULL
          )
          AND NOT EXISTS (
              SELECT 1 FROM reimbursements r2
              JOIN principals hp2 ON hp2.id = r2.hauler_principal_id AND hp2.kind = 'user'
              JOIN fulfillments f2
                ON f2.list_item_id = li.id
               AND f2.hauler_user_id = hp2.user_id
               AND f2.reversed_at IS NULL
              WHERE r2.list_id = r.list_id
                AND r2.hauler_principal_id <> r.hauler_principal_id
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

    let list_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT list_id FROM reimbursements WHERE contract_id = $1")
            .bind(contract_id)
            .fetch_all(&mut **tx)
            .await?;

    for lid in &list_ids {
        auto_close_list_if_complete(tx, *lid).await?;
    }

    let reimb_ids: Vec<Uuid> = settled_rows.iter().map(|r| r.0).collect();
    let webhook_info: Vec<ContractSettledReimbursement> = sqlx::query_as::<
        _,
        (Uuid, Option<String>, Option<String>, String, Decimal),
    >(
        "SELECT l.group_id, l.destination_label, ureq.display_name, uh.display_name, r.total_isk \
         FROM reimbursements r \
         JOIN lists l ON l.id = r.list_id \
         LEFT JOIN users ureq ON ureq.id = r.requester_user_id \
         JOIN users uh ON uh.id = r.hauler_user_id \
         WHERE r.id = ANY($1)",
    )
    .bind(&reimb_ids)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(
        |(group_id, dest, req_name, hauler_name, total_isk)| ContractSettledReimbursement {
            group_id,
            list_destination: dest.unwrap_or_else(|| "(unnamed)".into()),
            requester_name: req_name.unwrap_or_else(|| "Corp".into()),
            hauler_name,
            total_isk,
        },
    )
    .collect();

    Ok(webhook_info)
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
    let row: Option<(Uuid, Option<Uuid>, Uuid, String, bool)> = sqlx::query_as(
        "SELECT list_id, requester_user_id, hauler_user_id, status, is_corp_funded \
         FROM reimbursements WHERE id = $1 FOR UPDATE",
    )
    .bind(reimbursement_id)
    .fetch_optional(&mut **tx)
    .await?;

    let (list_id, requester_user_id, hauler_user_id, status, is_corp_funded) =
        row.ok_or(SettlementError::NotFound)?;

    if status != "pending" {
        return Err(SettlementError::NotPending(status));
    }

    // Mode-aware: corp-funded reimbursements cover all items on the list.
    let not_delivered: i64 = if is_corp_funded {
        sqlx::query_scalar(
            r#"
            SELECT COUNT(DISTINCT li.id)
            FROM list_items li
            WHERE li.list_id = $1
              AND li.status NOT IN ('delivered', 'settled')
              AND EXISTS (
                  SELECT 1 FROM fulfillments f
                  WHERE f.list_item_id = li.id
                    AND f.hauler_user_id = $2
                    AND f.reversed_at IS NULL
              )
            "#,
        )
        .bind(list_id)
        .bind(hauler_user_id)
        .fetch_one(&mut **tx)
        .await?
    } else {
        let req_uid = requester_user_id.ok_or(SettlementError::NotFound)?;
        sqlx::query_scalar(
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
        .bind(req_uid)
        .bind(hauler_user_id)
        .fetch_one(&mut **tx)
        .await?
    };

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

    if is_corp_funded {
        // Corp-funded: flip all hauler-fulfilled items on the list, but only
        // when no other hauler still has a pending reimbursement on the same
        // item (mirrors the guard in flip_items_to_settled / settle_via_contract).
        sqlx::query(
            r#"
            UPDATE list_items li
            SET status = 'settled'
            WHERE li.list_id = $1
              AND li.status = 'delivered'
              AND EXISTS (
                  SELECT 1 FROM fulfillments f
                  WHERE f.list_item_id = li.id
                    AND f.hauler_user_id = $2
                    AND f.reversed_at IS NULL
              )
              AND NOT EXISTS (
                  SELECT 1 FROM reimbursements r2
                  JOIN fulfillments f2
                    ON f2.list_item_id = li.id
                   AND f2.hauler_user_id = r2.hauler_user_id
                   AND f2.reversed_at IS NULL
                  WHERE r2.list_id = $1
                    AND r2.hauler_user_id <> $2
                    AND r2.status = 'pending'
              )
            "#,
        )
        .bind(list_id)
        .bind(hauler_user_id)
        .execute(&mut **tx)
        .await?;
    } else {
        let req_uid = requester_user_id.ok_or(SettlementError::NotFound)?;
        flip_items_to_settled(tx, list_id, req_uid, hauler_user_id, false).await?;
    }

    auto_close_list_if_complete(tx, list_id).await?;

    Ok(())
}

/// Wallet verification (audit-only).
///
/// Looks up the ESI contract id, sums `corp_wallet_journal.amount` for
/// `ref_type = 'contract_price'` entries that reference this contract, then:
/// - Sets `contracts.wallet_verified_at` and `wallet_payout_aggregate_isk`.
/// - For each reimbursement bound to this contract, sets `verified_by_wallet =
///   true` and computes a per-reimbursement `wallet_settlement_delta_isk`.
///
/// If no journal rows match (not yet visible), does nothing — no error.
pub async fn verify_contract_against_journal(
    pool: &sqlx::PgPool,
    contract_id: Uuid,
) -> Result<(), SettlementError> {
    // Fetch esi_contract_id and price_isk for this contract.
    let row: Option<(i64, Decimal)> =
        sqlx::query_as("SELECT esi_contract_id, price_isk FROM contracts WHERE id = $1")
            .bind(contract_id)
            .fetch_optional(pool)
            .await?;

    let (esi_contract_id, _price_isk) = match row {
        Some(r) => r,
        None => return Ok(()), // Unknown contract.
    };

    // Resolve the payer corp and division from the list linked via
    // reimbursements so we only count journal entries from the correct wallet.
    let payer: Option<(Uuid, i16)> = sqlx::query_as(
        "SELECT l.payer_corp_id, l.payer_division \
         FROM reimbursements r \
         JOIN lists l ON l.id = r.list_id \
         WHERE r.contract_id = $1 \
           AND l.payer_corp_id IS NOT NULL \
         LIMIT 1",
    )
    .bind(contract_id)
    .fetch_optional(pool)
    .await?;

    // Sum journal entries for this ESI contract_id (contract_price ref_type only),
    // scoped to the payer corp + division when known, otherwise to the discovering
    // corp to prevent double-counting when both sides' wallets are linked.
    let payout_sum: Option<Decimal> = match payer {
        Some((corp_id, division)) => {
            sqlx::query_scalar(
                r#"
                SELECT SUM(ABS(amount))
                FROM corp_wallet_journal
                WHERE context_id = $1
                  AND context_id_type = 'contract_id'
                  AND ref_type = 'contract_price'
                  AND corp_id = $2
                  AND division = $3
                "#,
            )
            .bind(esi_contract_id)
            .bind(corp_id)
            .bind(division)
            .fetch_one(pool)
            .await?
        }
        None => {
            sqlx::query_scalar(
                r#"
                SELECT SUM(ABS(j.amount))
                FROM corp_wallet_journal j
                JOIN contracts c ON c.esi_contract_id = j.context_id
                WHERE j.context_id = $1
                  AND j.context_id_type = 'contract_id'
                  AND j.ref_type = 'contract_price'
                  AND j.corp_id = COALESCE(c.source_corp_id, j.corp_id)
                "#,
            )
            .bind(esi_contract_id)
            .fetch_one(pool)
            .await?
        }
    };

    let payout_aggregate = match payout_sum {
        Some(v) if v > Decimal::ZERO => v,
        _ => return Ok(()), // No journal rows yet; leave unverified.
    };

    // Stamp the contract.
    sqlx::query(
        "UPDATE contracts \
         SET wallet_verified_at = now(), \
             wallet_payout_aggregate_isk = $2, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(contract_id)
    .bind(payout_aggregate)
    .execute(pool)
    .await?;

    // Fetch all pending/settled reimbursements bound to this contract.
    let reimbs: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT id, total_isk FROM reimbursements \
         WHERE contract_id = $1 AND status IN ('pending','settled')",
    )
    .bind(contract_id)
    .fetch_all(pool)
    .await?;

    if reimbs.is_empty() {
        return Ok(());
    }

    let total_reimb_sum: Decimal = reimbs.iter().map(|(_, t)| *t).sum();

    for (reimb_id, total_isk) in reimbs {
        let share = if total_reimb_sum > Decimal::ZERO {
            payout_aggregate * total_isk / total_reimb_sum
        } else {
            Decimal::ZERO
        };
        let share = share.round_dp(2);
        let delta = share - total_isk;

        sqlx::query(
            "UPDATE reimbursements \
             SET verified_by_wallet = true, \
                 wallet_settlement_delta_isk = $2, \
                 updated_at = now() \
             WHERE id = $1",
        )
        .bind(reimb_id)
        .bind(delta)
        .execute(pool)
        .await?;
    }

    Ok(())
}

pub async fn auto_close_list_if_complete(
    tx: &mut Transaction<'_, Postgres>,
    list_id: Uuid,
) -> Result<bool, SettlementError> {
    let rows = sqlx::query(
        "UPDATE lists SET status = 'closed', updated_at = now() \
         WHERE id = $1 AND status = 'open' \
           AND NOT EXISTS( \
               SELECT 1 FROM list_items WHERE list_id = $1 AND status <> 'settled' \
           )",
    )
    .bind(list_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    Ok(rows > 0)
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
