//! Citadel search + per-group tracked-market management.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{GroupRole, MarketKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{extract::CurrentGroup, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups/{id}/markets/citadels/search", get(search))
        .route("/groups/{id}/tracked-markets", get(list).post(track))
        .route("/groups/{id}/tracked-markets/{market_id}", delete(untrack))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
struct CitadelSearchHit {
    market_id: Uuid,
    name: String,
    short_label: Option<String>,
    region_id: Option<i64>,
    solar_system_id: Option<i64>,
    structure_type_id: Option<i32>,
    is_public: bool,
    tracked: bool,
}

async fn search(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<CitadelSearchHit>>, CitadelError> {
    let trimmed = q.q.trim();
    if trimmed.is_empty() {
        return Ok(Json(vec![]));
    }
    if trimmed.len() > 100 {
        return Err(CitadelError::BadRequest("query too long".into()));
    }
    let pattern = format!("%{}%", trimmed.replace(['%', '_'], ""));

    let rows: Vec<HitRow> = sqlx::query_as(
        r#"
        SELECT m.id, m.name, m.short_label, m.region_id, m.solar_system_id,
               m.structure_type_id, m.is_public,
               (gtm.market_id IS NOT NULL) AS tracked
        FROM markets m
        LEFT JOIN group_tracked_markets gtm
          ON gtm.market_id = m.id AND gtm.group_id = $1
        WHERE m.kind = 'public_structure'
          AND m.is_public = true
          AND m.details_synced_at IS NOT NULL
          AND m.name ILIKE $2
        ORDER BY tracked DESC, m.name
        LIMIT 25
        "#,
    )
    .bind(group_id)
    .bind(&pattern)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    Ok(Json(
        rows.into_iter()
            .map(|r| CitadelSearchHit {
                market_id: r.id,
                name: r.name.unwrap_or_default(),
                short_label: r.short_label,
                region_id: r.region_id,
                solar_system_id: r.solar_system_id,
                structure_type_id: r.structure_type_id,
                is_public: r.is_public,
                tracked: r.tracked,
            })
            .collect(),
    ))
}

#[derive(sqlx::FromRow)]
struct HitRow {
    id: Uuid,
    name: Option<String>,
    short_label: Option<String>,
    region_id: Option<i64>,
    solar_system_id: Option<i64>,
    structure_type_id: Option<i32>,
    is_public: bool,
    tracked: bool,
}

#[derive(Deserialize)]
struct TrackBody {
    market_id: Uuid,
}

async fn track(
    State(state): State<AppState>,
    cur: CurrentGroup,
    Json(body): Json<TrackBody>,
) -> Result<StatusCode, CitadelError> {
    require_owner(&cur)?;

    // Confirm the target is a public, detail-resolved citadel.
    let row: Option<(String, bool, bool)> = sqlx::query_as(
        "SELECT kind, is_public, (details_synced_at IS NOT NULL) FROM markets WHERE id = $1",
    )
    .bind(body.market_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let (kind, is_public, detailed) = row.ok_or(CitadelError::NotFound)?;
    let mk = kind
        .parse::<MarketKind>()
        .map_err(|e| CitadelError::Internal(anyhow::anyhow!("bad kind: {e}")))?;
    if mk != MarketKind::PublicStructure || !is_public || !detailed {
        return Err(CitadelError::BadRequest(
            "market is not a tracked-eligible public citadel".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO group_tracked_markets (group_id, market_id, added_by_user_id) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(cur.group_id)
    .bind(body.market_id)
    .bind(cur.user_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    Ok(StatusCode::CREATED)
}

async fn untrack(
    State(state): State<AppState>,
    cur: CurrentGroup,
    Path((_group_id, market_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, CitadelError> {
    require_owner(&cur)?;
    let r = sqlx::query("DELETE FROM group_tracked_markets WHERE group_id = $1 AND market_id = $2")
        .bind(cur.group_id)
        .bind(market_id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(CitadelError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct TrackedMarket {
    market_id: Uuid,
    name: Option<String>,
    short_label: Option<String>,
    region_id: Option<i64>,
    solar_system_id: Option<i64>,
    structure_type_id: Option<i32>,
    is_public: bool,
    last_orders_synced_at: Option<DateTime<Utc>>,
    accessing_character_id: Option<Uuid>,
    accessing_character_name: Option<String>,
    untrackable_until: Option<DateTime<Utc>>,
}

async fn list(
    State(state): State<AppState>,
    CurrentGroup { group_id, .. }: CurrentGroup,
) -> Result<Json<Vec<TrackedMarket>>, CitadelError> {
    let rows: Vec<TrackedRow> = sqlx::query_as(
        r#"
        SELECT m.id, m.name, m.short_label, m.region_id, m.solar_system_id,
               m.structure_type_id, m.is_public, m.last_orders_synced_at, m.untrackable_until,
               c.id   AS accessing_character_id,
               c.character_name AS accessing_character_name
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
        ORDER BY m.name
        "#,
    )
    .bind(group_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    Ok(Json(
        rows.into_iter()
            .map(|r| TrackedMarket {
                market_id: r.id,
                name: r.name,
                short_label: r.short_label,
                region_id: r.region_id,
                solar_system_id: r.solar_system_id,
                structure_type_id: r.structure_type_id,
                is_public: r.is_public,
                last_orders_synced_at: r.last_orders_synced_at,
                untrackable_until: r.untrackable_until,
                accessing_character_id: r.accessing_character_id,
                accessing_character_name: r.accessing_character_name,
            })
            .collect(),
    ))
}

#[derive(sqlx::FromRow)]
struct TrackedRow {
    id: Uuid,
    name: Option<String>,
    short_label: Option<String>,
    region_id: Option<i64>,
    solar_system_id: Option<i64>,
    structure_type_id: Option<i32>,
    is_public: bool,
    last_orders_synced_at: Option<DateTime<Utc>>,
    untrackable_until: Option<DateTime<Utc>>,
    accessing_character_id: Option<Uuid>,
    accessing_character_name: Option<String>,
}

fn require_owner(cur: &CurrentGroup) -> Result<(), CitadelError> {
    if cur.role == GroupRole::Owner {
        Ok(())
    } else {
        Err(CitadelError::Forbidden)
    }
}

pub enum CitadelError {
    BadRequest(String),
    Forbidden,
    NotFound,
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> CitadelError {
    CitadelError::Internal(e.into())
}

impl IntoResponse for CitadelError {
    fn into_response(self) -> Response {
        match self {
            CitadelError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            CitadelError::Forbidden => (StatusCode::FORBIDDEN, "owner only").into_response(),
            CitadelError::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            CitadelError::Internal(e) => {
                tracing::error!(error = ?e, "citadels handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
