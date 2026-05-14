//! Per-request transaction wrapper and the middleware that opens it.
//!
//! Every request that reaches a handler is wrapped in a single Postgres
//! transaction. The middleware:
//!
//!   1. Reads `user_id` from the tower-sessions cookie (empty string if anon).
//!   2. `pool.begin()` to start a new transaction.
//!   3. `SELECT set_config('app.current_user_id', $1, true)` — the `true`
//!      arg is `SET LOCAL`, so the value is visible only inside this tx and
//!      cannot leak across pool reuse.
//!   4. Inserts a [`Tx`] handle into request extensions.
//!   5. Runs the inner service.
//!   6. On a 2xx response, commits; on anything else (4xx, 5xx, panic), the
//!      transaction is rolled back when dropped or via an explicit rollback.
//!
//! Handlers extract `Tx` and call [`Tx::acquire`] to obtain a guard that
//! derefs to `sqlx::Transaction<'_, Postgres>` — i.e. any sqlx executor call
//! that accepts `&mut Transaction` or `&mut PgConnection` works directly:
//!
//! ```ignore
//! async fn handler(tx: Tx, ...) -> Result<_, ApiError> {
//!     let mut conn = tx.acquire().await;
//!     sqlx::query!("SELECT 1").fetch_one(&mut **conn).await?;
//! }
//! ```
//!
//! Handlers MUST NOT call `state.pool.begin()` themselves — there's already
//! a request-scoped transaction in flight, and a second one against another
//! connection will not see `app.current_user_id` (RLS will deny every row).

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{request::Parts, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::{Postgres, Transaction};
use tokio::sync::Mutex;
use tower_sessions::Session;
use uuid::Uuid;

use crate::{extract::SESSION_KEY_USER, state::AppState};

type TxSlot = Arc<Mutex<Option<Transaction<'static, Postgres>>>>;

/// Request-scoped database transaction. Cheap to clone (Arc inside); cloning
/// shares the same underlying transaction.
#[derive(Clone)]
pub struct Tx {
    slot: TxSlot,
}

impl Tx {
    /// Lock the transaction for use in a query. The guard derefs to
    /// `Transaction<'_, Postgres>`, so any sqlx executor call works:
    ///
    /// ```ignore
    /// let mut conn = tx.acquire().await;
    /// sqlx::query!("…").execute(&mut **conn).await?;
    /// ```
    ///
    /// Holding the guard blocks any other code path in the same request
    /// from touching the tx; in practice each request is single-threaded
    /// so this is uncontended. Do not call `acquire` twice without dropping
    /// the first guard — it will deadlock.
    pub async fn acquire(&self) -> TxGuard<'_> {
        TxGuard {
            guard: self.slot.lock().await,
        }
    }
}

pub struct TxGuard<'a> {
    guard: tokio::sync::MutexGuard<'a, Option<Transaction<'static, Postgres>>>,
}

impl std::ops::Deref for TxGuard<'_> {
    type Target = Transaction<'static, Postgres>;
    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("transaction already taken by middleware")
    }
}

impl std::ops::DerefMut for TxGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_mut()
            .expect("transaction already taken by middleware")
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Tx {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Tx>().cloned().ok_or_else(|| {
            tracing::error!("Tx extractor used on a route without tx_middleware");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        })
    }
}

/// Opens a per-request transaction, sets `app.current_user_id` from the
/// session, runs the inner service, then commits on 2xx / rolls back
/// otherwise.
pub async fn tx_middleware(
    State(state): State<AppState>,
    session: Session,
    mut req: Request,
    next: Next,
) -> Response {
    // Anon requests still get wrapped: the helper's `NULLIF(...,'')::uuid`
    // makes it return NULL, and every policy's EXISTS predicate then fails.
    let uid_str: String = match session.get::<Uuid>(SESSION_KEY_USER).await {
        Ok(Some(u)) => u.to_string(),
        Ok(None) => String::new(),
        Err(e) => {
            tracing::error!(error = ?e, "tx_middleware: session read failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
        }
    };

    let mut tx = match state.pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = ?e, "tx_middleware: pool.begin failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };

    if let Err(e) = sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
        .bind(&uid_str)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(error = ?e, "tx_middleware: set_config failed");
        let _ = tx.rollback().await;
        return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
    }

    let slot: TxSlot = Arc::new(Mutex::new(Some(tx)));
    req.extensions_mut().insert(Tx { slot: slot.clone() });

    let response = next.run(req).await;

    // Take the transaction back. Whoever owned the guard inside the handler
    // has already dropped it (handlers cannot outlive the Future returned by
    // `next.run`), so locking here is uncontended.
    let taken = slot.lock().await.take();
    if let Some(tx) = taken {
        let is_success = response.status().is_success();
        let result = if is_success {
            tx.commit().await
        } else {
            tx.rollback().await
        };
        if let Err(e) = result {
            tracing::error!(error = ?e, success = is_success,
                "tx_middleware: commit/rollback failed");
        }
    } else {
        // The slot should always hold the tx here: handlers only ever
        // `acquire()` a guard, never `take()`. An empty slot means the
        // tx was dropped without an explicit commit/rollback (e.g. a
        // handler panicked mid-guard). Postgres rolls back the orphaned
        // transaction on connection return, but log it so the silent
        // rollback is visible to operators.
        tracing::warn!(
            "tx_middleware: transaction slot empty at request end; \
             tx was dropped without explicit commit/rollback"
        );
    }

    response
}
