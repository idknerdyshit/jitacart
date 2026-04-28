//! Market lookup. Markets are global (not group-scoped).

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use domain::{Market, MarketKind};
use uuid::Uuid;

use crate::{extract::CurrentUser, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/markets", get(list))
}

async fn list(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<Vec<Market>>, MarketError> {
    let rows: Vec<MarketRow> = sqlx::query_as(
        "SELECT id, kind, esi_location_id, region_id, name, short_label, is_hub, is_public \
         FROM markets WHERE is_public ORDER BY is_hub DESC, short_label",
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

#[derive(sqlx::FromRow)]
pub struct MarketRow {
    pub id: Uuid,
    pub kind: String,
    pub esi_location_id: i64,
    pub region_id: i64,
    pub name: String,
    pub short_label: String,
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
