//! Phase 9 / M3: abuse-cap enforcement.
//!
//! Drives the `check_*_quota` helpers directly against a `sqlx::test`
//! pool with cap values dialled low, so we don't need 200 fixture rows
//! per test.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use jitacart_api::errors::ApiError;
use jitacart_api::groups::check_group_quota;
use jitacart_api::lists::check_lists_quota;
use sqlx::PgPool;
use uuid::Uuid;

// ── groups_per_user ───────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn group_quota_passes_below_cap(pool: PgPool) {
    let user = insert_user(&pool, "alice").await;
    insert_group(&pool, user, "g1").await;
    // Cap is 5; the user has 1 membership.
    check_group_quota(&pool, user, 5).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn group_quota_blocks_at_cap(pool: PgPool) {
    let user = insert_user(&pool, "alice").await;
    insert_group(&pool, user, "g1").await;
    insert_group(&pool, user, "g2").await;
    let err = check_group_quota(&pool, user, 2).await.unwrap_err();
    match err {
        ApiError::QuotaExceeded(msg) => {
            assert!(msg.contains("limit 2"), "msg: {msg}");
            assert!(msg.contains("already a member of 2"), "msg: {msg}");
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn group_quota_counts_joins_not_just_owns(pool: PgPool) {
    let alice = insert_user(&pool, "alice").await;
    let bob = insert_user(&pool, "bob").await;
    // Bob owns nothing; Alice owns one group; Bob joins 2 groups.
    let g1 = insert_group(&pool, alice, "g1").await;
    let g2 = insert_group(&pool, alice, "g2").await;
    add_member(&pool, g1, bob).await;
    add_member(&pool, g2, bob).await;

    // Bob's cap of 2 should already be hit.
    let err = check_group_quota(&pool, bob, 2).await.unwrap_err();
    assert!(matches!(err, ApiError::QuotaExceeded(_)));
}

// ── lists_per_group ───────────────────────────────────────────────────────

async fn insert_list(pool: &PgPool, group_id: Uuid, creator: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO lists (group_id, created_by_user_id, destination_label, status) \
         VALUES ($1, $2, 'D', $3) RETURNING id",
    )
    .bind(group_id)
    .bind(creator)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn lists_quota_blocks_at_cap(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group_id = insert_group(&pool, owner, "g").await;
    insert_list(&pool, group_id, owner, "open").await;
    insert_list(&pool, group_id, owner, "open").await;

    let err = check_lists_quota(&pool, group_id, 2).await.unwrap_err();
    assert!(matches!(err, ApiError::QuotaExceeded(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn lists_quota_excludes_archived(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group_id = insert_group(&pool, owner, "g").await;
    insert_list(&pool, group_id, owner, "open").await;
    insert_list(&pool, group_id, owner, "archived").await;
    insert_list(&pool, group_id, owner, "archived").await;

    // Cap is 2. Two archived rows exist but only one open row; the user
    // should be able to add one more open list.
    check_lists_quota(&pool, group_id, 2).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn lists_quota_passes_for_empty_group(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group_id = insert_group(&pool, owner, "g").await;
    check_lists_quota(&pool, group_id, 1).await.unwrap();
}
