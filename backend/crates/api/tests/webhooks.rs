//! Discord webhook CRUD and dispatch toggle filtering.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use std::sync::Mutex;

use async_trait::async_trait;
use common::*;
use domain::GroupRole;
use jitacart_api::webhooks::{
    do_delete_webhook, do_get_webhook, do_upsert_webhook, WebhookConfig, WebhookEvent,
};
use sqlx::PgPool;
use webhook_dispatch::{
    build_payload, dispatch_webhook, Error as WebhookDispatchError, WebhookSender,
};

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
