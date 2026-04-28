//! EVE SSO + session handlers.

use anyhow::{anyhow, Context};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use nea_esi::auth::{EsiTokens, PkceChallenge};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tower_sessions::Session;
use uuid::Uuid;

use auth_tokens::TokenCipher;

use crate::{
    extract::{CurrentUser, SESSION_KEY_USER},
    jwt::EveClaims,
    state::AppState,
};

const SESSION_KEY_PENDING: &str = "pending_auth";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/eve/login", get(login))
        .route("/auth/eve/callback", get(callback))
        .route("/auth/eve/upgrade", get(upgrade))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
}

#[derive(Serialize, Deserialize)]
struct PendingAuth {
    code_verifier: String,
    state: String,
    attach: bool,
    return_to: Option<String>,
    /// When set, the callback verifies the returned `claims.character_id`
    /// matches this. Used by the upgrade flow so the user can't accidentally
    /// re-auth as a different character on the EVE consent screen.
    #[serde(default)]
    target_character_id: Option<i64>,
    /// Scopes requested for this consent flow. Validated against config
    /// (`login_scopes ∪ upgrade_scopes`) before redirecting to EVE.
    #[serde(default)]
    requested_scopes: Vec<String>,
}

#[derive(Deserialize)]
struct LoginQuery {
    #[serde(default)]
    attach: bool,
    return_to: Option<String>,
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    Query(q): Query<LoginQuery>,
) -> Result<Redirect, AuthError> {
    if q.attach {
        let user: Option<Uuid> = session.get(SESSION_KEY_USER).await.map_err(internal)?;
        if user.is_none() {
            return Err(AuthError::Unauthorized);
        }
    }

    let cfg = &state.config.eve_sso;
    let scopes: Vec<&str> = cfg.login_scopes.iter().map(String::as_str).collect();

    let challenge: PkceChallenge = state
        .esi
        .authorize_url(&cfg.callback_url, &scopes)
        .map_err(|e| AuthError::Internal(anyhow!("authorize_url: {e}")))?;

    let pending = PendingAuth {
        code_verifier: challenge.code_verifier.expose_secret().to_string(),
        state: challenge.state.clone(),
        attach: q.attach,
        return_to: safe_return_to(q.return_to),
        target_character_id: None,
        requested_scopes: cfg.login_scopes.clone(),
    };
    session
        .insert(SESSION_KEY_PENDING, &pending)
        .await
        .map_err(internal)?;

    Ok(Redirect::to(&challenge.authorize_url))
}

#[derive(Deserialize)]
struct UpgradeQuery {
    character_id: i64,
    /// Comma-separated list. Each must be in `login_scopes ∪ upgrade_scopes`.
    scopes: String,
    return_to: Option<String>,
}

async fn upgrade(
    State(state): State<AppState>,
    session: Session,
    CurrentUser(user_id): CurrentUser,
    Query(q): Query<UpgradeQuery>,
) -> Result<Redirect, AuthError> {
    // Confirm the character belongs to this user.
    let owns: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM characters WHERE character_id = $1 AND user_id = $2")
            .bind(q.character_id)
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
    if owns.is_none() {
        return Err(AuthError::Unauthorized);
    }

    let cfg = &state.config.eve_sso;
    let requested: Vec<String> = q
        .scopes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if requested.is_empty() {
        return Err(AuthError::Internal(anyhow!("no scopes requested")));
    }
    let allowed: std::collections::HashSet<&str> = cfg
        .login_scopes
        .iter()
        .chain(cfg.upgrade_scopes.iter())
        .map(String::as_str)
        .collect();
    for s in &requested {
        if !allowed.contains(s.as_str()) {
            return Err(AuthError::Internal(anyhow!("scope not allowed: {s}")));
        }
    }

    // EVE SSO replaces the granted set on each consent — always include base
    // scopes so we don't accidentally drop e.g. `publicData`.
    let mut all: std::collections::BTreeSet<String> = cfg.login_scopes.iter().cloned().collect();
    for s in &requested {
        all.insert(s.clone());
    }
    let scopes_vec: Vec<&str> = all.iter().map(String::as_str).collect();

    let challenge: PkceChallenge = state
        .esi
        .authorize_url(&cfg.callback_url, &scopes_vec)
        .map_err(|e| AuthError::Internal(anyhow!("authorize_url: {e}")))?;

    let pending = PendingAuth {
        code_verifier: challenge.code_verifier.expose_secret().to_string(),
        state: challenge.state.clone(),
        attach: true,
        return_to: safe_return_to(q.return_to),
        target_character_id: Some(q.character_id),
        requested_scopes: all.into_iter().collect(),
    };
    session
        .insert(SESSION_KEY_PENDING, &pending)
        .await
        .map_err(internal)?;

    Ok(Redirect::to(&challenge.authorize_url))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: String,
    state: String,
}

async fn callback(
    State(state): State<AppState>,
    session: Session,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, AuthError> {
    let pending: PendingAuth = session
        .remove(SESSION_KEY_PENDING)
        .await
        .map_err(internal)?
        .ok_or(AuthError::NoPending)?;

    if pending.state != q.state {
        return Err(AuthError::StateMismatch);
    }

    let cfg = &state.config.eve_sso;
    let verifier = secrecy::SecretString::from(pending.code_verifier);

    let tokens = state
        .esi
        .exchange_code(&q.code, &verifier, &cfg.callback_url)
        .await
        .map_err(|e| AuthError::Internal(anyhow!("exchange_code: {e}")))?;

    let claims = state
        .jwks
        .verify(tokens.access_token.expose_secret())
        .await
        .map_err(AuthError::Internal)?;

    if let Some(target) = pending.target_character_id {
        let returned = claims.character_id().map_err(AuthError::Internal)?;
        if returned != target {
            return Err(AuthError::WrongCharacter);
        }
    }

    let session_user: Option<Uuid> = session.get(SESSION_KEY_USER).await.map_err(internal)?;

    let user_id = upsert_character(
        &state.pool,
        &state.cipher,
        &claims,
        &tokens,
        session_user,
        pending.attach,
    )
    .await
    .map_err(AuthError::Internal)?;

    session
        .insert(SESSION_KEY_USER, user_id)
        .await
        .map_err(internal)?;

    Ok(Redirect::to(pending.return_to.as_deref().unwrap_or("/me")))
}

async fn logout(session: Session) -> Result<Redirect, AuthError> {
    session.flush().await.map_err(internal)?;
    Ok(Redirect::to("/"))
}

#[derive(Serialize)]
struct MeResponse {
    user: domain::User,
    characters: Vec<domain::Character>,
}

async fn me(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
) -> Result<Json<MeResponse>, AuthError> {
    let user_q = sqlx::query_as::<_, UserRow>(
        "SELECT id, display_name, created_at FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool);

    let chars_q = sqlx::query_as::<_, CharacterRow>(
        "SELECT id, user_id, character_id, character_name, owner_hash, scopes, \
                access_token_expires_at, created_at, last_refreshed_at \
         FROM characters WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&state.pool);

    let (user, characters) = tokio::try_join!(user_q, chars_q).map_err(internal)?;
    let user = user.ok_or(AuthError::Unauthorized)?;

    Ok(Json(MeResponse {
        user: user.into(),
        characters: characters.into_iter().map(Into::into).collect(),
    }))
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    display_name: String,
    created_at: DateTime<Utc>,
}

impl From<UserRow> for domain::User {
    fn from(r: UserRow) -> Self {
        domain::User {
            id: r.id,
            display_name: r.display_name,
            created_at: r.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CharacterRow {
    id: Uuid,
    user_id: Uuid,
    character_id: i64,
    character_name: String,
    owner_hash: String,
    scopes: Vec<String>,
    access_token_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    last_refreshed_at: Option<DateTime<Utc>>,
}

impl From<CharacterRow> for domain::Character {
    fn from(r: CharacterRow) -> Self {
        domain::Character {
            id: r.id,
            user_id: r.user_id,
            character_id: r.character_id,
            character_name: r.character_name,
            owner_hash: r.owner_hash,
            scopes: r.scopes,
            access_token_expires_at: r.access_token_expires_at,
            created_at: r.created_at,
            last_refreshed_at: r.last_refreshed_at,
        }
    }
}

async fn upsert_character(
    pool: &PgPool,
    cipher: &TokenCipher,
    claims: &EveClaims,
    tokens: &EsiTokens,
    session_user: Option<Uuid>,
    attach: bool,
) -> anyhow::Result<Uuid> {
    let character_id = claims.character_id()?;
    let scopes = claims.scopes();

    let mut tx = pool.begin().await?;

    let existing: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, owner_hash FROM characters WHERE character_id = $1 FOR UPDATE",
    )
    .bind(character_id)
    .fetch_optional(&mut *tx)
    .await
    .context("looking up existing character")?;

    let target_user_id = if attach {
        session_user.ok_or_else(|| anyhow!("attach without session"))?
    } else if let Some((_, uid, hash)) = &existing {
        if hash == &claims.owner {
            *uid
        } else if let Some(sid) = session_user {
            sid
        } else {
            create_user(&mut tx, &claims.name).await?
        }
    } else if let Some(sid) = session_user {
        sid
    } else {
        create_user(&mut tx, &claims.name).await?
    };

    let (rt_ct, rt_nonce) = cipher.encrypt(tokens.refresh_token.expose_secret().as_bytes())?;
    let (at_ct, at_nonce) = cipher.encrypt(tokens.access_token.expose_secret().as_bytes())?;

    sqlx::query(
        r#"
        INSERT INTO characters (
            user_id, character_id, character_name, owner_hash, scopes,
            refresh_token_ciphertext, refresh_token_nonce,
            access_token_ciphertext, access_token_nonce, access_token_expires_at,
            last_refreshed_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
        ON CONFLICT (character_id) DO UPDATE SET
            user_id = EXCLUDED.user_id,
            character_name = EXCLUDED.character_name,
            owner_hash = EXCLUDED.owner_hash,
            scopes = EXCLUDED.scopes,
            refresh_token_ciphertext = EXCLUDED.refresh_token_ciphertext,
            refresh_token_nonce = EXCLUDED.refresh_token_nonce,
            access_token_ciphertext = EXCLUDED.access_token_ciphertext,
            access_token_nonce = EXCLUDED.access_token_nonce,
            access_token_expires_at = EXCLUDED.access_token_expires_at,
            last_refreshed_at = now()
        "#,
    )
    .bind(target_user_id)
    .bind(character_id)
    .bind(&claims.name)
    .bind(&claims.owner)
    .bind(&scopes)
    .bind(&rt_ct)
    .bind(&rt_nonce)
    .bind(&at_ct)
    .bind(&at_nonce)
    .bind(tokens.expires_at)
    .execute(&mut *tx)
    .await
    .context("upserting character")?;

    tx.commit().await?;
    Ok(target_user_id)
}

async fn create_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    display_name: &str,
) -> anyhow::Result<Uuid> {
    sqlx::query_scalar("INSERT INTO users (display_name) VALUES ($1) RETURNING id")
        .bind(display_name)
        .fetch_one(&mut **tx)
        .await
        .context("creating user")
}

fn safe_return_to(return_to: Option<String>) -> Option<String> {
    let path = return_to?;
    if path.starts_with('/') && !path.starts_with("//") && !path.contains(['\r', '\n']) {
        Some(path)
    } else {
        None
    }
}

pub enum AuthError {
    Unauthorized,
    StateMismatch,
    NoPending,
    WrongCharacter,
    Internal(anyhow::Error),
}

fn internal<E: Into<anyhow::Error>>(e: E) -> AuthError {
    AuthError::Internal(e.into())
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        match self {
            AuthError::Unauthorized => (StatusCode::UNAUTHORIZED, "not logged in").into_response(),
            AuthError::StateMismatch => (StatusCode::BAD_REQUEST, "state mismatch").into_response(),
            AuthError::NoPending => {
                (StatusCode::BAD_REQUEST, "no pending auth in session").into_response()
            }
            AuthError::WrongCharacter => (
                StatusCode::BAD_REQUEST,
                "consent returned a different character than the one requested",
            )
                .into_response(),
            AuthError::Internal(e) => {
                tracing::error!(error = ?e, "auth handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
