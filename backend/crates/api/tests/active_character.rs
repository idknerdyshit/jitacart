//! Active-character preference: setting, clearing, and ownership checks.
//!
//! Drives `do_set_active_character` directly against a `sqlx::test` pool so
//! the extractor / tower-sessions stack stays out of the picture.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use jitacart_api::auth::do_set_active_character;
use jitacart_api::errors::ApiError;
use sqlx::PgPool;
use uuid::Uuid;

async fn insert_character(pool: &PgPool, user_id: Uuid, eve_char_id: i64, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO characters \
         (user_id, character_id, character_name, owner_hash, \
          refresh_token_ciphertext, refresh_token_nonce) \
         VALUES ($1, $2, $3, 'owner-hash', '\\x00'::bytea, '\\x00'::bytea) \
         RETURNING id",
    )
    .bind(user_id)
    .bind(eve_char_id)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn get_active_character_id(pool: &PgPool, user_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT active_character_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn set_active_character_succeeds_for_owned_character(pool: PgPool) {
    let user = insert_user(&pool, "owner").await;
    let char_id = insert_character(&pool, user, 1001, "Alpha").await;

    do_set_active_character(&pool, user, Some(char_id))
        .await
        .unwrap();

    assert_eq!(get_active_character_id(&pool, user).await, Some(char_id));
}

#[sqlx::test(migrations = "../../migrations")]
async fn set_active_character_rejects_other_users_character(pool: PgPool) {
    let alice = insert_user(&pool, "alice").await;
    let bob = insert_user(&pool, "bob").await;
    let bobs_char = insert_character(&pool, bob, 2002, "BobsAlt").await;

    let err = do_set_active_character(&pool, alice, Some(bobs_char))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Unauthorized));

    assert_eq!(get_active_character_id(&pool, alice).await, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn clear_active_character_with_null(pool: PgPool) {
    let user = insert_user(&pool, "u").await;
    let char_id = insert_character(&pool, user, 3003, "Gamma").await;

    do_set_active_character(&pool, user, Some(char_id))
        .await
        .unwrap();
    assert_eq!(get_active_character_id(&pool, user).await, Some(char_id));

    do_set_active_character(&pool, user, None).await.unwrap();
    assert_eq!(get_active_character_id(&pool, user).await, None);
}
