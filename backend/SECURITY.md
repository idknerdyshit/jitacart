# JitaCart — Backend Security Notes

## Multi-tenant isolation (Phase 9 / M1)

JitaCart is a multi-tenant app: every authenticated user is a member of zero or
more `groups`. Every list/claim/contract/fulfillment/reimbursement/citadel
linkage is owned by exactly one group. Cross-tenant access is the most
catastrophic class of bug we ship for, so we enforce isolation in **the SQL
layer**, not in handler bodies.

### The three extractors

`crates/api/src/extract.rs` provides three FromRequestParts extractors. Every
authenticated route MUST use one of them, or do the equivalent JOIN inline:

| Extractor      | Path pattern               | Query                                                                                          |
| -------------- | -------------------------- | ---------------------------------------------------------------------------------------------- |
| `CurrentGroup` | `/groups/{id}/...`         | `SELECT role FROM group_memberships WHERE user_id = $caller AND group_id = $path`              |
| `CurrentList`  | `/lists/{id}/...`          | LEFT JOIN `lists` and `group_memberships`; `role IS NULL` ⇒ 403, missing list ⇒ 404            |
| `CurrentClaim` | `/claims/{id}/...`         | Same shape, joined through `claims → lists → group_memberships`                                |

If a route can't use one of these (e.g. `/contracts/{id}/manual-link`), the
handler MUST do an explicit `JOIN group_memberships gm ON gm.group_id =
l.group_id AND gm.user_id = $caller` before any mutation, and return Forbidden
when no row comes back. The `do_*` helpers in `contracts.rs`, `lists.rs`,
`fulfillment.rs`, and `corps.rs` follow this pattern.

### Tenant-scoped tables

These tables hold data that belongs to a specific group, transitively or
directly. New code MUST scope reads and writes to them by `group_id`:

```
lists, list_items, list_markets, claims, claim_items, fulfillments, contracts,
contract_items, contract_match_suggestions, reimbursements, group_corps,
group_tracked_markets, group_webhooks, character_structure_access (per-user),
corp_wallet_divisions / corp_wallet_journal (gated by group_corps),
corp_ambassadors (gated by group_corps).
```

NOT tenant-scoped (shared across all tenants): `markets`, `market_prices`,
`type_cache`, `stations`, `users`, `characters` (per-user, not per-group),
`principals`.

### Rule for new routes

> If a handler reads or writes any tenant-scoped table, it must call
> `CurrentGroup` / `CurrentList` / `CurrentClaim`, OR JOIN `group_memberships`
> with the caller's `user_id` BEFORE the operation. No exceptions.

### Regression tests

`crates/api/tests/tenant_isolation.rs` exercises the cross-tenant denial path
for every `do_*` helper and re-runs the literal SQL from each extractor
against a two-tenant fixture. If someone refactors the extractors and drops
the `user_id` predicate, those tests break loudly. Add a case here whenever
you add a new `do_*` or extractor.

## Refresh-token encryption

Refresh + access tokens for EVE characters are stored AES-256-GCM-encrypted
at rest in `characters.{refresh,access}_token_ciphertext` (+ matching
`*_nonce` columns). Each row also carries `characters.token_key_id` naming
the *kid* (key id) under which it was encrypted.

The runtime cipher is a `MultiKeyCipher` (`auth-tokens/src/cipher.rs`) that
holds a map of `kid → 32-byte AES key` plus a designated *primary* kid.
**Encrypts always use the primary**; **decrypts look up by the row's stored
kid** and error hard on unknown kid (no silent fallback — that would mask a
misconfiguration during rotation).

### Configuration

Two shapes are accepted (multi-key wins if both are present):

```toml
# Legacy single-key shim. Loaded as kid "v1" and made primary.
token_enc_key = "<base64-32-bytes>"

# Multi-key. Required for rotation.
[token_enc]
primary = "v2"
[token_enc.keys]
v1 = "<base64-32-bytes>"
v2 = "<base64-32-bytes>"
```

Both `api` and `worker` read the same shape from `config.toml` /
environment.

### Rotation runbook

Goal: introduce a new primary key with **zero downtime**, then drain old
ciphertext onto it before retiring the old key.

1. **Generate** a new 32-byte key:
   `openssl rand 32 | base64`
2. **Add it as non-primary** in both `api` and `worker` config:
   ```toml
   [token_enc]
   primary = "v1"        # unchanged
   [token_enc.keys]
   v1 = "<old key>"      # unchanged
   v2 = "<new key>"      # new
   ```
3. **Bounce both binaries.** Both processes must know about the new key
   before any row gets encrypted under it. (Order doesn't matter; either
   can restart first.) After this step nothing has changed at rest — every
   row still references kid `v1`.
4. **Flip `primary`** to the new kid, in both configs:
   ```toml
   [token_enc]
   primary = "v2"
   ```
5. **Bounce both binaries again.** From this point on, every refresh-token
   write (new logins, scope upgrades, nea-esi token rotations) lands under
   `v2`. Active characters drain automatically.
6. **Wait for the sweeper.** The worker runs `jobs::token_reencrypt::run`
   hourly; it decrypts any row still on a non-primary kid and rewrites it
   under the primary, in batches of 100. Watch the logs for
   `token-kid sweeper rewrote stale rows`. Once a sweep tick reports
   `scanned = 0`, the long tail is drained.
7. **Retire the old key.** Remove the `v1` entry from `[token_enc.keys]`
   and bounce. If anything was missed, the binary will fail loudly on
   that row's next decrypt rather than silently use the wrong key.

### Recovery / rollback

- Up to step 5, rolling back is just reverting `primary` and bouncing.
- After step 5 has run for a while, rows exist in both kids. Reverting
  `primary` to the old kid leaves the v2 rows decryptable as long as the
  v2 entry is kept in `keys`. **Don't remove a key that any row still
  references** — there's no way to recover that ciphertext.

### Threat model

- **Database leak alone**: useless without `TOKEN_ENC_KEY` /
  `[token_enc.keys]`.
- **Config leak alone**: useless without the database.
- **Both leaked**: rotate immediately; revoked sessions don't help here
  (the attacker already has plaintext refresh tokens). Coordinated
  response: revoke EVE app credentials at developers.eveonline.com,
  forcing every character to re-authorize.

## Public-readiness controls (Phase 9 / M3)

Three knobs that come on once the app is exposed to the open internet.

### Rate limiting

In-app via `tower_governor`, two layered token-bucket limiters keyed by
client IP. Configured under `[rate_limit]` in `config.toml`:

```toml
[rate_limit]
disabled = false        # set true in tests / local dev
# Per-IP global limit: 30-request burst, 1 token added every second.
per_ip_burst = 30
per_ip_period_secs = 1
# Stricter bucket for /auth/eve/{login,callback,upgrade,logout} + /me*.
auth_burst = 5
auth_period_secs = 10
```

429s are returned as `tower_governor`'s default response. To raise the
limit, edit config and bounce — no migration needed. Keys are **client
IP**; if you put a reverse proxy in front of the API, make sure it
forwards the real address (e.g. via `X-Forwarded-For`) and that
`tower_governor` is configured to read it.

### Abuse caps

Per-tenant size ceilings. Live in `[limits]` so they can be raised
without a migration. Defaults are generous enough that real users
won't hit them; they exist to stop runaway scripts.

```toml
[limits]
groups_per_user = 10        # counted as memberships, not just owned
lists_per_group = 200       # archived lists do NOT count
items_per_list  = 500       # enforced on create + add_items
characters_per_user = 12    # enforced when linking a NEW character
```

Cap violations return `422 Unprocessable Entity` with a typed message.
The handlers that enforce them:

- `groups::create`, `groups::join` → `groups_per_user`
- `lists::create` → `lists_per_group` + `items_per_list`
- `lists::add_items` → `items_per_list` (against the locked list row,
  so concurrent adds can't both squeak past)
- `auth::upsert_character` → `characters_per_user` (only when the
  upsert would add a *new* row under the user — re-logins exempt)

Tests live in `crates/api/tests/abuse_caps.rs`.

### Cloudflare Turnstile

Gates **new-user creation only**. Existing users with a valid session
or attaching a character to an existing account skip the check.

```toml
[turnstile]
disabled  = true       # default in dev / tests
site_key  = "<from Cloudflare dashboard>"
secret_key = "<from Cloudflare dashboard>"
```

Flow:
1. SvelteKit landing renders the Turnstile widget when the user is
   logged out.
2. Login button fires `GET /auth/eve/login?cf=<token>`.
3. Server (`auth::login`) calls
   `crate::turnstile::verify(http, secret, token, None)` — POST
   to `https://challenges.cloudflare.com/turnstile/v0/siteverify`.
4. On `success: false` the request is rejected with **403 captcha
   verification required**. On `success: true` the redirect to ESI
   authorize proceeds.
5. Returning users with a session cookie or with `attach=true` skip
   the check entirely.

To disable for local dev: set `disabled = true`. The validator is
never even called.

### Threat model

- **Anonymous abuse / signup spam**: rate-limited at the auth bucket
  and required to clear Turnstile. Both must fail open simultaneously
  for an attacker to mass-create accounts.
- **Logged-in abuse**: tower-governor still rate-limits the IP. Abuse
  caps prevent any single user from creating runaway state. None of
  these gate cross-tenant access — that's the M1 audit's job.

## ESI error budget (Phase 9 / M4)

`auth_tokens::EsiBudgetGuard` (`crates/auth-tokens/src/budget.rs`) tracks a
process-wide gauge of remaining non-2xx responses before ESI starts 420ing us
on rate. Every worker batch consults `has_budget()` before kicking off. The
guard resets once a minute (mirroring ESI's window) via `budget_reset.tick()`
in `worker/src/main.rs`.

### The convention

All ESI calls go through the `auth_tokens::budgeted` combinator:

```rust
use auth_tokens::budgeted;

let row = budgeted(&ctx.budget, esi.get_contracts(char_id))
    .await
    .map_err(|e| anyhow!("get_contracts: {e}"))?;
```

`budgeted` takes ownership of the future, decrements the guard on `Err`,
and returns the original `Result` untouched. Wrapping the call this way
makes "did this future decrement on failure?" a syntactic property
instead of a discipline question.

When you add a new ESI call site, **prefer `budgeted` over a hand-rolled
`match { Err(e) => { record_non_2xx(); ... } }`**. Existing call sites
in `worker/src/jobs/*.rs` are mid-migration: `npc_hubs.rs` and
`citadel_discovery.rs` are converted as exemplars; the rest still use
the manual pattern and will migrate as touched. Both forms are
correct.

### `/healthz/esi`

The worker exposes a tiny axum server on `worker.healthz_bind`
(default `127.0.0.1:9091`):

- `GET /healthz` → `200 ok`
- `GET /healthz/esi` → JSON `{"remaining": <i16>, "has_budget": <bool>}`

Bind address comes from `[worker]` config; an empty string disables the
server (used in tests). Exposed only on loopback by default — point an
internal probe (UptimeRobot via Tailscale, BetterStack via a sidecar,
or just a docker-compose `curl` check) at it.

## Backups (Phase 9 / M6)

Nightly Postgres dump → age-encrypted → S3-compatible bucket via the
profile-gated `backup` service in `docker-compose.yml`. The container
runs on its own cron loop (default 03:00 UTC) and exits each run after
either uploading + pruning, or warning that required env vars are
missing.

### Setup (first time)

1. **Generate an age key** on a trusted machine (NOT the production
   host — the *private* key only needs to exist where you'd restore):
   ```sh
   age-keygen -o jitacart-backup.age
   ```
   The `# public key:` line goes into `BACKUP_AGE_RECIPIENT` in `.env`.
   Stash the file (containing the secret line) somewhere durable
   (1Password, paper printout, etc.).
2. **Pick a bucket**. B2, R2, Wasabi, AWS, MinIO — anything `rclone`
   speaks. Copy `backup/rclone.conf.example` to `backup/rclone.conf`
   and fill in. Set `BACKUP_RCLONE_REMOTE` in `.env` to e.g.
   `b2:my-bucket/jitacart`.
3. **Bring up the service**:
   ```sh
   docker compose --profile backup up -d --build backup
   ```
   On startup it logs the next scheduled fire time. To smoke-test now:
   `BACKUP_RUN_ON_START=true docker compose --profile backup up backup`.

### Restore

Backups are streamed `pg_dump` custom-format → age-encrypted. Restore
one as:

```sh
# Pull the encrypted backup from S3.
rclone --config backup/rclone.conf copy "$BACKUP_RCLONE_REMOTE/jitacart-2026-05-08.dump.age" .

# Decrypt with the private key. Stays in /dev/shm or memory if at all
# possible; never write the plaintext dump to a shared disk.
age -d -i jitacart-backup.age jitacart-2026-05-08.dump.age > restored.dump

# Restore into a fresh database.
createdb -U jitacart jitacart_restore
pg_restore -U jitacart -d jitacart_restore --no-owner --no-acl restored.dump

# Verify, then point the api at it (DATABASE_URL) and bounce the stack.
```

### Threats

- **Bucket compromise alone**: ciphertext only; useless without the
  age key.
- **Age key compromise alone**: useless without the ciphertext.
- **Both lost**: restoration impossible. Print the age key.
- **Bucket lifecycle misconfiguration**: rclone-side prune is bounded
  by `BACKUP_RETAIN_DAILY` (default 30). If your bucket *also* has a
  shorter object-lifetime policy, that wins. Configure the bucket
  with at least 35 days retention and let the backup script's prune
  be the floor.

## Observability (Phase 9 / M6)

- **Structured logs**: set `JC_LOG_FORMAT=json` (api + worker) for
  one-JSON-object-per-line output. Anything else gets the default
  human-readable formatter.
- **Request IDs**: every inbound HTTP request gets a UUID via
  `tower-http`'s `SetRequestIdLayer`. The id is propagated to the
  response as `X-Request-Id` and into the request span's tracing
  fields. When debugging a user report, ask them for the request id
  from their browser's network tab.
- **Liveness vs readiness**: `/healthz` always returns 200 if the
  binary is running; `/readyz` returns 503 if the DB is unreachable
  or the connection pool is fully exhausted. Point uptime monitors
  (UptimeRobot, BetterStack, etc.) at `/readyz` so they alert when
  the app can't serve real traffic — not just "process alive."

## Privacy

User-facing privacy disclosure lives at `/privacy` (SvelteKit static
page). The `GET /api/me/export` endpoint returns a JSON dump of
everything keyed to the calling user's id (no token plaintext, no
columns named `*_ciphertext` or `*_nonce` — the export is a tested
allow-list of safe columns). Test in
`crates/api/tests/data_export.rs` asserts no token bytes leak.

## Known operational risks

- Token-encryption key compromise: rotate immediately by deploying a new key
  and following the [rotation runbook](#rotation-runbook). Old key must be
  retained until the sweeper drains every row.
- Session-secret compromise: rotate `SESSION_SECRET`; this invalidates all
  active sessions, forcing re-login.
- Database backups (M6) will be encrypted with `age` before upload to the
  S3-compatible bucket; backup-key loss = unrecoverable backups.
