use chrono::{DateTime, Utc};
use domain::{
    Claim, ClaimStatus, Fulfillment, FulfillmentSource, GroupRole, List, ListDetail, ListItem,
    ListItemStatus, ListStatus, LiveItemPrice, Market, MarketKind, Reimbursement,
    ReimbursementStatus,
};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::pricing::accessible_market_ids;
use crate::{db::Tx, errors::ApiError, state::AppState};

pub(crate) async fn load_list_detail(
    _state: &AppState,
    tx: &Tx,
    list_id: Uuid,
    viewer_user_id: Uuid,
    viewer_role: GroupRole,
) -> Result<ListDetail, ApiError> {
    let mut conn = tx.acquire().await;

    let list_row: ListRow = sqlx::query_as(
        "SELECT id, group_id, created_by_user_id, destination_label, notes, status, \
                total_estimate_isk, tip_pct, created_at, updated_at, \
                payer_corp_id, payer_division \
         FROM lists WHERE id = $1",
    )
    .bind(list_id)
    .fetch_optional(&mut **conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let group_id = list_row.group_id;
    let list = list_row.into_list();

    let item_rows: Vec<ListItemRow> = sqlx::query_as(
        "SELECT id, list_id, type_id, type_name, qty_requested, qty_fulfilled, \
                est_unit_price_isk, est_priced_market_id, status, source_line_no, \
                requested_by_user_id \
         FROM list_items WHERE list_id = $1 ORDER BY source_line_no NULLS LAST, created_at",
    )
    .bind(list_id)
    .fetch_all(&mut **conn)
    .await?;
    let items: Vec<ListItem> = item_rows.into_iter().map(ListItemRow::into_item).collect();

    let market_rows: Vec<MarketWithPrimaryRow> = sqlx::query_as(
        "SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label, \
                m.is_hub, m.is_public, lm.is_primary \
         FROM list_markets lm JOIN markets m ON m.id = lm.market_id \
         WHERE lm.list_id = $1 ORDER BY lm.is_primary DESC, m.short_label",
    )
    .bind(list_id)
    .fetch_all(&mut **conn)
    .await?;
    let primary_market_id = market_rows
        .iter()
        .find_map(|r| if r.is_primary { Some(r.id) } else { None })
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("list has no primary market")))?;
    let markets: Vec<Market> = market_rows
        .into_iter()
        .map(MarketWithPrimaryRow::into_market)
        .collect();

    let market_ids: Vec<Uuid> = markets.iter().map(|m| m.id).collect();
    let accessible: Vec<Uuid> = accessible_market_ids(&mut **conn, group_id, &market_ids)
        .await?
        .into_iter()
        .collect();

    let live_rows: Vec<LivePriceRow> = sqlx::query_as(
        r#"
        SELECT li.id          AS list_item_id,
               m.id           AS market_id,
               mp.best_sell,
               mp.best_buy,
               mp.sell_volume,
               mp.buy_volume,
               mp.computed_at
        FROM list_items li
        JOIN list_markets lm ON lm.list_id = li.list_id
        JOIN markets       m ON m.id       = lm.market_id
        LEFT JOIN market_prices mp
          ON mp.market_id = m.id
         AND mp.type_id   = li.type_id
         AND mp.market_id = ANY($2::uuid[])
        WHERE li.list_id = $1
        "#,
    )
    .bind(list_id)
    .bind(&accessible)
    .fetch_all(&mut **conn)
    .await?;
    let live_prices: Vec<LiveItemPrice> =
        live_rows.into_iter().map(LivePriceRow::into_live).collect();

    let claim_rows: Vec<ClaimRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.list_id, c.hauler_user_id, c.status, c.note,
               c.created_at, c.released_at,
               u.display_name AS hauler_display_name,
               ARRAY_AGG(ci.list_item_id) FILTER (WHERE ci.list_item_id IS NOT NULL AND ci.active)
                   AS item_ids
        FROM claims c
        JOIN users u ON u.id = c.hauler_user_id
        LEFT JOIN claim_items ci ON ci.claim_id = c.id
        WHERE c.list_id = $1
        GROUP BY c.id, u.display_name
        ORDER BY c.created_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&mut **conn)
    .await?;
    let claims: Vec<Claim> = claim_rows.into_iter().map(ClaimRow::into_claim).collect();

    // Non-reversed fulfillments only.
    let fulfillment_rows: Vec<FulfillmentRow> = sqlx::query_as(
        r#"
        SELECT f.id, f.list_item_id, f.claim_id, f.hauler_user_id, f.hauler_character_id,
               f.source, f.qty, f.unit_price_isk, f.bought_at_market_id, f.bought_at_note,
               f.bought_at, f.reversed_at,
               ch.character_name AS hauler_character_name,
               m.short_label     AS bought_at_market_short_label
        FROM fulfillments f
        JOIN list_items li ON li.id = f.list_item_id
        LEFT JOIN characters ch ON ch.id = f.hauler_character_id
        LEFT JOIN markets    m  ON m.id  = f.bought_at_market_id
        WHERE li.list_id = $1
          AND f.reversed_at IS NULL
        ORDER BY f.bought_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&mut **conn)
    .await?;
    let fulfillments: Vec<Fulfillment> = fulfillment_rows
        .into_iter()
        .map(FulfillmentRow::into_fulfillment)
        .collect();

    let reimbursement_rows: Vec<ReimbursementRow> = sqlx::query_as(
        r#"
        SELECT r.id, r.list_id, r.requester_user_id, r.hauler_user_id,
               r.subtotal_isk, r.tip_isk, r.total_isk, r.status,
               r.settled_at, r.settled_by_user_id, r.contract_id,
               r.created_at, r.updated_at,
               r.requester_principal_id, r.hauler_principal_id,
               r.is_corp_funded, r.verified_by_wallet, r.wallet_settlement_delta_isk,
               COALESCE(ru.display_name, corp_p.name, 'Corp') AS requester_display_name,
               hu.display_name AS hauler_display_name,
               c.esi_contract_id      AS contract_esi_contract_id,
               c.status               AS contract_status,
               c.price_isk            AS contract_price_isk,
               c.expected_total_isk   AS contract_expected_total_isk,
               c.settlement_delta_isk AS contract_settlement_delta_isk,
               c.date_completed       AS contract_date_completed
        FROM reimbursements r
        -- requester may be a user or a corp (corp-funded rows have no requester_user_id)
        LEFT JOIN users ru ON ru.id = r.requester_user_id
        LEFT JOIN principals rp ON rp.id = r.requester_principal_id AND rp.kind = 'corp'
        LEFT JOIN corps corp_p ON corp_p.id = rp.corp_id
        JOIN users hu ON hu.id = r.hauler_user_id
        LEFT JOIN contracts c ON c.id = r.contract_id
        WHERE r.list_id = $1
        ORDER BY r.created_at
        "#,
    )
    .bind(list_id)
    .fetch_all(&mut **conn)
    .await?;
    let reimbursements: Vec<Reimbursement> = reimbursement_rows
        .into_iter()
        .map(ReimbursementRow::into_reimbursement)
        .collect();

    let last_hauler_character_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT COALESCE(
            (SELECT f.hauler_character_id
             FROM fulfillments f
             JOIN list_items li ON li.id = f.list_item_id
             WHERE li.list_id = $1
               AND f.hauler_user_id = $2
               AND f.reversed_at IS NULL
               AND f.hauler_character_id IS NOT NULL
             ORDER BY f.bought_at DESC
             LIMIT 1),
            (SELECT id FROM characters WHERE user_id = $2 ORDER BY created_at ASC LIMIT 1)
        )
        "#,
    )
    .bind(list_id)
    .bind(viewer_user_id)
    .fetch_optional(&mut **conn)
    .await?
    .flatten();

    Ok(ListDetail {
        list,
        items,
        markets,
        primary_market_id,
        live_prices,
        claims,
        fulfillments,
        reimbursements,
        last_hauler_character_id,
        viewer_user_id,
        viewer_role,
    })
}

#[derive(sqlx::FromRow)]
struct ListRow {
    id: Uuid,
    group_id: Uuid,
    created_by_user_id: Uuid,
    destination_label: Option<String>,
    notes: Option<String>,
    status: ListStatus,
    total_estimate_isk: Decimal,
    tip_pct: Decimal,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    payer_corp_id: Option<Uuid>,
    payer_division: Option<i16>,
}

impl ListRow {
    fn into_list(self) -> List {
        List {
            id: self.id,
            group_id: self.group_id,
            created_by_user_id: self.created_by_user_id,
            destination_label: self.destination_label,
            notes: self.notes,
            status: self.status,
            total_estimate_isk: self.total_estimate_isk,
            tip_pct: self.tip_pct,
            created_at: self.created_at,
            updated_at: self.updated_at,
            payer_corp_id: self.payer_corp_id,
            payer_division: self.payer_division,
        }
    }
}

#[derive(sqlx::FromRow)]
struct ListItemRow {
    id: Uuid,
    list_id: Uuid,
    type_id: i64,
    type_name: String,
    qty_requested: i64,
    qty_fulfilled: i64,
    est_unit_price_isk: Option<Decimal>,
    est_priced_market_id: Option<Uuid>,
    status: ListItemStatus,
    source_line_no: Option<i32>,
    requested_by_user_id: Uuid,
}

impl ListItemRow {
    fn into_item(self) -> ListItem {
        ListItem {
            id: self.id,
            list_id: self.list_id,
            type_id: self.type_id,
            type_name: self.type_name,
            qty_requested: self.qty_requested,
            qty_fulfilled: self.qty_fulfilled,
            est_unit_price_isk: self.est_unit_price_isk,
            est_priced_market_id: self.est_priced_market_id,
            status: self.status,
            source_line_no: self.source_line_no,
            requested_by_user_id: self.requested_by_user_id,
        }
    }
}

#[derive(sqlx::FromRow)]
struct MarketWithPrimaryRow {
    id: Uuid,
    kind: MarketKind,
    esi_location_id: i64,
    region_id: Option<i64>,
    name: Option<String>,
    short_label: Option<String>,
    is_hub: bool,
    is_public: bool,
    is_primary: bool,
}

impl MarketWithPrimaryRow {
    fn into_market(self) -> Market {
        Market {
            id: self.id,
            kind: self.kind,
            esi_location_id: self.esi_location_id,
            region_id: self.region_id,
            name: self.name,
            short_label: self.short_label,
            is_hub: self.is_hub,
            is_public: self.is_public,
        }
    }
}

#[derive(sqlx::FromRow)]
struct LivePriceRow {
    list_item_id: Uuid,
    market_id: Uuid,
    best_sell: Option<Decimal>,
    best_buy: Option<Decimal>,
    sell_volume: Option<i64>,
    buy_volume: Option<i64>,
    computed_at: Option<DateTime<Utc>>,
}

impl LivePriceRow {
    fn into_live(self) -> LiveItemPrice {
        LiveItemPrice {
            list_item_id: self.list_item_id,
            market_id: self.market_id,
            best_sell: self.best_sell,
            best_buy: self.best_buy,
            sell_volume: self.sell_volume.unwrap_or(0),
            buy_volume: self.buy_volume.unwrap_or(0),
            computed_at: self.computed_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ClaimRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub hauler_user_id: Uuid,
    pub hauler_display_name: String,
    pub status: ClaimStatus,
    pub note: Option<String>,
    pub item_ids: Option<Vec<Uuid>>,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

impl ClaimRow {
    pub(crate) fn into_claim(self) -> Claim {
        Claim {
            id: self.id,
            list_id: self.list_id,
            hauler_user_id: self.hauler_user_id,
            hauler_display_name: self.hauler_display_name,
            status: self.status,
            note: self.note,
            item_ids: self.item_ids.unwrap_or_default(),
            created_at: self.created_at,
            released_at: self.released_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct FulfillmentRow {
    pub id: Uuid,
    pub list_item_id: Uuid,
    pub claim_id: Option<Uuid>,
    pub hauler_user_id: Uuid,
    pub hauler_character_id: Option<Uuid>,
    pub source: FulfillmentSource,
    pub qty: i64,
    pub unit_price_isk: Decimal,
    pub bought_at_market_id: Option<Uuid>,
    pub bought_at_note: Option<String>,
    pub bought_at: DateTime<Utc>,
    pub reversed_at: Option<DateTime<Utc>>,
    pub hauler_character_name: Option<String>,
    pub bought_at_market_short_label: Option<String>,
}

impl FulfillmentRow {
    pub(crate) fn into_fulfillment(self) -> Fulfillment {
        Fulfillment {
            id: self.id,
            list_item_id: self.list_item_id,
            claim_id: self.claim_id,
            hauler_user_id: self.hauler_user_id,
            hauler_character_id: self.hauler_character_id,
            hauler_character_name: self.hauler_character_name,
            source: self.source,
            qty: self.qty,
            unit_price_isk: self.unit_price_isk,
            bought_at_market_id: self.bought_at_market_id,
            bought_at_market_short_label: self.bought_at_market_short_label,
            bought_at_note: self.bought_at_note,
            bought_at: self.bought_at,
            reversed_at: self.reversed_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ReimbursementRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub requester_user_id: Option<Uuid>,
    pub hauler_user_id: Uuid,
    pub subtotal_isk: Decimal,
    pub tip_isk: Decimal,
    pub total_isk: Decimal,
    pub status: ReimbursementStatus,
    pub settled_at: Option<DateTime<Utc>>,
    pub settled_by_user_id: Option<Uuid>,
    pub contract_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub requester_display_name: String,
    pub hauler_display_name: String,
    pub contract_esi_contract_id: Option<i64>,
    pub contract_status: Option<domain::ContractStatus>,
    pub contract_price_isk: Option<Decimal>,
    pub contract_expected_total_isk: Option<Decimal>,
    pub contract_settlement_delta_isk: Option<Decimal>,
    pub contract_date_completed: Option<DateTime<Utc>>,
    pub requester_principal_id: Uuid,
    pub hauler_principal_id: Uuid,
    pub is_corp_funded: bool,
    pub verified_by_wallet: bool,
    pub wallet_settlement_delta_isk: Option<Decimal>,
}

impl ReimbursementRow {
    pub(crate) fn into_reimbursement(self) -> Reimbursement {
        let contract = match (
            self.contract_esi_contract_id,
            self.contract_status,
            self.contract_price_isk,
        ) {
            (Some(esi_id), Some(cstatus), Some(price)) => Some(domain::ContractSummary {
                esi_contract_id: esi_id,
                status: cstatus,
                price_isk: price,
                expected_total_isk: self.contract_expected_total_isk,
                settlement_delta_isk: self.contract_settlement_delta_isk,
                date_completed: self.contract_date_completed,
            }),
            _ => None,
        };
        Reimbursement {
            id: self.id,
            list_id: self.list_id,
            requester_user_id: self.requester_user_id,
            requester_display_name: self.requester_display_name,
            hauler_user_id: self.hauler_user_id,
            hauler_display_name: self.hauler_display_name,
            subtotal_isk: self.subtotal_isk,
            tip_isk: self.tip_isk,
            total_isk: self.total_isk,
            status: self.status,
            settled_at: self.settled_at,
            settled_by_user_id: self.settled_by_user_id,
            contract_id: self.contract_id,
            contract,
            created_at: self.created_at,
            updated_at: self.updated_at,
            requester_principal_id: self.requester_principal_id,
            hauler_principal_id: self.hauler_principal_id,
            is_corp_funded: self.is_corp_funded,
            verified_by_wallet: self.verified_by_wallet,
            wallet_settlement_delta_isk: self.wallet_settlement_delta_isk,
        }
    }
}
