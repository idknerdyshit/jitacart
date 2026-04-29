//! Shared fixture builders for settlement / API integration tests.
//!
//! Each helper writes the minimum schema required to exercise a settlement
//! state machine: two users, a group with two memberships, a list with two
//! items, fulfillments by the hauler, and a reimbursement row.

#![allow(dead_code)]

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

pub struct ListIds {
    pub group_id: Uuid,
    pub list_id: Uuid,
    pub requester_user_id: Uuid,
    pub hauler_user_id: Uuid,
    pub item_a_id: Uuid,
    pub item_b_id: Uuid,
    pub reimbursement_id: Uuid,
}

pub async fn insert_user(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO users (display_name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn insert_group(pool: &PgPool, owner: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, invite_code, created_by_user_id) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(format!("inv-{}", Uuid::new_v4().simple()))
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO group_memberships (user_id, group_id, role) VALUES ($1, $2, 'owner')")
        .bind(owner)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

pub async fn add_member(pool: &PgPool, group_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO group_memberships (user_id, group_id, role) VALUES ($1, $2, 'member')",
    )
    .bind(user_id)
    .bind(group_id)
    .execute(pool)
    .await
    .unwrap();
}

/// Seed a two-party list with two items both fulfilled by the hauler. By
/// default the items land in `bought` status (matching what the fulfillment
/// route writes after a buy run); use [`set_item_status`] to advance them.
pub async fn seed_two_party_list(pool: &PgPool, requester: Uuid, hauler: Uuid) -> ListIds {
    let owner = insert_user(pool, "owner").await;
    let group_id = insert_group(pool, owner, "test-group").await;
    add_member(pool, group_id, requester).await;
    add_member(pool, group_id, hauler).await;

    let list_id: Uuid = sqlx::query_scalar(
        "INSERT INTO lists (group_id, created_by_user_id, destination_label) \
         VALUES ($1, $2, 'Test Dest') RETURNING id",
    )
    .bind(group_id)
    .bind(requester)
    .fetch_one(pool)
    .await
    .unwrap();

    let item_a_id: Uuid = insert_item(pool, list_id, requester, 34, "Tritanium", 1000).await;
    let item_b_id: Uuid = insert_item(pool, list_id, requester, 35, "Pyerite", 500).await;

    insert_fulfillment(pool, item_a_id, hauler, 1000, Decimal::new(5, 0)).await;
    insert_fulfillment(pool, item_b_id, hauler, 500, Decimal::new(10, 0)).await;

    set_item_status(pool, item_a_id, "bought").await;
    set_item_status(pool, item_b_id, "bought").await;

    let subtotal = Decimal::new(5_000 + 5_000, 0); // 10000
    let reimbursement_id: Uuid = sqlx::query_scalar(
        "INSERT INTO reimbursements \
         (list_id, requester_user_id, hauler_user_id, subtotal_isk, tip_isk, total_isk) \
         VALUES ($1, $2, $3, $4, 0, $4) RETURNING id",
    )
    .bind(list_id)
    .bind(requester)
    .bind(hauler)
    .bind(subtotal)
    .fetch_one(pool)
    .await
    .unwrap();

    ListIds {
        group_id,
        list_id,
        requester_user_id: requester,
        hauler_user_id: hauler,
        item_a_id,
        item_b_id,
        reimbursement_id,
    }
}

pub async fn insert_item(
    pool: &PgPool,
    list_id: Uuid,
    requester: Uuid,
    type_id: i64,
    type_name: &str,
    qty: i64,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO list_items \
         (list_id, type_id, type_name, qty_requested, qty_fulfilled, requested_by_user_id, status) \
         VALUES ($1, $2, $3, $4, 0, $5, 'open') RETURNING id",
    )
    .bind(list_id)
    .bind(type_id)
    .bind(type_name)
    .bind(qty)
    .bind(requester)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub async fn insert_fulfillment(
    pool: &PgPool,
    item_id: Uuid,
    hauler: Uuid,
    qty: i64,
    unit_price: Decimal,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO fulfillments \
         (list_item_id, hauler_user_id, qty, unit_price_isk, bought_at_note) \
         VALUES ($1, $2, $3, $4, 'fixture') RETURNING id",
    )
    .bind(item_id)
    .bind(hauler)
    .bind(qty)
    .bind(unit_price)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub async fn set_item_status(pool: &PgPool, item_id: Uuid, status: &str) {
    sqlx::query("UPDATE list_items SET status = $1 WHERE id = $2")
        .bind(status)
        .bind(item_id)
        .execute(pool)
        .await
        .unwrap();
}

pub async fn get_item_status(pool: &PgPool, item_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM list_items WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn get_reimb_status(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM reimbursements WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub struct ContractFixture {
    pub contract_id: Uuid,
    pub esi_contract_id: i64,
}

/// `price_isk` is what the assignee pays the issuer. Goes through
/// `settlement::upsert_contract` so the fixture exercises the same write path
/// the worker uses.
pub async fn insert_contract(
    pool: &PgPool,
    issuer_user_id: Uuid,
    assignee_user_id: Uuid,
    price_isk: Decimal,
    status: &str,
    items_synced: bool,
) -> ContractFixture {
    let esi_id: i64 = chrono::Utc::now().timestamp_micros() & 0x7fff_ffff_ffff_ffff;
    let now = chrono::Utc::now();
    let date_completed = matches!(
        status,
        "finished" | "finished_issuer" | "finished_contractor"
    )
    .then_some(now);

    let upsert = settlement::ContractUpsert {
        esi_contract_id: esi_id,
        issuer_character_id: 1,
        issuer_user_id: Some(issuer_user_id),
        assignee_character_id: Some(2),
        assignee_user_id: Some(assignee_user_id),
        contract_type: "item_exchange".into(),
        status: status.into(),
        price_isk,
        reward_isk: Decimal::ZERO,
        collateral_isk: Decimal::ZERO,
        date_issued: now,
        date_expired: None,
        date_accepted: None,
        date_completed,
        start_location_id: None,
        end_location_id: None,
        raw_json: serde_json::json!({}),
    };

    let mut tx = pool.begin().await.unwrap();
    let outcome = settlement::upsert_contract(&mut tx, &upsert).await.unwrap();
    if items_synced {
        sqlx::query("UPDATE contracts SET items_synced_at = now() WHERE id = $1")
            .bind(outcome.contract_id)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    ContractFixture {
        contract_id: outcome.contract_id,
        esi_contract_id: esi_id,
    }
}

pub async fn bind_reimbursement_to_contract(
    pool: &PgPool,
    reimbursement_id: Uuid,
    contract_id: Uuid,
) {
    sqlx::query("UPDATE reimbursements SET contract_id = $1 WHERE id = $2")
        .bind(contract_id)
        .bind(reimbursement_id)
        .execute(pool)
        .await
        .unwrap();
}

pub async fn insert_suggestion(
    pool: &PgPool,
    contract_id: Uuid,
    reimbursement_id: Uuid,
    state: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO contract_match_suggestions \
         (contract_id, reimbursement_id, score, exact_match, state) \
         VALUES ($1, $2, 1.0, true, $3) RETURNING id",
    )
    .bind(contract_id)
    .bind(reimbursement_id)
    .bind(state)
    .fetch_one(pool)
    .await
    .unwrap()
}
