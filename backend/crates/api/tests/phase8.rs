//! Phase 8 integration tests: active-character preference, Discord webhook
//! CRUD + dispatch toggle filtering, and list archive transitions. Drives the
//! `do_*` helpers directly against a `sqlx::test` pool so the extractor /
//! tower-sessions stack stays out of the picture.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use std::sync::Mutex;

use async_trait::async_trait;
use common::*;
use domain::{GroupRole, ListStatus};
use jitacart_api::auth::do_set_active_character;
use jitacart_api::errors::ApiError;
use jitacart_api::extract::CurrentList;
use jitacart_api::lists::do_patch_list_status;
use jitacart_api::webhooks::{
    build_payload, dispatch_webhook, do_delete_webhook, do_get_webhook, do_upsert_webhook,
    WebhookConfig, WebhookEvent, WebhookSender,
};
use webhook_dispatch::Error as WebhookDispatchError;
use sqlx::PgPool;
use uuid::Uuid;

// ── Helpers ───────────────────────────────────────────────────────────────

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

fn sample_config(url: &str) -> WebhookConfig {
    WebhookConfig {
        webhook_url: url.into(),
        notify_list_created: true,
        notify_list_claimed: true,
        notify_list_delivered: true,
        notify_reimbursement_settled: true,
    }
}

#[derive(Default)]
struct CapturingSender {
    sent: Mutex<Vec<(String, serde_json::Value)>>,
}

#[async_trait]
impl WebhookSender for CapturingSender {
    async fn send(&self, url: &str, payload: serde_json::Value) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push((url.to_string(), payload));
        Ok(())
    }
}

// ── Active-character ──────────────────────────────────────────────────────

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

// ── Webhook CRUD ──────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_upsert_rejects_non_discord_url(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group = insert_group(&pool, owner, "g").await;

    let bad = sample_config("https://evil.example.com/api/webhooks/abc");
    let err = do_upsert_webhook(&pool, group, GroupRole::Owner, bad)
        .await
        .unwrap_err();
    assert!(matches!(err, WebhookDispatchError::BadRequest(_)));

    let stored = do_get_webhook(&pool, group, GroupRole::Owner)
        .await
        .unwrap();
    assert!(stored.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_upsert_then_get_roundtrip(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group = insert_group(&pool, owner, "g").await;

    let cfg = WebhookConfig {
        webhook_url: "https://discord.com/api/webhooks/123/abc".into(),
        notify_list_created: true,
        notify_list_claimed: false,
        notify_list_delivered: true,
        notify_reimbursement_settled: false,
    };
    do_upsert_webhook(&pool, group, GroupRole::Owner, cfg.clone())
        .await
        .unwrap();

    let stored = do_get_webhook(&pool, group, GroupRole::Owner)
        .await
        .unwrap()
        .expect("row was just written");
    assert_eq!(stored.webhook_url, cfg.webhook_url);
    assert!(stored.notify_list_created);
    assert!(!stored.notify_list_claimed);
    assert!(stored.notify_list_delivered);
    assert!(!stored.notify_reimbursement_settled);
}

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_delete_removes_row(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group = insert_group(&pool, owner, "g").await;

    do_upsert_webhook(
        &pool,
        group,
        GroupRole::Owner,
        sample_config("https://discord.com/api/webhooks/1/x"),
    )
    .await
    .unwrap();
    assert!(do_get_webhook(&pool, group, GroupRole::Owner)
        .await
        .unwrap()
        .is_some());

    do_delete_webhook(&pool, group, GroupRole::Owner)
        .await
        .unwrap();
    assert!(do_get_webhook(&pool, group, GroupRole::Owner)
        .await
        .unwrap()
        .is_none());
}

// ── Webhook dispatch ──────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_dispatch_skips_event_when_toggle_off(pool: PgPool) {
    let owner = insert_user(&pool, "owner").await;
    let group = insert_group(&pool, owner, "g").await;

    let cfg = WebhookConfig {
        webhook_url: "https://discord.com/api/webhooks/9/abc".into(),
        notify_list_created: false,
        notify_list_claimed: true,
        notify_list_delivered: true,
        notify_reimbursement_settled: true,
    };
    do_upsert_webhook(&pool, group, GroupRole::Owner, cfg)
        .await
        .unwrap();

    let sender = CapturingSender::default();

    // Toggle off → no send.
    let event_off = WebhookEvent::ListCreated {
        list_destination: "Jita".into(),
        item_count: 3,
        estimate_isk: "1000".into(),
        creator_name: "Alpha".into(),
    };
    dispatch_webhook(&pool, &sender, group, &event_off)
        .await
        .unwrap();

    // Toggle on → send.
    let event_on = WebhookEvent::ListClaimed {
        list_destination: "Jita".into(),
        hauler_name: "Beta".into(),
        item_count: 2,
    };
    dispatch_webhook(&pool, &sender, group, &event_on)
        .await
        .unwrap();

    let captured = sender.sent.lock().unwrap();
    assert_eq!(captured.len(), 1, "only the enabled event should be sent");
    let (url, payload) = &captured[0];
    assert_eq!(url, "https://discord.com/api/webhooks/9/abc");
    assert_eq!(payload, &build_payload(&event_on));
}

// ── List archive transitions ──────────────────────────────────────────────

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
    let status: String = sqlx::query_scalar("SELECT status FROM lists WHERE id = $1")
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

    let status: String = sqlx::query_scalar("SELECT status FROM lists WHERE id = $1")
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
