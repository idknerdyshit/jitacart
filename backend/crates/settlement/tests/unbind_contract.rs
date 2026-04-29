//! `unbind_contract` returns pending reimbursements to unbound, supersedes
//! pending suggestions, and is a no-op for already-settled rows.

mod common;

use common::*;
use rust_decimal::Decimal;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn pending_reimbursements_lose_contract_id(pool: PgPool) {
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
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;

    let mut tx = pool.begin().await.unwrap();
    let unbound = settlement::unbind_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(unbound, 1);
    let bound: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT contract_id FROM reimbursements WHERE id = $1")
            .bind(ids.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(bound.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn pending_suggestions_become_superseded(pool: PgPool) {
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

    let mut tx = pool.begin().await.unwrap();
    settlement::unbind_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let state: String =
        sqlx::query_scalar("SELECT state FROM contract_match_suggestions WHERE id = $1")
            .bind(sugg_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "superseded");
}

#[sqlx::test(migrations = "../../migrations")]
async fn settled_reimbursements_untouched_idempotent_double_unbind(pool: PgPool) {
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
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;

    // Settle first.
    let mut tx = pool.begin().await.unwrap();
    settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );

    // Now unbind twice; settled rows are not pending so unbound = 0.
    let mut tx = pool.begin().await.unwrap();
    let n1 = settlement::unbind_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let n2 = settlement::unbind_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(n1, 0);
    assert_eq!(n2, 0);
    // Reimbursement remains settled and bound.
    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );
    let still_bound: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT contract_id FROM reimbursements WHERE id = $1")
            .bind(ids.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(still_bound, Some(contract.contract_id));
}
