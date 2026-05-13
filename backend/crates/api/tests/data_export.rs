//! `/me/export` endpoint.
//!
//! The endpoint returns a JSON document keyed to the caller's user_id.
//! These tests exercise `do_export_me` directly.

#![allow(clippy::explicit_auto_deref)]

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use jitacart_api::auth::do_export_me;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn export_for_empty_user_has_all_keys(pool: PgPool) {
    let user = insert_user(&pool, "alice").await;
    let mut conn = pool.acquire().await.unwrap();
    let v = do_export_me(&mut *conn, user).await.unwrap();
    assert!(v["user"].is_object(), "user");
    assert_eq!(v["user"]["display_name"], "alice");
    assert!(v["characters"].is_array());
    assert_eq!(v["characters"].as_array().unwrap().len(), 0);
    assert!(v["group_memberships"].is_array());
    assert_eq!(v["group_memberships"].as_array().unwrap().len(), 0);
    assert!(v["lists_created"].is_array());
    assert_eq!(v["lists_created"].as_array().unwrap().len(), 0);
    assert!(v["fulfillments_as_hauler"].is_array());
    assert_eq!(v["fulfillments_as_hauler"].as_array().unwrap().len(), 0);
    assert!(v["reimbursements_involving_me"].is_array());
}

#[sqlx::test(migrations = "../../migrations")]
async fn export_includes_membership_and_list(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let alice = insert_user(&pool, "alice").await;
    let group = insert_group(&pool, owner, "team").await;
    add_member(&pool, group, alice).await;

    // Alice creates a list.
    let _list_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO lists (group_id, created_by_user_id, destination_label) \
         VALUES ($1, $2, 'Wormhole-A') RETURNING id",
    )
    .bind(group)
    .bind(alice)
    .fetch_one(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let v = do_export_me(&mut *conn, alice).await.unwrap();
    let memberships = v["group_memberships"].as_array().unwrap();
    assert_eq!(memberships.len(), 1, "alice in one group");
    assert_eq!(memberships[0]["group_name"], "team");
    assert_eq!(memberships[0]["role"], "member");

    let lists = v["lists_created"].as_array().unwrap();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["destination_label"], "Wormhole-A");
}

#[sqlx::test(migrations = "../../migrations")]
async fn export_excludes_other_users_data(pool: PgPool) {
    let alice = insert_user(&pool, "alice").await;
    let bob = insert_user(&pool, "bob").await;
    let group = insert_group(&pool, alice, "team").await;
    add_member(&pool, group, bob).await;

    // Both create a list.
    sqlx::query("INSERT INTO lists (group_id, created_by_user_id, destination_label) VALUES ($1, $2, 'AlicesList')")
        .bind(group)
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lists (group_id, created_by_user_id, destination_label) VALUES ($1, $2, 'BobsList')")
        .bind(group)
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let alice_export = do_export_me(&mut *conn, alice).await.unwrap();
    let lists = alice_export["lists_created"].as_array().unwrap();
    assert_eq!(lists.len(), 1, "alice should only see her own list");
    assert_eq!(lists[0]["destination_label"], "AlicesList");

    drop(conn);
    let mut conn = pool.acquire().await.unwrap();
    let bob_export = do_export_me(&mut *conn, bob).await.unwrap();
    let bob_lists = bob_export["lists_created"].as_array().unwrap();
    assert_eq!(bob_lists.len(), 1);
    assert_eq!(bob_lists[0]["destination_label"], "BobsList");
}

#[sqlx::test(migrations = "../../migrations")]
async fn export_never_includes_token_plaintext(pool: PgPool) {
    let user = insert_user(&pool, "alice").await;
    // Insert a fake character row with plaintext-looking blobs in the
    // ciphertext columns. The export must NOT surface those bytes.
    sqlx::query(
        "INSERT INTO characters \
         (user_id, character_id, character_name, owner_hash, \
          refresh_token_ciphertext, refresh_token_nonce) \
         VALUES ($1, 9001, 'Alpha', 'h', \
                 decode('deadbeef', 'hex'), decode('cafebabecafebabecafebabe', 'hex'))",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let v = do_export_me(&mut *conn, user).await.unwrap();
    let chars = v["characters"].as_array().unwrap();
    assert_eq!(chars.len(), 1);
    let serialized = serde_json::to_string(&chars[0]).unwrap();
    assert!(
        !serialized.to_lowercase().contains("ciphertext"),
        "token ciphertext column must not appear in export"
    );
    assert!(
        !serialized.to_lowercase().contains("nonce"),
        "token nonce column must not appear in export"
    );
    assert!(
        !serialized.to_lowercase().contains("deadbeef"),
        "raw token bytes must not appear in export: {serialized}"
    );
    // Metadata that *is* fine to show:
    assert_eq!(chars[0]["character_name"], "Alpha");
    assert_eq!(chars[0]["character_id"], 9001);
}
