//! EVE SSO + session handlers.

use anyhow::{anyhow, Context};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::Redirect,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use nea_esi::auth::{EsiTokens, PkceChallenge};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use auth_tokens::MultiKeyCipher;

use crate::{
    db::Tx,
    errors::ApiError,
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
        .route("/me/active-character", patch(set_active_character))
        .route("/me/export", get(export_me))
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
    /// Cloudflare Turnstile token. Required for first-time logins (no
    /// existing session, not attaching). Frontend renders the widget,
    /// captures the token, and forwards it on the redirect to /auth/eve/login.
    cf: Option<String>,
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> Result<Redirect, ApiError> {
    let session_user: Option<Uuid> = session.get(SESSION_KEY_USER).await?;
    if q.attach {
        if session_user.is_none() {
            return Err(ApiError::Unauthorized);
        }
    } else if session_user.is_none() && !state.config.turnstile.disabled {
        // Turnstile gates only fresh-browser SSO inits — that is, this branch
        // requires `session_user.is_none()`. A logged-in user re-initiating
        // SSO (the `?attach=` "add another character" path, or a re-auth from
        // an authenticated session) hits the `q.attach` arm above or skips
        // this branch entirely, because they have already proven they are
        // human in this session.
        //
        // Threat model: the captcha exists to keep automated tools from
        // burning ESI authorize calls / harvesting state cookies at scale.
        // Once a session cookie exists we trust it — replaying captcha on
        // every SSO init for an already-authenticated user would only
        // friction legitimate flows without changing the attacker's cost.
        // The cost is therefore one-per-fresh-browser (no session cookie),
        // not one-per-session-cookie-expiry.
        let token = q.cf.as_deref().ok_or(ApiError::Forbidden(
            "captcha verification required for new accounts".into(),
        ))?;
        // X-Forwarded-For is set by the trusted reverse proxy (Caddy); take
        // the first hop. Behind a misconfigured proxy this is None, which
        // Turnstile accepts.
        let remote_ip: Option<&str> = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(str::trim);
        let result = crate::turnstile::verify(
            &state.webhook_http,
            &state.config.turnstile.secret_key,
            token,
            remote_ip,
        )
        .await?;
        if !result.success {
            tracing::warn!(
                error_codes = ?result.error_codes,
                "turnstile verification failed"
            );
            return Err(ApiError::Forbidden(
                "captcha verification required for new accounts".into(),
            ));
        }
    }

    let cfg = &state.config.eve_sso;
    let scopes: Vec<&str> = cfg.login_scopes.iter().map(String::as_str).collect();

    let challenge: PkceChallenge = state
        .esi
        .authorize_url(&cfg.callback_url, &scopes)
        .map_err(|e| ApiError::Internal(anyhow!("authorize_url: {e}")))?;

    let pending = PendingAuth {
        code_verifier: challenge.code_verifier.expose_secret().to_string(),
        state: challenge.state.clone(),
        attach: q.attach,
        return_to: safe_return_to(q.return_to),
        target_character_id: None,
        requested_scopes: cfg.login_scopes.clone(),
    };
    session.insert(SESSION_KEY_PENDING, &pending).await?;

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
    tx: Tx,
    Query(q): Query<UpgradeQuery>,
) -> Result<Redirect, ApiError> {
    let cfg = &state.config.eve_sso;
    let requested: Vec<String> = q
        .scopes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if requested.is_empty() {
        return Err(ApiError::BadRequest("no scopes requested".into()));
    }
    let allowed: std::collections::HashSet<&str> = cfg
        .login_scopes
        .iter()
        .chain(cfg.upgrade_scopes.iter())
        .map(String::as_str)
        .collect();
    for s in &requested {
        if !allowed.contains(s.as_str()) {
            return Err(ApiError::Internal(anyhow!("scope not allowed: {s}")));
        }
    }

    // Single query that both confirms ownership and reads existing scopes.
    // EVE SSO replaces the granted set on each consent — we merge base
    // login_scopes (so we don't drop `publicData`) and the character's
    // already-granted scopes (so a corp-scope upgrade doesn't silently
    // revoke e.g. `esi-markets.structure_markets.v1`).
    let mut conn = tx.acquire().await;
    let existing_scopes: Vec<String> = sqlx::query_scalar(
        "SELECT scopes FROM characters WHERE character_id = $1 AND user_id = $2",
    )
    .bind(q.character_id)
    .bind(user_id)
    .fetch_optional(&mut **conn)
    .await?
    .ok_or(ApiError::Unauthorized)?;

    let all = merge_upgrade_scopes(&cfg.login_scopes, &existing_scopes, &requested);
    let scopes_vec: Vec<&str> = all.iter().map(String::as_str).collect();

    let challenge: PkceChallenge = state
        .esi
        .authorize_url(&cfg.callback_url, &scopes_vec)
        .map_err(|e| ApiError::Internal(anyhow!("authorize_url: {e}")))?;

    let pending = PendingAuth {
        code_verifier: challenge.code_verifier.expose_secret().to_string(),
        state: challenge.state.clone(),
        attach: true,
        return_to: safe_return_to(q.return_to),
        target_character_id: Some(q.character_id),
        requested_scopes: all,
    };
    session.insert(SESSION_KEY_PENDING, &pending).await?;

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
    tx: Tx,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, ApiError> {
    let pending: PendingAuth = session
        .remove(SESSION_KEY_PENDING)
        .await?
        .ok_or_else(|| ApiError::BadRequest("no pending auth in session".into()))?;

    if pending.state != q.state {
        return Err(ApiError::BadRequest("state mismatch".into()));
    }

    let cfg = &state.config.eve_sso;
    let verifier = secrecy::SecretString::from(pending.code_verifier);

    let tokens = state
        .esi
        .exchange_code(&q.code, &verifier, &cfg.callback_url)
        .await
        .map_err(|e| ApiError::Internal(anyhow!("exchange_code: {e}")))?;

    let claims = state
        .jwks
        .verify(tokens.access_token.expose_secret())
        .await?;

    if let Some(target) = pending.target_character_id {
        let returned = claims.character_id()?;
        if returned != target {
            return Err(ApiError::BadRequest(
                "consent returned a different character than the one requested".into(),
            ));
        }
    }

    let session_user: Option<Uuid> = session.get(SESSION_KEY_USER).await?;

    let mut conn = tx.acquire().await;
    let user_id = upsert_character(
        &mut *conn,
        &state.cipher,
        &claims,
        &tokens,
        session_user,
        pending.attach,
        state.config.limits.characters_per_user,
    )
    .await?;
    drop(conn);

    session.insert(SESSION_KEY_USER, user_id).await?;

    // Re-validate even though the value was sanitized before being stored in
    // the signed session — cheap defense-in-depth against a session-shape
    // change or a future code path that stores an unsanitized value.
    let return_to = safe_return_to(pending.return_to);
    Ok(Redirect::to(return_to.as_deref().unwrap_or("/me")))
}

async fn logout(session: Session) -> Result<Redirect, ApiError> {
    session.flush().await?;
    Ok(Redirect::to("/"))
}

#[derive(Serialize)]
struct MeResponse {
    user: domain::User,
    characters: Vec<domain::Character>,
    active_character_id: Option<Uuid>,
}

async fn me(
    State(_state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
) -> Result<Json<MeResponse>, ApiError> {
    let mut conn = tx.acquire().await;
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, display_name, active_character_id, created_at FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut **conn)
    .await?
    .ok_or(ApiError::Unauthorized)?;

    let characters = sqlx::query_as::<_, CharacterRow>(
        "SELECT id, user_id, character_id, character_name, owner_hash, scopes, \
                access_token_expires_at, created_at, last_refreshed_at \
         FROM characters WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&mut **conn)
    .await?;

    let active_character_id = user.active_character_id;
    Ok(Json(MeResponse {
        user: user.into(),
        characters: characters.into_iter().map(Into::into).collect(),
        active_character_id,
    }))
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    display_name: String,
    active_character_id: Option<Uuid>,
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
            character_id: domain::EsiCharacterId(r.character_id),
            character_name: r.character_name,
            owner_hash: r.owner_hash,
            scopes: r.scopes,
            access_token_expires_at: r.access_token_expires_at,
            created_at: r.created_at,
            last_refreshed_at: r.last_refreshed_at,
        }
    }
}

#[derive(Deserialize)]
struct SetActiveCharacterBody {
    character_id: Option<Uuid>,
}

async fn set_active_character(
    State(state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
    Json(body): Json<SetActiveCharacterBody>,
) -> Result<Json<MeResponse>, ApiError> {
    let mut conn = tx.acquire().await;
    do_set_active_character(&mut **conn, user_id, body.character_id).await?;
    drop(conn);
    me(State(state), CurrentUser(user_id), tx).await
}

/// Update the user's `active_character_id`. Ownership is enforced by
/// constraining the UPDATE to rows where the character (if supplied)
/// belongs to the same user; if no row matches we return `Unauthorized`
/// so callers see a 401 rather than a silent no-op.
pub async fn do_set_active_character(
    executor: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
    character_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let result = sqlx::query(
        "UPDATE users SET active_character_id = $1 WHERE id = $2 \
         AND ($1::uuid IS NULL OR EXISTS \
              (SELECT 1 FROM characters WHERE id = $1 AND user_id = $2))",
    )
    .bind(character_id)
    .bind(user_id)
    .execute(executor)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

/// User-data export. Returns a JSON document of everything keyed to
/// the caller's `users.id` — for personal-data review or self-service
/// off-boarding. Token plaintext is *never* included; only metadata
/// like character names, scopes, and expiry timestamps.
pub async fn do_export_me(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
) -> anyhow::Result<serde_json::Value> {
    let user: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(u) - 'active_character_id' \
         FROM (SELECT id, display_name, created_at, active_character_id FROM users \
               WHERE id = $1) u",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| anyhow!("user {user_id} not found"))?;

    // Characters: metadata only. Token columns explicitly excluded.
    let characters: serde_json::Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(c ORDER BY c.created_at), '[]'::jsonb) FROM ( \
            SELECT id, character_id, character_name, owner_hash, scopes, \
                   access_token_expires_at, created_at, last_refreshed_at, \
                   token_key_id \
            FROM characters WHERE user_id = $1 \
         ) c",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;

    let memberships: serde_json::Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(m ORDER BY m.joined_at), '[]'::jsonb) FROM ( \
            SELECT g.id AS group_id, g.name AS group_name, gm.role, gm.joined_at \
            FROM group_memberships gm \
            JOIN groups g ON g.id = gm.group_id \
            WHERE gm.user_id = $1 \
         ) m",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;

    let lists: serde_json::Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(l ORDER BY l.created_at DESC), '[]'::jsonb) FROM ( \
            SELECT id, group_id, destination_label, status, total_estimate_isk, \
                   created_at, updated_at \
            FROM lists WHERE created_by_user_id = $1 \
         ) l",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;

    let fulfillments: serde_json::Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(f ORDER BY f.bought_at DESC), '[]'::jsonb) FROM ( \
            SELECT f.id, f.list_item_id, f.qty, f.unit_price_isk, \
                   f.bought_at_market_id, f.bought_at, f.reversed_at \
            FROM fulfillments f \
            WHERE f.hauler_user_id = $1 \
         ) f",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;

    let reimbursements: serde_json::Value = sqlx::query_scalar(
        "SELECT coalesce(jsonb_agg(r ORDER BY r.id), '[]'::jsonb) FROM ( \
            SELECT id, list_id, status, total_isk, contract_id, settled_at \
            FROM reimbursements \
            WHERE requester_user_id = $1 OR hauler_user_id = $1 \
         ) r",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;

    Ok(serde_json::json!({
        "exported_at": chrono::Utc::now(),
        "user": user,
        "characters": characters,
        "group_memberships": memberships,
        "lists_created": lists,
        "fulfillments_as_hauler": fulfillments,
        "reimbursements_involving_me": reimbursements,
    }))
}

async fn export_me(
    State(_state): State<AppState>,
    CurrentUser(user_id): CurrentUser,
    tx: Tx,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = tx.acquire().await;
    let result = do_export_me(&mut **conn, user_id).await?;
    drop(conn);
    Ok(Json(result))
}

async fn upsert_character(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    cipher: &MultiKeyCipher,
    claims: &EveClaims,
    tokens: &EsiTokens,
    session_user: Option<Uuid>,
    attach: bool,
    characters_cap: i64,
) -> Result<Uuid, ApiError> {
    let character_id = claims.character_id()?;
    let scopes = claims.scopes();

    let existing: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, owner_hash FROM characters WHERE character_id = $1 FOR UPDATE",
    )
    .bind(character_id)
    .fetch_optional(&mut **tx)
    .await
    .context("looking up existing character")?;

    let target_user_id = if attach {
        session_user.ok_or_else(|| ApiError::internal(anyhow!("attach without session")))?
    } else if let Some((_, uid, hash)) = &existing {
        if hash == &claims.owner {
            *uid
        } else if let Some(sid) = session_user {
            sid
        } else {
            create_user(tx, &claims.name).await?
        }
    } else if let Some(sid) = session_user {
        sid
    } else {
        create_user(tx, &claims.name).await?
    };

    // Enforce characters_per_user when this insert would add a NEW row under
    // target_user_id. An owner-hash transfer (existing.user_id != target) is
    // also "moving in," so it counts. Re-login on the same row is exempt.
    let would_add_new = match &existing {
        None => true,
        Some((_, uid, _)) => *uid != target_user_id,
    };
    if would_add_new {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM characters WHERE user_id = $1")
            .bind(target_user_id)
            .fetch_one(&mut **tx)
            .await?;
        if count >= characters_cap {
            return Err(ApiError::QuotaExceeded(format!(
                "you already have {count} linked characters (cap {characters_cap})"
            )));
        }
    }

    let aad = character_id.to_be_bytes();
    let (rt_ct, rt_nonce, kid) =
        cipher.encrypt(tokens.refresh_token.expose_secret().as_bytes(), &aad)?;
    let (at_ct, at_nonce, _) =
        cipher.encrypt(tokens.access_token.expose_secret().as_bytes(), &aad)?;

    sqlx::query(
        r#"
        INSERT INTO characters (
            user_id, character_id, character_name, owner_hash, scopes,
            refresh_token_ciphertext, refresh_token_nonce,
            access_token_ciphertext, access_token_nonce, access_token_expires_at,
            token_key_id, last_refreshed_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
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
            token_key_id = EXCLUDED.token_key_id,
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
    .bind(&kid)
    .execute(&mut **tx)
    .await
    .context("upserting character")?;

    Ok(target_user_id)
}

async fn create_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    display_name: &str,
) -> anyhow::Result<Uuid> {
    let user_id: Uuid =
        sqlx::query_scalar("INSERT INTO users (display_name) VALUES ($1) RETURNING id")
            .bind(display_name)
            .fetch_one(&mut **tx)
            .await
            .context("creating user")?;

    sqlx::query(
        "INSERT INTO principals (kind, user_id) VALUES ('user', $1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .execute(&mut **tx)
    .await
    .context("creating user principal")?;

    Ok(user_id)
}

/// Merge login scopes, existing character scopes, and the newly requested
/// scopes into a deduplicated sorted set. Extracted for unit-testing the
/// granted-set regression fix.
pub fn merge_upgrade_scopes(
    login_scopes: &[String],
    existing_character_scopes: &[String],
    requested: &[String],
) -> Vec<String> {
    let mut all: std::collections::BTreeSet<String> = login_scopes.iter().cloned().collect();
    for s in existing_character_scopes {
        all.insert(s.clone());
    }
    for s in requested {
        all.insert(s.clone());
    }
    all.into_iter().collect()
}

fn safe_return_to(return_to: Option<String>) -> Option<String> {
    let path = return_to?;
    // Must be a site-relative path. Reject:
    //  - protocol-relative `//host` and (after browser backslash normalization)
    //    `/\host` / `\\host`, which redirect off-site;
    //  - any backslash, since browsers normalize `\` to `/` in the Location
    //    header before resolving;
    //  - CRLF, to block header injection.
    if path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains(['\r', '\n'])
    {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(scopes: &[&str]) -> Vec<String> {
        scopes.iter().map(|s| s.to_string()).collect()
    }

    /// Regression: upgrading to corp scopes must not drop already-granted
    /// market scopes — or anything else the character already held.
    #[test]
    fn merge_preserves_existing_scopes() {
        let login = sv(&["publicData"]);
        let existing = sv(&[
            "publicData",
            "esi-markets.structure_markets.v1",
            "esi-contracts.read_character_contracts.v1",
        ]);
        let requested = sv(&[
            "esi-contracts.read_corporation_contracts.v1",
            "esi-wallet.read_corporation_wallets.v1",
        ]);
        let merged = merge_upgrade_scopes(&login, &existing, &requested);
        // All three origin-sets must be present in the output.
        for s in existing.iter().chain(requested.iter()) {
            assert!(
                merged.contains(s),
                "scope {s} missing from merged set: {merged:?}"
            );
        }
    }

    #[test]
    fn merge_deduplicates() {
        let login = sv(&["publicData"]);
        let existing = sv(&["publicData", "esi-contracts.read_character_contracts.v1"]);
        let requested = sv(&["publicData"]);
        let merged = merge_upgrade_scopes(&login, &existing, &requested);
        let count = merged.iter().filter(|s| s.as_str() == "publicData").count();
        assert_eq!(count, 1, "publicData should appear exactly once");
    }

    #[test]
    fn merge_is_sorted() {
        let login = sv(&["zzz"]);
        let existing = sv(&["aaa", "mmm"]);
        let requested = sv(&["bbb"]);
        let merged = merge_upgrade_scopes(&login, &existing, &requested);
        let mut sorted = merged.clone();
        sorted.sort();
        assert_eq!(merged, sorted, "output should be lexicographically sorted");
    }

    #[test]
    fn safe_return_to_accepts_site_relative_paths() {
        assert_eq!(
            safe_return_to(Some("/groups/abc".into())),
            Some("/groups/abc".into())
        );
        assert_eq!(safe_return_to(Some("/".into())), Some("/".into()));
        assert_eq!(safe_return_to(None), None);
    }

    #[test]
    fn safe_return_to_rejects_open_redirects() {
        // protocol-relative and absolute URLs
        assert_eq!(safe_return_to(Some("//evil.com".into())), None);
        assert_eq!(safe_return_to(Some("https://evil.com".into())), None);
        // backslash variants browsers normalize to `/` -> off-site
        assert_eq!(safe_return_to(Some("/\\evil.com".into())), None);
        assert_eq!(safe_return_to(Some("\\\\evil.com".into())), None);
        assert_eq!(safe_return_to(Some("/\\/evil.com".into())), None);
        // header injection
        assert_eq!(safe_return_to(Some("/foo\r\nSet-Cookie: x".into())), None);
        // not site-relative
        assert_eq!(safe_return_to(Some("evil.com".into())), None);
    }
}
