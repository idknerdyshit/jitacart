//! Reusable extractors.

use axum::{
    extract::{FromRequestParts, Path},
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Response},
};
use domain::GroupRole;
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

        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM group_memberships WHERE user_id = $1 AND group_id = $2",
        )
        .bind(user_id)
        .bind(group_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "group membership lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        })?;

        let role = role
            .ok_or_else(|| {
                (StatusCode::FORBIDDEN, "you are not a member of this group").into_response()
            })?
            .parse::<GroupRole>()
            .map_err(|e| {
                tracing::error!(error = ?e, "invalid group role in database");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            })?;

        Ok(CurrentGroup {
            user_id,
            group_id,
            role,
        })
    }
}
