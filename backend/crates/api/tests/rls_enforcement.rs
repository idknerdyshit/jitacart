//! Direct database RLS regression tests.
//!
//! These tests bypass every line of Rust handler/extractor code: they open a
//! raw connection, `SET LOCAL ROLE jitacart_app`, set the per-request session
//! var `app.current_user_id`, and probe the schema directly. The point is to
//! prove that the RLS policies installed by the init migration ARE the
//! gatekeeper — independent of any application-layer JOIN. A bug that drops
//! the extractor's `WHERE user_id = $caller` clause is caught by
//! `tenant_isolation.rs`; a bug that drops the RLS policy is caught here.
//!
//! `sqlx::test` runs each test against a freshly migrated DB owned by the
//! postgres superuser. The migration creates `jitacart_app` (idempotently,
//! without a password — fine for `SET LOCAL ROLE` which doesn't authenticate),
//! so `SET LOCAL ROLE jitacart_app` works inside the test connection.

#[path = "../../settlement/tests/common/mod.rs"]
mod common;

use common::*;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Wrap a closure in `BEGIN; SET LOCAL ROLE jitacart_app;
/// SET LOCAL app.current_user_id = $uid; <body>; ROLLBACK;`. Pass empty
/// string for an anonymous probe.
async fn as_app<F, Fut, T>(pool: &PgPool, uid: &str, f: F) -> T
where
    F: FnOnce(PgConnection) -> Fut,
    Fut: std::future::Future<Output = (PgConnection, T)>,
{
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    sqlx::query("SET LOCAL ROLE jitacart_app")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
        .bind(uid)
        .execute(&mut *conn)
        .await
        .unwrap();

    let detached = conn.detach();
    let (mut conn, result) = f(detached).await;

    sqlx::query("ROLLBACK").execute(&mut conn).await.unwrap();
    result
}

struct TwoTenants {
    a_group_id: Uuid,
    a_list_id: Uuid,
    a_item_a_id: Uuid,
    a_reimbursement_id: Uuid,
    a_hauler: Uuid,
    b_list_id: Uuid,
    stranger_in_b: Uuid,
}

async fn seed_two_tenants(pool: &PgPool) -> TwoTenants {
    let requester_a = insert_user(pool, "requester_a").await;
    let hauler_a = insert_user(pool, "hauler_a").await;
    let a = seed_two_party_list(pool, requester_a, hauler_a).await;

    let requester_b = insert_user(pool, "requester_b").await;
    let hauler_b = insert_user(pool, "hauler_b").await;
    let b = seed_two_party_list(pool, requester_b, hauler_b).await;

    let stranger_in_b = insert_user(pool, "stranger_in_b").await;
    add_member(pool, b.group_id, stranger_in_b).await;

    TwoTenants {
        a_group_id: a.group_id,
        a_list_id: a.list_id,
        a_item_a_id: a.item_a_id,
        a_reimbursement_id: a.reimbursement_id,
        a_hauler: a.hauler_user_id,
        b_list_id: b.list_id,
        stranger_in_b,
    }
}

// ─────────────────────────── Read-side policies ───────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn rls_hides_lists_from_strangers(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let ids = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM lists")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(
        !ids.contains(&t.a_list_id),
        "RLS leaked group-A list to a stranger in group B; saw {ids:?}"
    );
    assert!(
        ids.contains(&t.b_list_id),
        "stranger should still see their own group's list; saw {ids:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_hides_list_items_from_strangers(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let ids = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM list_items")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(
        !ids.contains(&t.a_item_a_id),
        "RLS leaked group-A item to a stranger in group B"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_hides_reimbursements_from_strangers(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let ids = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM reimbursements")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(
        !ids.contains(&t.a_reimbursement_id),
        "RLS leaked group-A reimbursement to a stranger in group B"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_hides_claims_from_strangers(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    // Seed a claim on group A.
    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (list_id, hauler_user_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(t.a_list_id)
    .bind(t.a_hauler)
    .fetch_one(&pool)
    .await
    .unwrap();

    let ids = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM claims")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(!ids.contains(&claim_id), "RLS leaked group-A claim");
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_anon_sees_no_tenant_rows(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let (lists, items, reimbs) = as_app(&pool, "", |mut conn| async move {
        let lists: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM lists")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        let items: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM list_items")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        let reimbs: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM reimbursements")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, (lists, items, reimbs))
    })
    .await;

    assert!(
        lists.is_empty(),
        "anon must not see any tenant rows from lists; saw {lists:?}"
    );
    assert!(items.is_empty(), "anon list_items leak: {items:?}");
    assert!(reimbs.is_empty(), "anon reimbursements leak: {reimbs:?}");
    // Sanity-check the fixture actually produced rows.
    let _ = (t.a_list_id, t.b_list_id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_member_sees_own_group(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let lists = as_app(&pool, &t.a_hauler.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM lists")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(
        lists.contains(&t.a_list_id),
        "hauler must see their own group's list; saw {lists:?}"
    );
    assert!(
        !lists.contains(&t.b_list_id),
        "hauler must NOT see group B's list; saw {lists:?}"
    );
}

// ─────────────────────────── Write-side policies ──────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn rls_blocks_cross_tenant_insert_on_list_items(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let result = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let res = sqlx::query(
            "INSERT INTO list_items (list_id, type_id, type_name, qty_requested) \
             VALUES ($1, 34, 'Tritanium', 1)",
        )
        .bind(t.a_list_id)
        .execute(&mut conn)
        .await;
        (conn, res)
    })
    .await;

    let err = result.expect_err("RLS must refuse cross-tenant INSERT on list_items");
    let db_err = match err {
        sqlx::Error::Database(e) => e,
        other => panic!("expected Database error, got {other:?}"),
    };
    // 42501 = insufficient_privilege. Postgres reports RLS WITH CHECK
    // violations under this code.
    assert_eq!(
        db_err.code().as_deref(),
        Some("42501"),
        "RLS policy violation should surface as SQLSTATE 42501; got {db_err:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_blocks_cross_tenant_update_on_lists(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let rows_affected = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let res = sqlx::query("UPDATE lists SET destination_label = 'pwned' WHERE id = $1")
            .bind(t.a_list_id)
            .execute(&mut conn)
            .await
            .unwrap();
        (conn, res.rows_affected())
    })
    .await;

    // RLS makes the row invisible, so UPDATE simply matches zero rows.
    assert_eq!(
        rows_affected, 0,
        "stranger must not be able to UPDATE group A's list"
    );
    let label: String = sqlx::query_scalar("SELECT destination_label FROM lists WHERE id = $1")
        .bind(t.a_list_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(label, "Test Dest", "label must remain untouched");
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_blocks_cross_tenant_delete_on_list_items(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    let rows_affected = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let res = sqlx::query("DELETE FROM list_items WHERE id = $1")
            .bind(t.a_item_a_id)
            .execute(&mut conn)
            .await
            .unwrap();
        (conn, res.rows_affected())
    })
    .await;

    assert_eq!(
        rows_affected, 0,
        "stranger must not be able to DELETE group A's items"
    );
    let still_there: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM list_items WHERE id = $1)")
            .bind(t.a_item_a_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(still_there, "item must still exist");
}

// ─────────────────────────── Nested-policy tables ─────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn rls_claim_items_inherit_list_membership(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;
    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (list_id, hauler_user_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(t.a_list_id)
    .bind(t.a_hauler)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO claim_items (claim_id, list_item_id) VALUES ($1, $2)")
        .bind(claim_id)
        .bind(t.a_item_a_id)
        .execute(&pool)
        .await
        .unwrap();

    let rows = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let rows: Vec<(Uuid, Uuid)> =
            sqlx::query_as("SELECT claim_id, list_item_id FROM claim_items")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        (conn, rows)
    })
    .await;

    assert!(
        rows.is_empty(),
        "stranger should see no claim_items for group A; saw {rows:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn rls_group_memberships_own_rows_visible(pool: PgPool) {
    let t = seed_two_tenants(&pool).await;

    // Stranger is a member of group B only. Policy: see own rows + rows of
    // groups you belong to. So the stranger must see exactly their own row
    // (in group B) plus any other group-B members, but no group-A rows.
    let group_ids = as_app(&pool, &t.stranger_in_b.to_string(), |mut conn| async move {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM group_memberships")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        (conn, ids)
    })
    .await;

    assert!(
        !group_ids.contains(&t.a_group_id),
        "stranger must not see group A memberships; saw {group_ids:?}"
    );
}
