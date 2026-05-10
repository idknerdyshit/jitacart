//! Reusable extractors.

use axum::{
    extract::{FromRequestParts, Path},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
};
use domain::{ClaimStatus, GroupRole, ListStatus};

use crate::errors::ApiError;
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::state::AppState;

pub const SESSION_KEY_USER: &str = "user_id";

/// Extracts the logged-in user's id from the session, or returns 401.
pub struct CurrentUser(pub Uuid);

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        let user_id: Option<Uuid> = session.get(SESSION_KEY_USER).await.map_err(|e| {
            tracing::error!(error = ?e, "session lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response()
        })?;
        match user_id {
            Some(id) => Ok(CurrentUser(id)),
            None => Err((StatusCode::UNAUTHORIZED, "not logged in").into_response()),
        }
    }
}

fn db_error(context: &'static str, e: sqlx::Error) -> Response {
    tracing::error!(error = ?e, "{context}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

/// Extracts the logged-in user and verifies membership in the `{id}` group.
pub struct CurrentGroup {
    pub user_id: Uuid,
    pub group_id: Uuid,
    pub role: GroupRole,
}

#[derive(Deserialize)]
struct GroupPath {
    id: Option<Uuid>,
    group_id: Option<Uuid>,
}

impl FromRequestParts<AppState> for CurrentGroup {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user_id) = CurrentUser::from_request_parts(parts, state).await?;
        let Path(path) = Path::<GroupPath>::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        let group_id = path.group_id.or(path.id).ok_or_else(|| {
            (StatusCode::BAD_REQUEST, "missing group id in route").into_response()
        })?;

        let role: Option<GroupRole> = sqlx::query_scalar(
            "SELECT role FROM group_memberships WHERE user_id = $1 AND group_id = $2",
        )
        .bind(user_id)
        .bind(group_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| db_error("group membership lookup failed", e))?;

        let role = role.ok_or_else(|| {
            (StatusCode::FORBIDDEN, "you are not a member of this group").into_response()
        })?;

        Ok(CurrentGroup {
            user_id,
            group_id,
            role,
        })
    }
}

/// Looks up a claim by `{id}` path param and verifies the caller is a member
/// of the claim's list's group.
pub struct CurrentClaim {
    pub user_id: Uuid,
    pub group_id: Uuid,
    pub list_id: Uuid,
    pub claim_id: Uuid,
    pub hauler_user_id: Uuid,
    pub role: GroupRole,
    pub status: ClaimStatus,
    pub list_status: ListStatus,
}

#[derive(Deserialize)]
struct ClaimPath {
    id: Option<Uuid>,
    claim_id: Option<Uuid>,
}

#[allow(clippy::result_large_err)]
impl CurrentClaim {
    pub fn require_list_mutable(&self) -> Result<(), Response> {
        if self.list_status == ListStatus::Archived {
            return Err((
                StatusCode::CONFLICT,
                "list is archived; no changes can be made",
            )
                .into_response());
        }
        Ok(())
    }
}

impl FromRequestParts<AppState> for CurrentClaim {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user_id) = CurrentUser::from_request_parts(parts, state).await?;
        let Path(path) = Path::<ClaimPath>::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        let claim_id = path.claim_id.or(path.id).ok_or_else(|| {
            (StatusCode::BAD_REQUEST, "missing claim id in route").into_response()
        })?;

        let row: Option<(Uuid, Uuid, Uuid, ClaimStatus, Option<GroupRole>, ListStatus)> =
            sqlx::query_as(
                "SELECT c.list_id, l.group_id, c.hauler_user_id, c.status, gm.role, l.status \
                 FROM claims c \
                 JOIN lists l ON l.id = c.list_id \
                 LEFT JOIN group_memberships gm \
                   ON gm.group_id = l.group_id AND gm.user_id = $1 \
                 WHERE c.id = $2",
            )
            .bind(user_id)
            .bind(claim_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| db_error("claim lookup failed", e))?;

        let (list_id, group_id, hauler_user_id, status, role_opt, list_status) =
            row.ok_or_else(|| (StatusCode::NOT_FOUND, "claim not found").into_response())?;

        let role = role_opt.ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                "you are not a member of this claim's group",
            )
                .into_response()
        })?;

        Ok(CurrentClaim {
            user_id,
            group_id,
            list_id,
            claim_id,
            hauler_user_id,
            role,
            status,
            list_status,
        })
    }
}

/// Looks up the list by `{id}` path param and verifies the caller is a
/// member of the list's group, in a single JOIN.
pub struct CurrentList {
    pub user_id: Uuid,
    /// Surfaced for handlers that need to scope further DB queries by group.
    pub group_id: Uuid,
    pub list_id: Uuid,
    pub role: GroupRole,
    pub created_by_user_id: Uuid,
    pub status: ListStatus,
}

#[allow(clippy::result_large_err)] // Response is the standard axum error type.
impl CurrentList {
    pub fn require_open(&self) -> Result<(), ApiError> {
        if self.status != ListStatus::Open {
            return Err(ApiError::Conflict(format!(
                "list is {}; this action requires an open list",
                self.status
            )));
        }
        Ok(())
    }

    pub fn require_mutable(&self) -> Result<(), ApiError> {
        if self.status == ListStatus::Archived {
            return Err(ApiError::Conflict(
                "list is archived; no changes can be made".into(),
            ));
        }
        Ok(())
    }

    pub fn require_can_manage(&self) -> Result<(), ApiError> {
        if self.user_id != self.created_by_user_id && self.role != GroupRole::Owner {
            return Err(ApiError::Forbidden(
                "only the list creator or a group owner can change list status".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct ListPath {
    id: Option<Uuid>,
    list_id: Option<Uuid>,
}

impl FromRequestParts<AppState> for CurrentList {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user_id) = CurrentUser::from_request_parts(parts, state).await?;
        let Path(path) = Path::<ListPath>::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        let list_id = path
            .list_id
            .or(path.id)
            .ok_or_else(|| (StatusCode::BAD_REQUEST, "missing list id in route").into_response())?;

        // Outer-join membership so a missing list returns 404 while a non-member
        // on an existing list returns 403 (role IS NULL).
        let row: Option<(Uuid, Uuid, ListStatus, Option<GroupRole>)> = sqlx::query_as(
            "SELECT l.group_id, l.created_by_user_id, l.status, gm.role \
             FROM lists l \
             LEFT JOIN group_memberships gm \
               ON gm.group_id = l.group_id AND gm.user_id = $1 \
             WHERE l.id = $2",
        )
        .bind(user_id)
        .bind(list_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| db_error("list lookup failed", e))?;

        let (group_id, created_by_user_id, status, role) =
            row.ok_or_else(|| (StatusCode::NOT_FOUND, "list not found").into_response())?;

        let role = role.ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                "you are not a member of this list's group",
            )
                .into_response()
        })?;

        Ok(CurrentList {
            user_id,
            group_id,
            list_id,
            role,
            created_by_user_id,
            status,
        })
    }
}
