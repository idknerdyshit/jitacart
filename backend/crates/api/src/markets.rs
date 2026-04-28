//! Market lookup.
//!
//! `GET /markets` — global, NPC hubs only (any logged-in user).
//! `GET /groups/:id/markets` — NPC hubs ∪ that group's tracked citadels.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{Market, MarketKind};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    extract::{CurrentGroup, CurrentUser},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/markets", get(list_global))
        .route("/groups/{id}/markets", get(list_for_group))
}

async fn list_global(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<Vec<Market>>, MarketError> {
    let rows: Vec<MarketRow> = sqlx::query_as(
        "SELECT id, kind, esi_location_id, region_id, name, short_label, is_hub, is_public \
         FROM markets WHERE is_public AND kind = 'npc_hub' \
         ORDER BY is_hub DESC, short_label",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    rows.into_iter()
        .map(MarketRow::into_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json)
        .map_err(internal)
}

#[derive(Serialize)]
pub struct GroupMarket {
    #[serde(flatten)]
    pub market: Market,
    pub last_orders_synced_at: Option<DateTime<Utc>>,
    pub untrackable_until: Option<DateTime<Utc>>,
    pub accessing_character_id: Option<Uuid>,
    pub accessing_character_name: Option<String>,
}

async fn list_for_group(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
) -> Result<Json<Vec<GroupMarket>>, MarketError> {
    let rows: Vec<GroupMarketRow> = sqlx::query_as(
        r#"
        SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label,
               m.is_hub, m.is_public, m.last_orders_synced_at, m.untrackable_until,
               NULL::uuid AS accessing_character_id,
               NULL::text AS accessing_character_name
        FROM markets m
        WHERE m.is_public AND m.kind = 'npc_hub'
        UNION ALL
        SELECT m.id, m.kind, m.esi_location_id, m.region_id, m.name, m.short_label,
               m.is_hub, m.is_public, m.last_orders_synced_at, m.untrackable_until,
               c.id, c.character_name
        FROM group_tracked_markets gtm
        JOIN markets m ON m.id = gtm.market_id
        LEFT JOIN LATERAL (
            SELECT c.id, c.character_name
            FROM characters c
            JOIN group_memberships gm ON gm.user_id = c.user_id AND gm.group_id = $1
            JOIN character_structure_access csa
              ON csa.character_id = c.id AND csa.market_id = m.id
            WHERE csa.market_status = 'ok'
            ORDER BY csa.market_checked_at DESC NULLS LAST
            LIMIT 1
        ) c ON true
        WHERE gtm.group_id = $1
        ORDER BY 7 DESC, 6 NULLS LAST
        "#,
    )
    .bind(group_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    rows.into_iter()
        .map(GroupMarketRow::into_group_market)
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json)
        .map_err(internal)
}

#[derive(sqlx::FromRow)]
pub struct MarketRow {
    pub id: Uuid,
    pub kind: String,
    pub esi_location_id: i64,
    pub region_id: Option<i64>,
    pub name: Option<String>,
    pub short_label: Option<String>,
    pub is_hub: bool,
    pub is_public: bool,
}

impl MarketRow {
    pub fn into_market(self) -> anyhow::Result<Market> {
        let kind = self
            .kind
            .parse::<MarketKind>()
            .map_err(anyhow::Error::msg)?;
        Ok(Market {
            id: self.id,
            kind,
            esi_location_id: self.esi_location_id,
            region_id: self.region_id,
            name: self.name,
            short_label: self.short_label,
            is_hub: self.is_hub,
            is_public: self.is_public,
        })
    }
}

#[derive(sqlx::FromRow)]
struct GroupMarketRow {
    id: Uuid,
    kind: String,
    esi_location_id: i64,
    region_id: Option<i64>,
    name: Option<String>,
    short_label: Option<String>,
    is_hub: bool,
    is_public: bool,
    last_orders_synced_at: Option<DateTime<Utc>>,
    untrackable_until: Option<DateTime<Utc>>,
    accessing_character_id: Option<Uuid>,
    accessing_character_name: Option<String>,
}

impl GroupMarketRow {
    fn into_group_market(self) -> anyhow::Result<GroupMarket> {
        let kind = self
            .kind
            .parse::<MarketKind>()
            .map_err(anyhow::Error::msg)?;
        Ok(GroupMarket {
            market: Market {
                id: self.id,
                kind,
                esi_location_id: self.esi_location_id,
                region_id: self.region_id,
                name: self.name,
                short_label: self.short_label,
                is_hub: self.is_hub,
                is_public: self.is_public,
            },
            last_orders_synced_at: self.last_orders_synced_at,
            untrackable_until: self.untrackable_until,
            accessing_character_id: self.accessing_character_id,
            accessing_character_name: self.accessing_character_name,
        })
    }
}

pub enum MarketError {
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> MarketError {
    MarketError::Internal(e.into())
}

impl IntoResponse for MarketError {
    fn into_response(self) -> Response {
        match self {
            MarketError::Internal(e) => {
                tracing::error!(error = ?e, "markets handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
