//! Integration tests for `reencrypt_stale`: the sweeper that drains
//! refresh tokens encrypted under a non-primary kid onto the current
//! primary key. Drives the function directly against a `sqlx::test`
//! pool with two MultiKeyCipher configurations to simulate "before"
//! and "after" rotation.

use std::collections::HashMap;

use auth_tokens::{reencrypt_stale, MultiKeyCipher};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use sqlx::PgPool;
use uuid::Uuid;

fn fresh_key_b64() -> String {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    B64.encode(k)
}

fn cipher(keys: &[(&str, &str)], primary: &str) -> MultiKeyCipher {
    let map: HashMap<String, String> = keys
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    MultiKeyCipher::from_keys(map, primary.to_string()).unwrap()
}

async fn insert_user(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (display_name) VALUES ($1) RETURNING id",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Insert a character row with both refresh and access tokens encrypted
/// under `c`'s primary kid. Returns `(character_id, refresh_pt, access_pt)`.
async fn insert_character(
    pool: &PgPool,
    user_id: Uuid,
    eve_char: i64,
    c: &MultiKeyCipher,
) -> (Uuid, Vec<u8>, Vec<u8>) {
    let refresh_pt = b"refresh-token-blob".to_vec();
    let access_pt = b"access-token-blob".to_vec();
    let (rt_ct, rt_nonce, kid) = c.encrypt(&refresh_pt).unwrap();
    let (at_ct, at_nonce, _) = c.encrypt(&access_pt).unwrap();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO characters \
         (user_id, character_id, character_name, owner_hash, \
          refresh_token_ciphertext, refresh_token_nonce, \
          access_token_ciphertext, access_token_nonce, token_key_id) \
         VALUES ($1, $2, $3, 'h', $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user_id)
    .bind(eve_char)
    .bind(format!("char-{eve_char}"))
    .bind(&rt_ct)
    .bind(&rt_nonce)
    .bind(&at_ct)
    .bind(&at_nonce)
    .bind(&kid)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, refresh_pt, access_pt)
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeper_rewrites_stale_rows(pool: PgPool) {
    let v1 = fresh_key_b64();
    let v2 = fresh_key_b64();

    // Pre-rotation: rows written under v1.
    let pre = cipher(&[("v1", &v1)], "v1");
    let user = insert_user(&pool, "u").await;
    let (cid, refresh_pt, access_pt) = insert_character(&pool, user, 1001, &pre).await;

    // After rotation: both keys present, primary flipped to v2.
    let post = cipher(&[("v1", &v1), ("v2", &v2)], "v2");

    let outcome = reencrypt_stale(&pool, &post, 100).await.unwrap();
    assert_eq!(outcome.scanned, 1);
    assert_eq!(outcome.rewritten, 1);

    // Row's kid is now v2 and the new ciphertext decrypts under v2.
    #[derive(sqlx::FromRow)]
    struct RowAfter {
        token_key_id: String,
        refresh_token_ciphertext: Vec<u8>,
        refresh_token_nonce: Vec<u8>,
        access_token_ciphertext: Option<Vec<u8>>,
        access_token_nonce: Option<Vec<u8>>,
    }
    let row: RowAfter = sqlx::query_as(
        "SELECT token_key_id, refresh_token_ciphertext, refresh_token_nonce, \
                access_token_ciphertext, access_token_nonce \
         FROM characters WHERE id = $1",
    )
    .bind(cid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.token_key_id, "v2");
    assert_eq!(
        post.decrypt(&row.refresh_token_ciphertext, &row.refresh_token_nonce, "v2")
            .unwrap(),
        refresh_pt
    );
    assert_eq!(
        post.decrypt(
            &row.access_token_ciphertext.unwrap(),
            &row.access_token_nonce.unwrap(),
            "v2",
        )
        .unwrap(),
        access_pt
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeper_is_noop_when_already_primary(pool: PgPool) {
    let v1 = fresh_key_b64();
    let c = cipher(&[("v1", &v1)], "v1");
    let user = insert_user(&pool, "u").await;
    let _ = insert_character(&pool, user, 1001, &c).await;

    let outcome = reencrypt_stale(&pool, &c, 100).await.unwrap();
    assert_eq!(outcome.scanned, 0);
    assert_eq!(outcome.rewritten, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeper_handles_row_without_access_token(pool: PgPool) {
    let v1 = fresh_key_b64();
    let v2 = fresh_key_b64();
    let pre = cipher(&[("v1", &v1)], "v1");

    let refresh_pt = b"r".to_vec();
    let (rt_ct, rt_nonce, kid) = pre.encrypt(&refresh_pt).unwrap();
    let user = insert_user(&pool, "u").await;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO characters \
         (user_id, character_id, character_name, owner_hash, \
          refresh_token_ciphertext, refresh_token_nonce, token_key_id) \
         VALUES ($1, 2002, 'noaccess', 'h', $2, $3, $4) RETURNING id",
    )
    .bind(user)
    .bind(&rt_ct)
    .bind(&rt_nonce)
    .bind(&kid)
    .fetch_one(&pool)
    .await
    .unwrap();

    let post = cipher(&[("v1", &v1), ("v2", &v2)], "v2");
    let outcome = reencrypt_stale(&pool, &post, 100).await.unwrap();
    assert_eq!(outcome.rewritten, 1);

    let kid: String = sqlx::query_scalar("SELECT token_key_id FROM characters WHERE id = $1")
        .bind(cid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kid, "v2");
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeper_respects_batch_size(pool: PgPool) {
    let v1 = fresh_key_b64();
    let v2 = fresh_key_b64();
    let pre = cipher(&[("v1", &v1)], "v1");
    let user = insert_user(&pool, "u").await;
    for i in 0..3 {
        let _ = insert_character(&pool, user, 3000 + i, &pre).await;
    }

    let post = cipher(&[("v1", &v1), ("v2", &v2)], "v2");
    let first = reencrypt_stale(&pool, &post, 2).await.unwrap();
    assert_eq!(first.scanned, 2);
    assert_eq!(first.rewritten, 2);

    let second = reencrypt_stale(&pool, &post, 2).await.unwrap();
    assert_eq!(second.scanned, 1);
    assert_eq!(second.rewritten, 1);

    let third = reencrypt_stale(&pool, &post, 2).await.unwrap();
    assert_eq!(third.scanned, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeper_errors_when_kid_unknown(pool: PgPool) {
    // A row encrypted under "v0" but the runtime cipher only knows v1+v2.
    let v0 = fresh_key_b64();
    let v1 = fresh_key_b64();
    let v2 = fresh_key_b64();
    let bogus = cipher(&[("v0", &v0)], "v0");
    let user = insert_user(&pool, "u").await;
    let _ = insert_character(&pool, user, 4000, &bogus).await;

    let runtime = cipher(&[("v1", &v1), ("v2", &v2)], "v2");
    let err = reencrypt_stale(&pool, &runtime, 10).await.unwrap_err();
    assert!(
        err.to_string().contains("unknown kid")
            || err
                .chain()
                .any(|c| c.to_string().contains("unknown kid")),
        "expected unknown-kid error, got: {err:#}"
    );
}
