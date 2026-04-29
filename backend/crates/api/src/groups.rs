//! Groups & invites.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use domain::{Group, GroupMember, GroupRole};
use rand::distributions::{Alphanumeric, DistString};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    extract::{CurrentGroup, CurrentUser},
    state::AppState,
};

const INVITE_CODE_LEN: usize = 10;
const MAX_INVITE_GEN_ATTEMPTS: usize = 8;
const INVITE_CODE_UNIQUE: &str = "groups_invite_code_key";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/groups", post(create).get(list))
        .route("/groups/{id}", get(detail).delete(delete_group))
        .route("/groups/{id}/leave", post(leave))
        .route("/groups/{id}/rotate-invite", post(rotate_invite))
        .route("/groups/join/{code}", post(join))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
}

#[derive(Serialize)]
struct GroupSummary {
    #[serde(flatten)]
    group: Group,
    role: GroupRole,
    member_count: i64,
}

#[derive(Serialize)]
struct GroupDetail {
    group: Group,
    role: GroupRole,
    members: Vec<GroupMember>,
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Json(body): Json<CreateBody>,
) -> Result<Json<Group>, GroupError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(GroupError::BadRequest(
            "name must be 1–80 characters".into(),
        ));
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    let mut group_row: Option<GroupRow> = None;
    for _ in 0..MAX_INVITE_GEN_ATTEMPTS {
        let code = random_invite_code();
        match sqlx::query_as::<_, GroupRow>(
            "INSERT INTO groups (name, invite_code, created_by_user_id) \
             VALUES ($1, $2, $3) \
             RETURNING id, name, invite_code, created_by_user_id, created_at, default_tip_pct",
        )
        .bind(name)
        .bind(&code)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(row) => {
                group_row = Some(row);
                break;
            }
            Err(e) if is_invite_collision(&e) => continue,
            Err(e) => return Err(internal(e)),
        }
    }
    let group = group_row.ok_or_else(invite_exhausted)?.into_group();

    sqlx::query("INSERT INTO group_memberships (user_id, group_id, role) VALUES ($1, $2, 'owner')")
        .bind(user_id)
        .bind(group.id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

    tx.commit().await.map_err(internal)?;
    Ok(Json(group))
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<Vec<GroupSummary>>, GroupError> {
    let rows = sqlx::query_as::<_, GroupListRow>(
        r#"
        SELECT g.id, g.name, g.invite_code, g.created_by_user_id, g.created_at,
               g.default_tip_pct,
               m.role,
               (SELECT count(*) FROM group_memberships gm WHERE gm.group_id = g.id) AS member_count
        FROM groups g
        JOIN group_memberships m ON m.group_id = g.id
        WHERE m.user_id = $1
        ORDER BY g.created_at
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    rows.into_iter()
        .map(GroupListRow::into_summary)
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Json)
        .map_err(internal)
}

async fn detail(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<GroupDetail>, GroupError> {
    let group_q = sqlx::query_as::<_, GroupRow>(
        "SELECT id, name, invite_code, created_by_user_id, created_at, default_tip_pct \
         FROM groups WHERE id = $1",
    )
    .bind(group_id)
    .fetch_optional(&state.pool);

    let members_q = sqlx::query_as::<_, MemberRow>(
        r#"
        SELECT m.user_id, u.display_name, m.role, m.joined_at
        FROM group_memberships m
        JOIN users u ON u.id = m.user_id
        WHERE m.group_id = $1
        ORDER BY m.joined_at
        "#,
    )
    .bind(group_id)
    .fetch_all(&state.pool);

    let (group, member_rows) = tokio::try_join!(group_q, members_q).map_err(internal)?;
    let group = group.ok_or(GroupError::NotFound)?.into_group();

    let members = member_rows
        .into_iter()
        .map(MemberRow::into_member)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(internal)?;

    Ok(Json(GroupDetail {
        group,
        role,
        members,
    }))
}

async fn leave(
    State(state): State<AppState>,
    CurrentGroup {
        user_id, group_id, ..
    }: CurrentGroup,
) -> Result<StatusCode, GroupError> {
    let mut tx = state.pool.begin().await.map_err(internal)?;

    let memberships = sqlx::query_as::<_, MembershipLockRow>(
        "SELECT user_id, role FROM group_memberships WHERE group_id = $1 FOR UPDATE",
    )
    .bind(group_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    let role = memberships
        .iter()
        .find(|m| m.user_id == user_id)
        .map(|m| m.role.as_str())
        .ok_or(GroupError::NotMember)?
        .parse::<GroupRole>()
        .map_err(|e| internal(anyhow::anyhow!(e)))?;

    if role == GroupRole::Owner {
        let other_owners = memberships
            .iter()
            .filter(|m| m.user_id != user_id && m.role == GroupRole::Owner.as_str())
            .count();
        if other_owners == 0 {
            return Err(GroupError::BadRequest(
                "you are the last owner; delete the group instead".into(),
            ));
        }
    }

    sqlx::query("DELETE FROM group_memberships WHERE user_id = $1 AND group_id = $2")
        .bind(user_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_group(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<StatusCode, GroupError> {
    if role != GroupRole::Owner {
        return Err(GroupError::Forbidden);
    }

    let result = sqlx::query("DELETE FROM groups WHERE id = $1")
        .bind(group_id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;

    if result.rows_affected() == 0 {
        return Err(GroupError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_invite(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<Group>, GroupError> {
    if role != GroupRole::Owner {
        return Err(GroupError::Forbidden);
    }

    let mut group_row: Option<GroupRow> = None;
    for _ in 0..MAX_INVITE_GEN_ATTEMPTS {
        let code = random_invite_code();
        match sqlx::query_as::<_, GroupRow>(
            "UPDATE groups SET invite_code = $1 WHERE id = $2 \
             RETURNING id, name, invite_code, created_by_user_id, created_at, default_tip_pct",
        )
        .bind(&code)
        .bind(group_id)
        .fetch_one(&state.pool)
        .await
        {
            Ok(row) => {
                group_row = Some(row);
                break;
            }
            Err(e) if is_invite_collision(&e) => continue,
            Err(e) => return Err(internal(e)),
        }
    }
    let group = group_row.ok_or_else(invite_exhausted)?.into_group();
    Ok(Json(group))
}

async fn join(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<Group>, GroupError> {
    let group = sqlx::query_as::<_, GroupRow>(
        r#"
        WITH g AS (
            SELECT id, name, invite_code, created_by_user_id, created_at, default_tip_pct
            FROM groups WHERE invite_code = $1
        ),
        ins AS (
            INSERT INTO group_memberships (user_id, group_id, role)
            SELECT $2, id, 'member' FROM g
            ON CONFLICT (user_id, group_id) DO NOTHING
        )
        SELECT id, name, invite_code, created_by_user_id, created_at, default_tip_pct FROM g
        "#,
    )
    .bind(&code)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or(GroupError::InvalidInvite)?
    .into_group();

    Ok(Json(group))
}

fn random_invite_code() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), INVITE_CODE_LEN)
}

fn is_invite_collision(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.constraint() == Some(INVITE_CODE_UNIQUE))
}

fn invite_exhausted() -> GroupError {
    internal(anyhow::anyhow!(
        "could not generate a unique invite code after {MAX_INVITE_GEN_ATTEMPTS} tries"
    ))
}

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: Uuid,
    name: String,
    invite_code: String,
    created_by_user_id: Uuid,
    created_at: DateTime<Utc>,
    default_tip_pct: Decimal,
}

impl GroupRow {
    fn into_group(self) -> Group {
        Group {
            id: self.id,
            name: self.name,
            invite_code: self.invite_code,
            created_by_user_id: self.created_by_user_id,
            created_at: self.created_at,
            default_tip_pct: self.default_tip_pct,
        }
    }
}

#[derive(sqlx::FromRow)]
struct GroupListRow {
    id: Uuid,
    name: String,
    invite_code: String,
    created_by_user_id: Uuid,
    created_at: DateTime<Utc>,
    default_tip_pct: Decimal,
    role: String,
    member_count: i64,
}

impl GroupListRow {
    fn into_summary(self) -> anyhow::Result<GroupSummary> {
        let role = self.role.parse::<GroupRole>().map_err(anyhow::Error::msg)?;
        Ok(GroupSummary {
            group: Group {
                id: self.id,
                name: self.name,
                invite_code: self.invite_code,
                created_by_user_id: self.created_by_user_id,
                created_at: self.created_at,
                default_tip_pct: self.default_tip_pct,
            },
            role,
            member_count: self.member_count,
        })
    }
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    user_id: Uuid,
    display_name: String,
    role: String,
    joined_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct MembershipLockRow {
    user_id: Uuid,
    role: String,
}

impl MemberRow {
    fn into_member(self) -> anyhow::Result<GroupMember> {
        let role = self.role.parse::<GroupRole>().map_err(anyhow::Error::msg)?;
        Ok(GroupMember {
            user_id: self.user_id,
            display_name: self.display_name,
            role,
            joined_at: self.joined_at,
        })
    }
}

pub enum GroupError {
    BadRequest(String),
    NotFound,
    NotMember,
    Forbidden,
    InvalidInvite,
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> GroupError {
    GroupError::Internal(e.into())
}

impl IntoResponse for GroupError {
    fn into_response(self) -> Response {
        match self {
            GroupError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            GroupError::NotFound => (StatusCode::NOT_FOUND, "group not found").into_response(),
            GroupError::NotMember => {
                (StatusCode::FORBIDDEN, "you are not a member of this group").into_response()
            }
            GroupError::Forbidden => (StatusCode::FORBIDDEN, "owner role required").into_response(),
            GroupError::InvalidInvite => {
                (StatusCode::NOT_FOUND, "invite code not found").into_response()
            }
            GroupError::Internal(e) => {
                tracing::error!(error = ?e, "groups handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
