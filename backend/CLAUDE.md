# CLAUDE.md — backend

Backend-specific guidance. Root `CLAUDE.md` covers the overall topology, frontend, compose stack, and CI.

## Commands

Run from `backend/`.

```sh
cargo fmt --all -- --check                       # rustfmt check (CI: -D warnings)
cargo clippy --all-targets --all-features        # lint (CI: -D warnings)
cargo test --all-features                        # full test suite
cargo test -p jitacart-api                       # one crate
cargo test -p jitacart-api --test tenant_isolation   # one integration test file
cargo test some_test_name                        # single test by name
cargo run -p jitacart-api                        # run api locally
cargo run -p jitacart-worker                     # run worker locally
cargo audit --ignore RUSTSEC-2023-0071           # CI uses this exact ignore
```

Tests need Postgres reachable at `DATABASE_URL`. CI uses `postgres://jitacart:jitacart@localhost:5432/jitacart` with `SQLX_OFFLINE=true`. Migrations in `migrations/` are applied by `cargo run` on api startup via `sqlx::migrate!`.

The `cargo audit` ignore is for RUSTSEC-2023-0071 (Marvin) in build-time-only `rsa` pulled in unconditionally by `sqlx-macros-core`. See the rationale comment in `.github/workflows/ci.yml`; do not change without checking it.

## Workspace (`crates/`)

- **`api`** — Axum HTTP server. `main.rs` wires sqlx pool, `tower-sessions` (Postgres-backed cookie sessions), `tower_governor` two-tier rate limit (stricter `auth_*` bucket on SSO routes, generous per-IP elsewhere), `DefaultBodyLimit::max(256 KiB)`, `SetRequestId`/`TraceLayer`/`PropagateRequestId`. Modules: `auth`, `groups`, `markets`, `lists`, `citadels`, `fulfillment`, `contracts`, `corps`, `webhooks`. `/healthz` always-200 liveness, `/readyz` returns 503 if DB or pool is unhappy — point uptime monitors at `/readyz`.
- **`worker`** — One driver task per `JobSlot` (see `worker/src/jobs/mod.rs::registry`), each with its own `tokio::time::interval` (`MissedTickBehavior::Delay`) and concurrency semaphore. Jobs: `market_prices`, `contracts` + `corp_contracts`, `citadel_discovery` / `details` / `orders`, `npc_hubs`, `corp_wallet`, `token_reencrypt`, `pending_webhooks`, `csa`. Default intervals: prices 300s, contracts 300s, structure dir 3600s, per-citadel orders 600s, wallets 3600s, details 86400s. All ESI calls gated by the shared `EsiBudgetGuard`.
- **`domain`** — Type-IDs, principals, MultiBuy parsing. Domain enums use `sqlx::Type` derives.
- **`market`** — Hub/citadel price aggregation.
- **`auth-tokens`** — `MultiKeyCipher` (AES-256-GCM, kid-keyed) for refresh+access tokens at rest, `CharacterTokenStore`, `EsiBudgetGuard`. Encrypts always use the configured *primary* kid; decrypts dispatch on the row's stored `token_key_id` and error hard on unknown kid. Enables key rotation — see `SECURITY.md`.
- **`bootstrap`** — `init_tracing` + `load_config` (figment, toml + env, double-underscore nesting: `EVE_SSO__CLIENT_ID`).
- **`settlement`** — Contract-based reimbursement reconciliation.
- **`webhook-dispatch`** — Outbound webhooks (drained by the worker's `pending_webhooks` job).

## Multi-tenant isolation (read `SECURITY.md`)

Every list/claim/contract/fulfillment/reimbursement belongs to exactly one `group`. Cross-tenant leakage is the worst class of bug we ship. **Enforcement lives in the SQL layer, not in handler bodies.** Three extractors in `crates/api/src/extract.rs`:

| Extractor      | Path                       | Effect                                                    |
| -------------- | -------------------------- | --------------------------------------------------------- |
| `CurrentGroup` | `/groups/{id}/...`         | Verifies caller's `group_memberships` row, returns role.  |
| `CurrentList`  | `/lists/{id}/...`          | LEFT JOIN lists+memberships; null role ⇒ 403, missing ⇒ 404. |
| `CurrentClaim` | `/claims/{id}/...`         | Same shape, joined through `claims → lists → group_memberships`. |

**Rule for new routes**: if a handler reads or writes any tenant-scoped table (`lists`, `list_items`, `claims`, `claim_items`, `fulfillments`, `contracts`, `reimbursements`, `group_corps`, `group_webhooks`, `corp_wallet_*`, etc.), it MUST use one of the three extractors OR inline an explicit `JOIN group_memberships gm ON gm.group_id = l.group_id AND gm.user_id = $caller` before any mutation. Non-tenant tables: `markets`, `market_prices`, `type_cache`, `stations`, `users`, `characters`, `principals`.

`crates/api/tests/tenant_isolation.rs` re-runs each extractor's literal SQL against a two-tenant fixture and exercises cross-tenant denial for every `do_*` helper. Add a case here whenever you add a new `do_*` or extractor.

## Token-at-rest rotation

Refresh + access tokens are AES-GCM-encrypted with a per-row `token_key_id`. Config supports a legacy `token_enc_key` single-key shim (loaded as kid `v1`) and a multi-key `[token_enc]` table with a `primary` kid. To rotate: add the new kid alongside the old, restart both api and worker, flip `primary`, restart again, wait for the worker's `token_reencrypt` sweeper to drain old rows. Full runbook in `SECURITY.md`.

## Migrations

Single consolidated migration at `migrations/20260427000000_init.sql`. Applied by `sqlx::migrate!` on api startup. When you add a migration, both `cargo run -p jitacart-api` and api container startup will apply it before opening the listener.

## Conventions

- Config is figment (toml + env). Env-var nesting uses double underscores: `EVE_SSO__CLIENT_ID`, `RATE_LIMIT__AUTH_BURST`, `TOKEN_ENC__PRIMARY`. Required-var changes belong in both `.env.example` and any docs that list them.
- Logs are JSON when `JC_LOG_FORMAT=json`, with `request_id` on per-request lines.
- ISK columns use `rust_decimal`, not `f64`.
- Domain enums use `#[derive(sqlx::Type)]`.
