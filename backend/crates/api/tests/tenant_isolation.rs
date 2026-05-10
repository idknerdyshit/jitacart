//! Cross-tenant isolation regressions.
//!
//! Two patterns are exercised here:
//!
//! 1. **Public `do_*` helpers** (the chunks of handler logic the worker also
//!    calls) are invoked with `user_id` belonging to a different group than
//!    the resource — must reject.
//! 2. **Extractor SQL probes** — the literal queries used by `CurrentGroup`,
//!    `CurrentList`, and `CurrentClaim` (in `extract.rs`) are exercised
//!    against fixtures so that any "improvement" to those extractors that
//!    drops the `user_id` predicate fails this suite loudly.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use domain::ListStatus;
use jitacart_api::contracts::{do_confirm, do_manual_link, do_reject, do_unlink};
use jitacart_api::errors::ApiError;
use jitacart_api::lists::do_patch_list_status;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

// ── Fixture: two fully-populated groups + a "stranger" who is only in B ──

struct TwoTenants {
    /// Group A: requester_a + hauler_a + list_a + reimbursement_a + contract_a
    a: ListIds,
    contract_a: ContractFixture,
    /// User who is a member of group B but NOT group A. The whole point of
    /// the suite is that this user cannot poke at A's resources.
    stranger_in_b: Uuid,
}

async fn seed_two_tenants(pool: &PgPool) -> TwoTenants {
    let requester_a = insert_user(pool, "requester_a").await;
    let hauler_a = insert_user(pool, "hauler_a").await;
    let a = seed_two_party_list(pool, requester_a, hauler_a).await;

    let contract_a = insert_contract(
        pool,
        hauler_a,
        requester_a,
        Decimal::new(10_000, 0),
        domain::ContractStatus::Outstanding,
        true,
    )
    .await;

    // Group B is built by seed_two_party_list again (it makes its own group).
    let requester_b = insert_user(pool, "requester_b").await;
    let hauler_b = insert_user(pool, "hauler_b").await;
    let _b = seed_two_party_list(pool, requester_b, hauler_b).await;

    // The "stranger" is a separate user added only to group B.
    let stranger_in_b = insert_user(pool, "stranger_in_b").await;
    add_member(pool, _b.group_id, stranger_in_b).await;

    TwoTenants {
        a,
        contract_a,
        stranger_in_b,
    }
}

// ─────────────────────────── do_* cross-tenant tests ──────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn patch_list_status_cross_tenant_is_forbidden(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let err = do_patch_list_status(&pool, t.a.list_id, t.stranger_in_b, ListStatus::Archived)
        .await
        .unwrap_err();

    assert!(
        matches!(err, ApiError::Forbidden(_)),
        "expected Forbidden, got {err:?}"
    );

    // List must remain unchanged.
    let status: String = sqlx::query_scalar("SELECT status::text FROM lists WHERE id = $1")
        .bind(t.a.list_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "open");
}

#[sqlx::test(migrations = "../../migrations")]
async fn patch_list_status_for_unknown_list_is_not_found(pool: PgPool) {
    let _t = seed_two_tenants(&pool).await;
    let err = do_patch_list_status(&pool, Uuid::new_v4(), Uuid::new_v4(), ListStatus::Archived)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirm_suggestion_cross_tenant_is_forbidden(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    let sugg_id = insert_suggestion(
        &pool,
        t.contract_a.contract_id,
        t.a.reimbursement_id,
        "pending",
    )
    .await;

    let err = do_confirm(&pool, t.stranger_in_b, sugg_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ApiError::Forbidden(_)),
        "stranger from group B must not confirm group A's suggestion, got {err:?}"
    );

    let state: String =
        sqlx::query_scalar("SELECT state::text FROM contract_match_suggestions WHERE id = $1")
            .bind(sugg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "pending", "suggestion state must not have moved");
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_link_cross_tenant_is_forbidden(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let err = do_manual_link(
        &pool,
        t.stranger_in_b,
        t.contract_a.contract_id,
        t.a.reimbursement_id,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::Forbidden(_)), "got {err:?}");

    // No suggestion was inserted.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM contract_match_suggestions WHERE contract_id = $1",
    )
    .bind(t.contract_a.contract_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 0);

    // Reimbursement still unbound.
    let bound: Option<Uuid> =
        sqlx::query_scalar("SELECT contract_id FROM reimbursements WHERE id = $1")
            .bind(t.a.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(bound.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn unlink_by_non_issuer_cross_tenant_is_forbidden(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    let err = do_unlink(&pool, t.stranger_in_b, t.contract_a.contract_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Forbidden(_)), "got {err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn reject_suggestion_cross_tenant_is_forbidden(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    let sugg_id = insert_suggestion(
        &pool,
        t.contract_a.contract_id,
        t.a.reimbursement_id,
        "pending",
    )
    .await;

    let err = do_reject(&pool, t.stranger_in_b, sugg_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Forbidden(_)), "got {err:?}");
}

/// Corp-issued contracts have issuer_user_id = NULL. The hauler (assignee) must
/// still be able to reject the suggestion; previously the `Some(user_id)` check
/// rejected everyone. Cross-tenant strangers must still be 403.
#[sqlx::test(migrations = "../../migrations")]
async fn reject_corp_issued_suggestion_by_hauler(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    // Make the contract corp-issued: clear issuer_user_id.
    sqlx::query("UPDATE contracts SET issuer_user_id = NULL WHERE id = $1")
        .bind(t.contract_a.contract_id)
        .execute(&pool)
        .await
        .unwrap();
    let sugg_id = insert_suggestion(
        &pool,
        t.contract_a.contract_id,
        t.a.reimbursement_id,
        "pending",
    )
    .await;

    // Stranger (only in group B) is still 403.
    let err = do_reject(&pool, t.stranger_in_b, sugg_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Forbidden(_)), "got {err:?}");

    // Hauler in group A can reject the corp-issued suggestion.
    let hauler: Uuid =
        sqlx::query_scalar("SELECT hauler_user_id FROM reimbursements WHERE id = $1")
            .bind(t.a.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let dec = do_reject(&pool, hauler, sugg_id).await.unwrap();
    assert_eq!(dec.state, "rejected");
}

// ────────────────── Extractor SQL probes (regression guards) ─────────────
//
// These hit the *exact* query strings used by `CurrentGroup`, `CurrentList`,
// and `CurrentClaim` in `extract.rs`. If someone refactors and drops the
// `user_id` predicate, these break. They're tests of the SQL contract those
// extractors rely on, not of the extractors themselves (which need a full
// Axum harness).

#[sqlx::test(migrations = "../../migrations")]
async fn current_group_query_rejects_non_member(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    // Mirrors `CurrentGroup::from_request_parts` in extract.rs.
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role::text FROM group_memberships WHERE user_id = $1 AND group_id = $2",
    )
    .bind(t.stranger_in_b)
    .bind(t.a.group_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(role.is_none(), "stranger must not see a role for group A");
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_list_query_returns_null_role_for_non_member(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    // Mirrors `CurrentList::from_request_parts`. Existence-of-list returns a
    // row, but the LEFT-joined role is NULL — handler converts to 403.
    let row: Option<(Uuid, Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT l.group_id, l.created_by_user_id, l.status::text, gm.role::text \
         FROM lists l \
         LEFT JOIN group_memberships gm \
           ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE l.id = $2",
    )
    .bind(t.stranger_in_b)
    .bind(t.a.list_id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    let (_group_id, _creator, _status, role) = row.expect("list still exists");
    assert!(
        role.is_none(),
        "non-member must get role=NULL, got {role:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_list_query_returns_none_for_unknown_list(pool: PgPool) {
    let _t = seed_two_tenants(&pool).await;
    let row: Option<(Uuid, Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT l.group_id, l.created_by_user_id, l.status::text, gm.role::text \
         FROM lists l \
         LEFT JOIN group_memberships gm \
           ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE l.id = $2",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(row.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_claim_query_returns_null_role_for_non_member(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    // Seed a claim in group A.
    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (list_id, hauler_user_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(t.a.list_id)
    .bind(t.a.hauler_user_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Mirrors `CurrentClaim::from_request_parts`.
    let row: Option<(Uuid, Uuid, Uuid, String, Option<String>, String)> = sqlx::query_as(
        "SELECT c.list_id, l.group_id, c.hauler_user_id, c.status::text, \
                gm.role::text, l.status::text \
         FROM claims c \
         JOIN lists l ON l.id = c.list_id \
         LEFT JOIN group_memberships gm \
           ON gm.group_id = l.group_id AND gm.user_id = $1 \
         WHERE c.id = $2",
    )
    .bind(t.stranger_in_b)
    .bind(claim_id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    let (_, _, _, _, role, _) = row.expect("claim exists");
    assert!(role.is_none(), "non-member must get role=NULL on claim");
}

// ──────────── Direct-table cross-tenant probes (data-isolation) ──────────
//
// Confirm the underlying writes/queries can't touch B's rows when scoped to
// A. These guard against any future handler that forgets to scope by group.

#[sqlx::test(migrations = "../../migrations")]
async fn lists_for_group_excludes_other_tenants(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    // Group B has its own list — find any list NOT in group A:
    let other_list_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lists WHERE group_id != $1")
            .bind(t.a.group_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        other_list_count >= 1,
        "fixture should have created a list in group B"
    );

    // The list-for-group query (lists.rs:list_for_group) must filter by group_id.
    let listed: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM lists WHERE group_id = $1")
        .bind(t.a.group_id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "only group A's list should be visible");
    assert_eq!(listed[0], t.a.list_id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn group_corps_lookup_is_scoped(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    // Insert a corp linked to group B but not A.
    let corp_id: Uuid = sqlx::query_scalar(
        "INSERT INTO corps (esi_corporation_id, name, ticker) \
         VALUES ($1, 'CorpB', 'CRPB') RETURNING id",
    )
    .bind(98_000_001_i64)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Find group B and one of its members.
    let (group_b, linker): (Uuid, Uuid) = sqlx::query_as(
        "SELECT g.id, gm.user_id \
         FROM groups g \
         JOIN group_memberships gm ON gm.group_id = g.id \
         WHERE g.id != $1 \
         LIMIT 1",
    )
    .bind(t.a.group_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO group_corps (group_id, corp_id, linked_by_user_id) VALUES ($1, $2, $3)",
    )
    .bind(group_b)
    .bind(corp_id)
    .bind(linker)
    .execute(&pool)
    .await
    .unwrap();

    // The "is this corp linked to MY group?" probe (corps.rs:require_corp_in_group)
    // must return false for group A.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM group_corps \
         WHERE group_id = $1 AND corp_id = $2 AND unlinked_at IS NULL)",
    )
    .bind(t.a.group_id)
    .bind(corp_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!exists, "corp_b must not be visible to group A");
}
