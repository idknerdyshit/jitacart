//! Groups & invites.

use axum::{
    extract::{Path, State},
    http::StatusCode,
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
    errors::ApiError,
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
) -> Result<Json<Group>, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(ApiError::BadRequest("name must be 1–80 characters".into()));
    }

    require_group_quota(&state, user_id).await?;

    let mut tx = state.pool.begin().await?;
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
            Err(e) => return Err(ApiError::internal(e)),
        }
    }
    let group = group_row.ok_or_else(invite_exhausted)?.into_group();

    sqlx::query("INSERT INTO group_memberships (user_id, group_id, role) VALUES ($1, $2, 'owner')")
        .bind(user_id)
        .bind(group.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(Json(group))
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<Vec<GroupSummary>>, ApiError> {
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
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(GroupListRow::into_summary)
            .collect::<anyhow::Result<Vec<_>>>()?,
    ))
}

async fn detail(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<GroupDetail>, ApiError> {
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

    let (group, member_rows) = tokio::try_join!(group_q, members_q)?;
    let group = group.ok_or_else(ApiError::not_found)?.into_group();

    let members = member_rows
        .into_iter()
        .map(MemberRow::into_member)
        .collect::<anyhow::Result<Vec<_>>>()?;

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
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;

    let memberships = sqlx::query_as::<_, MembershipLockRow>(
        "SELECT user_id, role FROM group_memberships WHERE group_id = $1 FOR UPDATE",
    )
    .bind(group_id)
    .fetch_all(&mut *tx)
    .await?;

    let role = memberships
        .iter()
        .find(|m| m.user_id == user_id)
        .map(|m| m.role.as_str())
        .ok_or_else(|| ApiError::Forbidden("you are not a member of this group".into()))?
        .parse::<GroupRole>()
        .map_err(|e| ApiError::internal(anyhow::anyhow!(e)))?;

    if role == GroupRole::Owner {
        let other_owners = memberships
            .iter()
            .filter(|m| m.user_id != user_id && m.role == GroupRole::Owner.as_str())
            .count();
        if other_owners == 0 {
            return Err(ApiError::BadRequest(
                "you are the last owner; delete the group instead".into(),
            ));
        }
    }

    sqlx::query("DELETE FROM group_memberships WHERE user_id = $1 AND group_id = $2")
        .bind(user_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_group(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<StatusCode, ApiError> {
    if role != GroupRole::Owner {
        return Err(ApiError::Forbidden("owner role required".into()));
    }

    let result = sqlx::query("DELETE FROM groups WHERE id = $1")
        .bind(group_id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("group not found".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_invite(
    State(state): State<AppState>,
    CurrentGroup { group_id, role, .. }: CurrentGroup,
) -> Result<Json<Group>, ApiError> {
    if role != GroupRole::Owner {
        return Err(ApiError::Forbidden("owner role required".into()));
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
            Err(e) => return Err(ApiError::internal(e)),
        }
    }
    let group = group_row.ok_or_else(invite_exhausted)?.into_group();
    Ok(Json(group))
}

async fn join(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<Group>, ApiError> {
    require_group_quota(&state, user_id).await?;
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
    .await?
    .ok_or_else(|| ApiError::NotFound("invite is invalid or expired".into()))?
    .into_group();

    Ok(Json(group))
}

fn random_invite_code() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), INVITE_CODE_LEN)
}

/// Enforce `limits.groups_per_user`. Counts memberships, not just owned
/// groups — a user who joins 1000 groups via invite is just as much a
/// problem as one who creates 1000.
pub async fn check_group_quota(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    cap: i64,
) -> Result<(), ApiError> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM group_memberships WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    if count >= cap {
        return Err(ApiError::QuotaExceeded(format!(
            "you are already a member of {count} groups (limit {cap})"
        )));
    }
    Ok(())
}

async fn require_group_quota(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    check_group_quota(&state.pool, user_id, state.config.limits.groups_per_user).await
}

fn is_invite_collision(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.constraint() == Some(INVITE_CODE_UNIQUE))
}

fn invite_exhausted() -> ApiError {
    ApiError::internal(anyhow::anyhow!(
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
