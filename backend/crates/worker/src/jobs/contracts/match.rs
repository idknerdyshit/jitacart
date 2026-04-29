//! Contract → reimbursement matcher.
//!
//! For each candidate contract C:
//! - Pull pending reimbursements R where the hauler == C.issuer_user_id and
//!   the requester == C.assignee_user_id (one-to-many is allowed).
//! - Compare contract item quantities by type to the sum of (non-reversed)
//!   fulfillments under R.
//! - Score = Σ_t min(C[t], F[t]) / Σ_t F[t]; exact_match when all of F is
//!   covered with no surplus types in C.
//! - Insert/refresh a `contract_match_suggestions` row when score ≥ 0.5;
//!   never overwrite a row that the user already decided on.

use std::collections::HashMap;

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

const SCORE_THRESHOLD: f64 = 0.5;

pub async fn run_for_contracts(pool: &PgPool, contract_ids: &[Uuid]) -> anyhow::Result<()> {
    if contract_ids.is_empty() {
        return Ok(());
    }

    for cid in contract_ids {
        if let Err(e) = run_one(pool, *cid).await {
            tracing::warn!(error = ?e, contract_id = %cid, "matcher run_one failed");
        }
    }
    Ok(())
}

async fn run_one(pool: &PgPool, contract_id: Uuid) -> anyhow::Result<()> {
    #[allow(clippy::type_complexity)]
    let header: Option<(
        Option<Uuid>,
        Option<Uuid>,
        String,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT issuer_user_id, assignee_user_id, contract_type, items_synced_at \
             FROM contracts WHERE id = $1",
    )
    .bind(contract_id)
    .fetch_optional(pool)
    .await?;

    let (issuer_user_id, assignee_user_id, contract_type, items_synced_at) = match header {
        Some(h) => h,
        None => return Ok(()),
    };

    if contract_type != "item_exchange" || items_synced_at.is_none() {
        return Ok(());
    }
    let (Some(issuer), Some(assignee)) = (issuer_user_id, assignee_user_id) else {
        return Ok(());
    };

    // Contract item totals.
    let c_items: Vec<(i32, i64)> = sqlx::query_as(
        "SELECT type_id, quantity FROM contract_items \
         WHERE contract_id = $1 AND is_included = TRUE",
    )
    .bind(contract_id)
    .fetch_all(pool)
    .await?;
    let mut c_totals: HashMap<i32, i64> = HashMap::new();
    for (t, q) in c_items {
        *c_totals.entry(t).or_insert(0) += q;
    }
    if c_totals.is_empty() {
        return Ok(());
    }

    // One grouped query covers every candidate reimbursement and its
    // fulfillment totals by type. Reimbursements with no fulfillments don't
    // appear and are skipped (score_match returns None for empty f_totals).
    let rows: Vec<(Uuid, i32, i64)> = sqlx::query_as(
        r#"
        SELECT r.id, li.type_id::int, COALESCE(SUM(f.qty), 0)::bigint
        FROM reimbursements r
        JOIN list_items   li ON li.list_id = r.list_id
                            AND li.requested_by_user_id = r.requester_user_id
        JOIN fulfillments f  ON f.list_item_id = li.id
                            AND f.hauler_user_id = r.hauler_user_id
                            AND f.reversed_at IS NULL
        WHERE r.hauler_user_id = $1 AND r.requester_user_id = $2
          AND r.contract_id IS NULL AND r.status = 'pending'
        GROUP BY r.id, li.type_id
        "#,
    )
    .bind(issuer)
    .bind(assignee)
    .fetch_all(pool)
    .await?;

    let mut f_totals_by_reimb: HashMap<Uuid, HashMap<i32, i64>> = HashMap::new();
    for (reimb_id, type_id, qty) in rows {
        if qty > 0 {
            f_totals_by_reimb
                .entry(reimb_id)
                .or_default()
                .insert(type_id, qty);
        }
    }

    let mut reimb_ids: Vec<Uuid> = Vec::new();
    let mut scores: Vec<Decimal> = Vec::new();
    let mut exacts: Vec<bool> = Vec::new();
    for (reimb_id, f_totals) in f_totals_by_reimb {
        let Some((score, exact_match)) = score_match(&c_totals, &f_totals) else {
            continue;
        };
        if score < SCORE_THRESHOLD {
            continue;
        }
        reimb_ids.push(reimb_id);
        scores.push(
            Decimal::from_f64_retain(score)
                .unwrap_or(Decimal::ZERO)
                .round_dp(4),
        );
        exacts.push(exact_match);
    }

    if !reimb_ids.is_empty() {
        // ON CONFLICT update only when the existing row is still pending; if a
        // user already decided, leave it alone.
        sqlx::query(
            r#"
            INSERT INTO contract_match_suggestions (contract_id, reimbursement_id, score, exact_match)
            SELECT $1, * FROM UNNEST($2::uuid[], $3::numeric[], $4::bool[])
            ON CONFLICT (contract_id, reimbursement_id) DO UPDATE
                SET score = EXCLUDED.score,
                    exact_match = EXCLUDED.exact_match
                WHERE contract_match_suggestions.state = 'pending'
            "#,
        )
        .bind(contract_id)
        .bind(&reimb_ids)
        .bind(&scores)
        .bind(&exacts)
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Score a candidate (contract totals, fulfillment totals) pair.
///
/// Returns `(score, exact_match)` where `score = Σ min(C[t], F[t]) / Σ F[t]`,
/// and `exact_match` is true iff every fulfilled type is fully covered AND the
/// contract has no surplus types beyond what's fulfilled. Returns `None` when
/// the fulfillment side is empty (nothing to score against).
pub(crate) fn score_match(
    c_totals: &HashMap<i32, i64>,
    f_totals: &HashMap<i32, i64>,
) -> Option<(f64, bool)> {
    if f_totals.is_empty() {
        return None;
    }
    let f_sum: i64 = f_totals.values().sum();
    let covered: i64 = f_totals
        .iter()
        .map(|(t, fq)| c_totals.get(t).copied().unwrap_or(0).min(*fq))
        .sum();
    let surplus = c_totals.keys().any(|t| !f_totals.contains_key(t));
    Some((covered as f64 / f_sum as f64, covered == f_sum && !surplus))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(i32, i64)]) -> HashMap<i32, i64> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn exact_match_identical_maps() {
        let c = map(&[(34, 100), (35, 50)]);
        let f = map(&[(34, 100), (35, 50)]);
        let (score, exact) = score_match(&c, &f).unwrap();
        assert_eq!(score, 1.0);
        assert!(exact);
    }

    #[test]
    fn surplus_types_in_contract_full_coverage_not_exact() {
        let c = map(&[(34, 100), (35, 50), (36, 10)]);
        let f = map(&[(34, 100), (35, 50)]);
        let (score, exact) = score_match(&c, &f).unwrap();
        assert_eq!(score, 1.0);
        assert!(!exact);
    }

    #[test]
    fn missing_types_partial_coverage() {
        let c = map(&[(34, 100)]);
        let f = map(&[(34, 100), (35, 100)]);
        let (score, exact) = score_match(&c, &f).unwrap();
        assert_eq!(score, 0.5);
        assert!(!exact);
    }

    #[test]
    fn one_to_many_partial_quantity() {
        let c = map(&[(34, 50)]);
        let f = map(&[(34, 100)]);
        let (score, exact) = score_match(&c, &f).unwrap();
        assert_eq!(score, 0.5);
        assert!(!exact);
    }

    #[test]
    fn empty_fulfillments_returns_none() {
        let c = map(&[(34, 100)]);
        let f: HashMap<i32, i64> = HashMap::new();
        assert!(score_match(&c, &f).is_none());
    }

    #[test]
    fn surplus_quantity_in_contract_does_not_inflate() {
        // Contract has 200 of type 34; fulfillment has 100. min(200,100) = 100.
        let c = map(&[(34, 200)]);
        let f = map(&[(34, 100)]);
        let (score, exact) = score_match(&c, &f).unwrap();
        assert_eq!(score, 1.0);
        // No surplus types (only one type, present in both), so exact.
        assert!(exact);
    }
}
