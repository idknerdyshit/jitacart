//! Shared types/helpers for the character and corp contract pollers.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;
use webhook_dispatch::WebhookEvent;

use crate::Ctx;
use domain::{ContractStatus, Principal, PrincipalIndex, PrincipalKind};

#[derive(Clone)]
pub(crate) struct UpsertedContract {
    pub contract_id: uuid::Uuid,
    pub esi_contract_id: i64,
    pub status: ContractStatus,
    pub status_changed: bool,
}

/// Settle or unbind a contract that just transitioned to a terminal status.
///
/// On settlement, webhook payloads are persisted to `pending_webhooks` in the
/// same transaction so neither side can drift if the worker crashes between
/// commit and HTTP delivery. The drainer job (`pending_webhooks::Job`) does
/// the actual sending.
pub(crate) async fn handle_contract_terminal(
    ctx: &Ctx,
    u: &UpsertedContract,
) -> anyhow::Result<()> {
    if u.status.is_terminal_success() {
        let mut tx = ctx.pool.begin().await?;
        let settled = settlement::settle_via_contract(&mut tx, u.contract_id)
            .await
            .map_err(|e| anyhow::anyhow!("settle_via_contract: {e}"))?;
        for info in settled {
            let event = WebhookEvent::ReimbursementSettled {
                list_destination: info.list_destination,
                requester_name: info.requester_name,
                hauler_name: info.hauler_name,
                total_isk: info.total_isk.to_string(),
            };
            let payload = serde_json::to_value(&event)
                .map_err(|e| anyhow::anyhow!("serialize webhook payload: {e}"))?;
            sqlx::query("INSERT INTO pending_webhooks (group_id, payload) VALUES ($1, $2)")
                .bind(info.group_id)
                .bind(&payload)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    } else if u.status.is_terminal_failure() {
        let mut tx = ctx.pool.begin().await?;
        settlement::unbind_contract(&mut tx, u.contract_id)
            .await
            .map_err(|e| anyhow::anyhow!("unbind_contract: {e}"))?;
        tx.commit().await?;
    }
    Ok(())
}

/// Build a lightweight `PrincipalIndex` for a slice of EVE entity ids.
pub(crate) async fn build_principal_index(
    pool: &PgPool,
    eve_ids: &[i64],
) -> anyhow::Result<PrincipalIndex> {
    let mut idx = PrincipalIndex::default();
    if eve_ids.is_empty() {
        return Ok(idx);
    }

    // Characters → users.
    let char_rows: Vec<(i64, Uuid)> = sqlx::query_as(
        "SELECT character_id, user_id FROM characters WHERE character_id = ANY($1::bigint[])",
    )
    .bind(eve_ids)
    .fetch_all(pool)
    .await?;

    let user_ids: Vec<Uuid> = char_rows.iter().map(|(_, uid)| *uid).collect();
    let user_principals: Vec<(Uuid, Uuid)> = if user_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, user_id FROM principals \
             WHERE kind = 'user' AND user_id = ANY($1::uuid[])",
        )
        .bind(&user_ids)
        .fetch_all(pool)
        .await?
    };

    let user_principal_by_user: HashMap<Uuid, Uuid> = user_principals
        .into_iter()
        .map(|(pid, uid)| (uid, pid))
        .collect();
    for (char_id, user_id) in char_rows {
        if let Some(pid) = user_principal_by_user.get(&user_id) {
            idx.add_user(
                domain::EsiCharacterId(char_id),
                user_id,
                Principal {
                    id: *pid,
                    kind: PrincipalKind::User,
                    user_id: Some(user_id),
                    corp_id: None,
                },
            );
        }
    }

    // Corps.
    let corp_rows: Vec<(i64, Uuid)> = sqlx::query_as(
        "SELECT esi_corporation_id, id FROM corps \
         WHERE esi_corporation_id = ANY($1::bigint[])",
    )
    .bind(eve_ids)
    .fetch_all(pool)
    .await?;

    let corp_ids: Vec<Uuid> = corp_rows.iter().map(|(_, cid)| *cid).collect();
    let corp_principals: Vec<(Uuid, Uuid)> = if corp_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, corp_id FROM principals \
             WHERE kind = 'corp' AND corp_id = ANY($1::uuid[])",
        )
        .bind(&corp_ids)
        .fetch_all(pool)
        .await?
    };

    let corp_principal_by_corp: HashMap<Uuid, Uuid> = corp_principals
        .into_iter()
        .map(|(pid, cid)| (cid, pid))
        .collect();
    for (esi_corp_id, corp_id) in corp_rows {
        if let Some(pid) = corp_principal_by_corp.get(&corp_id) {
            idx.add_corp(
                domain::EsiCorporationId(esi_corp_id),
                corp_id,
                Principal {
                    id: *pid,
                    kind: PrincipalKind::Corp,
                    user_id: None,
                    corp_id: Some(corp_id),
                },
            );
        }
    }

    Ok(idx)
}

pub(crate) fn find_user_for_principal(idx: &PrincipalIndex, principal_id: Uuid) -> Option<Uuid> {
    idx.principal_by_user_id
        .values()
        .find(|p| p.id == principal_id)
        .and_then(|p| p.user_id)
}
