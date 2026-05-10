//! List archive transitions and the guards that depend on them.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use domain::{GroupRole, ListStatus};
use jitacart_api::errors::ApiError;
use jitacart_api::extract::CurrentList;
use jitacart_api::lists::do_patch_list_status;
use sqlx::PgPool;
use uuid::Uuid;

fn cur(list_id: Uuid, user_id: Uuid, group_id: Uuid, status: ListStatus) -> CurrentList {
    CurrentList {
        user_id,
        group_id,
        list_id,
        role: GroupRole::Owner,
        created_by_user_id: user_id,
        status,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn archive_list_blocks_item_add(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    do_patch_list_status(&pool, ids.list_id, requester, ListStatus::Archived)
        .await
        .unwrap();

    // The status was applied at the DB level.
    let status: String = sqlx::query_scalar("SELECT status::text FROM lists WHERE id = $1")
        .bind(ids.list_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "archived");

    // The require_open / require_mutable guards `add_items` and the
    // `patch_item` / `delete_item` paths use will reject mutation.
    let archived = cur(ids.list_id, requester, ids.group_id, ListStatus::Archived);
    assert!(archived.require_open().is_err());
    assert!(archived.require_mutable().is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn archive_list_blocks_claim_create(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    do_patch_list_status(&pool, ids.list_id, requester, ListStatus::Archived)
        .await
        .unwrap();

    // Claim creation calls `cur.require_open()` — same guard, exercised from
    // the hauler's perspective.
    let archived = cur(ids.list_id, hauler, ids.group_id, ListStatus::Archived);
    let err = archived.require_open().unwrap_err();
    let _ = err; // body of the response is opaque; we only care it was Err.
}

#[sqlx::test(migrations = "../../migrations")]
async fn unarchive_list_restores_mutability(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    do_patch_list_status(&pool, ids.list_id, requester, ListStatus::Archived)
        .await
        .unwrap();
    do_patch_list_status(&pool, ids.list_id, requester, ListStatus::Open)
        .await
        .unwrap();

    let status: String = sqlx::query_scalar("SELECT status::text FROM lists WHERE id = $1")
        .bind(ids.list_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "open");

    let open = cur(ids.list_id, requester, ids.group_id, ListStatus::Open);
    assert!(open.require_open().is_ok());
    assert!(open.require_mutable().is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn patch_list_status_forbidden_for_non_creator_member(pool: PgPool) {
    let requester = insert_user(&pool, "requester").await;
    let hauler = insert_user(&pool, "hauler").await;
    let ids = seed_two_party_list(&pool, requester, hauler).await;

    let err = do_patch_list_status(&pool, ids.list_id, hauler, ListStatus::Archived)
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Forbidden(_)));
}
