//! Manual-path settlement: items must be `delivered`, only the caller's triple
//! `(list, requester, hauler)` is touched.

mod common;

use common::*;
use rust_decimal::Decimal;
use settlement::SettlementError;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn errors_when_items_still_bought(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;
    // Items default to 'bought' — manual settle must refuse.

    let mut tx = pool.begin().await.unwrap();
    let err = settlement::settle_manual(&mut tx, ids.reimbursement_id, requester)
        .await
        .unwrap_err();
    match err {
        SettlementError::NotDelivered { count } => assert!(count > 0),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn succeeds_when_items_delivered(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    set_item_status(&pool, ids.item_a_id, "delivered").await;
    set_item_status(&pool, ids.item_b_id, "delivered").await;

    let mut tx = pool.begin().await.unwrap();
    settlement::settle_manual(&mut tx, ids.reimbursement_id, requester)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        get_reimb_status(&pool, ids.reimbursement_id).await,
        "settled"
    );
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, ids.item_b_id).await, "settled");
}

#[sqlx::test(migrations = "../../migrations")]
async fn cross_hauler_isolation(pool: PgPool) {
    // Requester owes hauler_b for one item; settling the requester↔hauler_a
    // reimbursement must not flip hauler_b's item.
    let requester = insert_user(&pool, "requester").await;
    let hauler_a = insert_user(&pool, "hauler_a").await;
    let hauler_b = insert_user(&pool, "hauler_b").await;
    let ids = seed_two_party_list(&pool, requester, hauler_a).await;
    add_member(&pool, ids.group_id, hauler_b).await;

    set_item_status(&pool, ids.item_a_id, "delivered").await;
    set_item_status(&pool, ids.item_b_id, "delivered").await;

    // Add an item fulfilled by hauler_b on the same list.
    let item_c = insert_item(&pool, ids.list_id, requester, 36, "Mexallon", 100).await;
    insert_fulfillment(&pool, item_c, hauler_b, 100, Decimal::new(20, 0)).await;
    set_item_status(&pool, item_c, "delivered").await;

    // hauler_b's pending reimbursement on the same list/requester.
    let reimb_b: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO reimbursements \
         (list_id, requester_user_id, hauler_user_id, subtotal_isk, tip_isk, total_isk) \
         VALUES ($1, $2, $3, 2000, 0, 2000) RETURNING id",
    )
    .bind(ids.list_id)
    .bind(requester)
    .bind(hauler_b)
    .fetch_one(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    settlement::settle_manual(&mut tx, ids.reimbursement_id, requester)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // hauler_a's items flipped, but hauler_b's item must remain delivered
    // because there's still a pending reimbursement covering it.
    assert_eq!(get_item_status(&pool, ids.item_a_id).await, "settled");
    assert_eq!(get_item_status(&pool, item_c).await, "delivered");
    assert_eq!(get_reimb_status(&pool, reimb_b).await, "pending");
}
