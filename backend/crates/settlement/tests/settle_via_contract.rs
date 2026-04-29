//! `settle_via_contract` end-to-end behaviour against a live test database.

mod common;

use common::*;
use rust_decimal::Decimal;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn bought_only_items_flip_to_settled(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    // Both items already in 'bought' (default seed). Bind reimbursement.
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

    let mut tx = pool.begin().await.unwrap();
    let count = settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(count, 1);
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn delivered_only_items_flip_to_settled(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    set_item_status(&pool, ids.item_a_id, "delivered").await;
    set_item_status(&pool, ids.item_b_id, "delivered").await;

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

    let mut tx = pool.begin().await.unwrap();
    let count = settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(count, 1);
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
}

#[sqlx::test(migrations = "../../migrations")]
async fn mixed_bought_and_delivered_flip_together(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    set_item_status(&pool, ids.item_a_id, "delivered").await;
    // item_b stays at 'bought'

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

    let mut tx = pool.begin().await.unwrap();
    settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
}

#[sqlx::test(migrations = "../../migrations")]
async fn settlement_delta_isk_is_overpayment_signed(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    // Reimbursement total is 10_000 (subtotal 10k, no tip). Contract pays 12_000 → +2_000.
    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(12_000, 0),
        "finished",
        true,
    )
    .await;
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;

    let mut tx = pool.begin().await.unwrap();
    settlement::recompute_contract_expected_total(&mut tx, contract.contract_id)
        .await
        .unwrap();
    settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let delta: Decimal =
        sqlx::query_scalar("SELECT settlement_delta_isk FROM contracts WHERE id = $1")
            .bind(contract.contract_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(delta, Decimal::new(2_000, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn settlement_delta_isk_negative_when_underpaid(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let contract = insert_contract(
        &pool,
        hauler,
        requester,
        Decimal::new(8_000, 0),
        "finished",
        true,
    )
    .await;
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;

    let mut tx = pool.begin().await.unwrap();
    settlement::recompute_contract_expected_total(&mut tx, contract.contract_id)
        .await
        .unwrap();
    settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let delta: Decimal =
        sqlx::query_scalar("SELECT settlement_delta_isk FROM contracts WHERE id = $1")
            .bind(contract.contract_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(delta, Decimal::new(-2_000, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn settled_at_uses_contract_date_completed(pool: PgPool) {
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

    // Pin date_completed to a known timestamp.
    let pinned = chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    sqlx::query("UPDATE contracts SET date_completed = $1 WHERE id = $2")
        .bind(pinned)
        .bind(contract.contract_id)
        .execute(&pool)
        .await
        .unwrap();

    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;

    let mut tx = pool.begin().await.unwrap();
    settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (settled_at, settled_by): (Option<chrono::DateTime<chrono::Utc>>, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT settled_at, settled_by_user_id FROM reimbursements WHERE id = $1")
            .bind(ids.reimbursement_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(settled_at, Some(pinned));
    assert_eq!(settled_by, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_bound_rows_returns_zero_no_side_effects(pool: PgPool) {
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
    // Deliberately do NOT bind the reimbursement.

    let mut tx = pool.begin().await.unwrap();
    let count = settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(count, 0);
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "bought");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "bought");
    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "pending"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn multiple_bound_reimbursements_all_settle(pool: PgPool) {
    // Two requesters owe the same hauler on the same contract. The bulk
    // UPDATE must flip both reimbursements and every covered item.
    let requester_a = insert_user(&pool, "requester_a").await;
    let requester_b = insert_user(&pool, "requester_b").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester_a, hauler).await;
    add_member(&pool, ids.group_id, requester_b).await;

    // Add a second list-item, fulfilled by the hauler for requester_b.
    let item_c = insert_item(&pool, ids.list_id, requester_b, 36, "Mexallon", 200).await;
    insert_fulfillment(&pool, item_c, hauler, 200, Decimal::new(15, 0)).await;
    set_item_status(&pool, item_c, "bought").await;

    // Reimbursement for requester_b ↔ hauler.
    let reimb_b: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO reimbursements \
         (list_id, requester_user_id, hauler_user_id, subtotal_isk, tip_isk, total_isk) \
         VALUES ($1, $2, $3, 3000, 0, 3000) RETURNING id",
    )
    .bind(ids.list_id)
    .bind(requester_b)
    .bind(hauler)
    .fetch_one(&pool)
    .await
    .unwrap();

    // One contract; both reimbursements bound to it. (assignee_user_id can
    // only point at one user — we leave it as requester_a; the matcher's
    // assignee guard runs at confirm time, not here.)
    let contract = insert_contract(
        &pool,
        hauler,
        requester_a,
        Decimal::new(20_000, 0),
        "finished",
        true,
    )
    .await;
    bind_reimbursement_to_contract(&pool, ids.reimbursement_id, contract.contract_id).await;
    bind_reimbursement_to_contract(&pool, reimb_b, contract.contract_id).await;

    let mut tx = pool.begin().await.unwrap();
    let count = settlement::settle_via_contract(&mut tx, contract.contract_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(count, 2);
    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );
    assert_eq!(get_reimb_status(&pool, reimb_b).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
    assert_eq!(get_item_status(&pool, item_c).await, "settled");
}
