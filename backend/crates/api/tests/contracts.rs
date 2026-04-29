//! API contracts integration tests. Drives the route handler bodies (extracted
//! as `do_*` helpers) directly against a sqlx::test database, avoiding the
//! tower-sessions / JWKS harness — none of which the contract logic touches.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use jitacart_api::contracts::{do_confirm, do_manual_link, do_reject, do_unlink, ContractError};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrations = "../../migrations")]
async fn confirm_happy_path_contract_outstanding(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let sugg_id =
        insert_suggestion(&pool, contract.contract_id, ids.reimbursement_id, "pending").await;

    let res = do_confirm(&pool, hauler, sugg_id).await.unwrap();
    assert_eq!(res.state, "confirmed");
    assert!(!res.settled);

    // Reimbursement now bound, items still in original status.
    let bound: Option<Uuid> =
        sqlx::query_scalar("SELECT contract_id FROM reimbursements WHERE id = $1")
            .bind(ids.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(bound, Some(contract.contract_id));
    let state: String =
        sqlx::query_scalar("SELECT state FROM contract_match_suggestions WHERE id = $1")
            .bind(sugg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "confirmed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirm_against_finished_contract_settles_immediately(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "finished",
        true,
    )
    .await;
    let sugg_id =
        insert_suggestion(&pool, contract.contract_id, ids.reimbursement_id, "pending").await;

    let res = do_confirm(&pool, hauler, sugg_id).await.unwrap();
    assert_eq!(res.state, "confirmed");
    assert!(res.settled);

    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirm_by_non_issuer_is_forbidden(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let other = insert_user(&pool, "other").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let sugg_id =
        insert_suggestion(&pool, contract.contract_id, ids.reimbursement_id, "pending").await;

    let err = do_confirm(&pool, other, sugg_id).await.unwrap_err();
    assert!(matches!(err, ContractError::Forbidden));
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirm_when_reimbursement_already_bound(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract_a = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let contract_b = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(11_000, 0),
        "outstanding",
        true,
    )
    .await;

    // Pre-bind to A.
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract_a.contract_id).await;
    // Suggestion exists for B.
    let sugg_b = insert_suggestion(
        &pool,
        contract_b.contract_id,
        ids.reimbursement_id,
        "pending",
    )
    .await;

    let err = do_confirm(&pool, hauler, sugg_b).await.unwrap_err();
    assert!(matches!(err, ContractError::Conflict(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn double_confirm_hits_partial_unique_index(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract_a = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let contract_b = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(11_000, 0),
        "outstanding",
        true,
    )
    .await;
    let sugg_a = insert_suggestion(
        &pool,
        contract_a.contract_id,
        ids.reimbursement_id,
        "pending",
    )
    .await;
    let sugg_b = insert_suggestion(
        &pool,
        contract_b.contract_id,
        ids.reimbursement_id,
        "pending",
    )
    .await;

    // First confirm against A succeeds.
    do_confirm(&pool, hauler, sugg_a).await.unwrap();

    // Now manually unbind reimbursement so the "already bound" check passes,
    // but leave A's confirmed suggestion in place — that's what triggers the
    // partial unique index when we try to confirm B.
    sqlx::query("UPDATE reimbursements SET contract_id = NULL WHERE id = $1")
        .bind(ids.reimbursement_id)
        .execute(&pool)
        .await
        .unwrap();

    let err = do_confirm(&pool, hauler, sugg_b).await.unwrap_err();
    match err {
        ContractError::Conflict(msg) => assert!(msg.contains("already confirmed")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn reject_by_non_issuer_is_forbidden(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let other = insert_user(&pool, "other").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let sugg =
        insert_suggestion(&pool, contract.contract_id, ids.reimbursement_id, "pending").await;

    let err = do_reject(&pool, other, sugg).await.unwrap_err();
    assert!(matches!(err, ContractError::Forbidden));
}

#[sqlx::test(migrations = "../../migrations")]
async fn reject_already_decided_is_conflict(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;
    let sugg = insert_suggestion(
        &pool,
        contract.contract_id,
        ids.reimbursement_id,
        "rejected",
    )
    .await;

    let err = do_reject(&pool, hauler, sugg).await.unwrap_err();
    assert!(matches!(err, ContractError::Conflict(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_link_with_mismatched_assignee_is_conflict(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let other_requester = insert_user(&pool, "other_requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;
    add_member(&pool, ids.group_id, other_requester).await;

    // Contract assigns to other_requester, but reimbursement's requester is `requester`.
    let contract = insert_contract(
        &pool,
        hauler,
        other_requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;

    let err = do_manual_link(&pool, hauler, contract.contract_id, ids.reimbursement_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ContractError::Conflict(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_link_by_non_member_is_forbidden(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    // Contract issued by hauler, assignee is requester. But the hauler's membership row
    // only exists in this group; let's place the contract in a different group.
    // Simpler: drop the hauler from the group → hauler is the issuer but not a group
    // member (which is what the route guards against).
    sqlx::query("DELETE FROM group_memberships WHERE user_id = $1 AND group_id = $2")
        .bind(hauler)
        .bind(ids.group_id)
        .execute(&pool)
        .await
        .unwrap();

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "outstanding",
        true,
    )
    .await;

    let err = do_manual_link(&pool, hauler, contract.contract_id, ids.reimbursement_id)
        .await
        .unwrap_err();
    assert!(matches!(err, ContractError::Forbidden));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unlink_finished_contract_is_conflict(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let _ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "finished",
        true,
    )
    .await;

    let err = do_unlink(&pool, hauler, contract.contract_id)
        .await
        .unwrap_err();
    match err {
        ContractError::Conflict(msg) => assert!(msg.contains("unwind")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn unlink_in_progress_returns_pending_supersedes_confirmed(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(10_000, 0),
        "in_progress",
        true,
    )
    .await;
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;
    let sugg = insert_suggestion(
        &pool,
        contract.contract_id,
        ids.reimbursement_id,
        "confirmed",
    )
    .await;

    do_unlink(&pool, hauler, contract.contract_id)
        .await
        .unwrap();

    let bound: Option<Uuid> =
        sqlx::query_scalar("SELECT contract_id FROM reimbursements WHERE id = $1")
            .bind(ids.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(bound.is_none());

    let state: String =
        sqlx::query_scalar("SELECT state FROM contract_match_suggestions WHERE id = $1")
            .bind(sugg)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "superseded");
}
