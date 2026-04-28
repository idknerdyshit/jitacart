# JitaCart — Phased Plan

A small webapp for wormhole groups: paste an EVE Online MultiBuy string, generate
a shopping list, and let any group member who's heading to a trade hub fulfill
items. Reimbursements are matched against in-game **contracts** so settlement is
exact and trustworthy. Both personal wallets and **corp wallets/contracts** are
supported, so a corp can pool ISK for logistics if it wants to.

Initial scope: a private instance for 3–4 friends across multiple corps.
Designed so it can be opened to the public later without a rewrite.

---

## Stack

- **Backend**: Rust + [Axum](https://github.com/tokio-rs/axum) + [sqlx](https://github.com/launchbadge/sqlx) (compile-time-checked queries against Postgres)
- **ESI client**: [`nea-esi`](https://crates.io/crates/nea-esi) — handles OAuth/SSO, token auto-refresh, ETag caching, pagination, retry, and the ESI error-budget. Maintained in-house, so we can extend it when JitaCart needs an endpoint it doesn't cover yet.
- **Frontend**: SvelteKit (TypeScript), server-side rendered, talks to backend over JSON + cookie-auth
- **Database**: Postgres 16
- **Background work**: in-process tokio tasks (scheduled polling of ESI). Split into a separate worker binary if/when public.
- **Auth**: EVE SSO via `nea-esi`'s OAuth helpers, session cookie (HttpOnly, SameSite=Lax), refresh tokens encrypted at rest
- **Config**: figment-style layered config (file + env). Callback URL, EVE client id/secret, DB URL, encryption key all come from `config.toml` + env overrides.
- **Dev**: docker-compose for Postgres; `cargo watch` + `vite dev`

### Repo layout

```
jitacart/
├── PLAN.md
├── README.md
├── docker-compose.yml          # Postgres for local dev
├── .env.example
├── backend/                    # Cargo workspace
│   ├── Cargo.toml
│   ├── config.toml             # callback URL, polling cadence, etc.
│   ├── crates/
│   │   ├── api/                # Axum HTTP server
│   │   ├── worker/             # ESI pollers (contracts, prices, type-id resolver)
│   │   └── domain/             # shared types + db models
│   └── migrations/             # sqlx migrations
└── frontend/                   # SvelteKit
    ├── package.json
    └── src/
```

---

## EVE-specific notes (read first)

- **Identity**: a user authorizes one or more *characters*. Always store EVE's
  `owner_hash` alongside `character_id`; if it changes, the character was
  transferred and tokens must be invalidated.
- **Required ESI scopes** (asked progressively, not all at once):
  - `publicData` — always
  - `esi-contracts.read_character_contracts.v1` — for hauler-side contract tracking
  - `esi-wallet.read_character_wallet.v1` — fallback price source if hauler skips contracts
  - `esi-corporations.read_divisions.v1` — corp wallet division names (corp ambassadors only)
  - `esi-wallet.read_corporation_wallets.v1` — corp wallet journals (requires `Accountant` or `Junior Accountant` role on the linked character)
  - `esi-contracts.read_corporation_contracts.v1` — corp contracts (requires `Contract Manager` role)
- **Corp roles are not part of the OAuth scope** — ESI grants the scope, but
  the data fetch still 403s if the character lost the in-corp role. Treat
  repeated 403s as an auth invalidation event and surface it loudly.
- **No webhooks**: ESI is poll-only. `nea-esi` handles ETag caching and the
  error-budget, so we just configure cadences:
  - contracts: 300s
  - wallet transactions: 3600s
  - market prices: 300s
- **User-Agent header is mandatory** on every ESI request — `nea-esi` requires
  it in the form `app-name (contact; +repo_url; eve:CharacterName)`. We'll set
  this from config at startup.
- **MultiBuy format**: tab-separated `Name\tQuantity`, sometimes with extra
  trailing columns (`...\tvolume\tprice`). Parser must be tolerant.
- **Type-ID resolution**: `Name → type_id` via ESI `/universe/ids/`. Cache
  results in Postgres permanently; names rarely change.
- **Price estimates**: Jita 4-4 sell aggregates from ESI
  `/markets/10000002/orders/?type_id=...&order_type=sell`, or Fuzzwork
  aggregates if we want lower latency.

---

## Data model (sketch)

```sql
-- An app user. One human; may own multiple characters.
users(id, display_name, created_at)

-- A linked EVE character. Owner_hash detects transfers.
characters(
  id, user_id, character_id, character_name, owner_hash,
  scopes, refresh_token_encrypted, access_token_cache, access_token_expires_at,
  created_at, last_refreshed_at
)

-- A logistics group. Loose, not tied to corp.
groups(id, name, invite_code, created_by_user_id, created_at)
group_memberships(user_id, group_id, role, joined_at)

-- A corp linked into JitaCart by an ambassador. Optional per group.
corps(
  id, esi_corporation_id, name, ticker,
  ambassador_character_id,           -- character providing the corp data
  scopes_granted, last_synced_at, last_auth_error_at
)
group_corps(group_id, corp_id)
corp_wallet_divisions(
  corp_id, division, name, balance_isk, last_synced_at
)

-- A shopping list (one MultiBuy paste, typically).
lists(
  id, group_id, created_by_user_id, status,
  destination_label, notes, created_at,
  payer_corp_id, payer_corp_division   -- nullable; set if corp-funded
)
list_items(
  id, list_id, type_id, type_name,
  qty_requested, qty_fulfilled,
  est_unit_price_isk, requested_by_user_id,
  status                         -- open | claimed | bought | delivered | settled
)

-- Hauler claims a list (or specific items).
claims(id, list_id, hauler_user_id, claimed_at, released_at)

-- A "this stuff was bought for this list" record. Initially manual,
-- later auto-matched to ESI transactions.
fulfillments(
  id, list_item_id, hauler_character_id,
  qty, unit_price_isk,
  source,                        -- manual | esi_wallet | esi_contract
  esi_transaction_id, esi_contract_id,
  created_at
)

-- The contract used to deliver bought items to the requester.
contracts(
  id, esi_contract_id, issuer_character_id, assignee_character_id,
  contract_type,                 -- item_exchange | courier | auction
  status,                        -- outstanding | in_progress | finished | failed | cancelled
  price_isk, reward_isk, collateral_isk,
  date_issued, date_expired, date_completed,
  raw_json,                      -- keep the whole payload for debugging
  matched_list_id                -- nullable; set when we link to a list
)
contract_items(contract_id, type_id, quantity, is_included)

-- Final settlement. Calculated; status updated when contract finishes.
-- Payer/payee may be a user OR a corp (exactly one of each set).
reimbursements(
  id, list_id,
  payer_user_id, payer_corp_id,
  payee_user_id, payee_corp_id,
  base_amount_isk, tip_pct, total_amount_isk,
  status,                        -- pending | settled
  contract_id,                   -- the contract that settled it
  settled_at
)
```

Indexes: `(list_id, status)` on `list_items`; `(esi_contract_id)` unique on
`contracts`; `(character_id, owner_hash)` on `characters`.

---

## Phased plan

### Phase 0 — Foundation (≈1 evening)
- Cargo workspace, SvelteKit scaffold, docker-compose Postgres
- sqlx migrations runner wired in
- `/healthz` endpoint, basic SvelteKit landing page
- CI: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`, `pnpm check`
- `.env.example` with placeholders for `EVE_CLIENT_ID`, `EVE_CLIENT_SECRET`,
  `DATABASE_URL`, `SESSION_SECRET`, `TOKEN_ENC_KEY`

**Done when**: clean checkout → `docker compose up && cargo run` boots the API,
migrations apply, and SvelteKit shows a homepage.

### Phase 1 — EVE SSO + sessions (≈1–2 evenings)
- Register an app on developers.eveonline.com (you do this — I'll list scopes)
- Use `nea-esi`'s OAuth helpers for the authorization-code + PKCE flow at
  `/auth/eve/login` and `/auth/eve/callback` (redirect URI read from config)
- Verify the returned JWT against EVE's JWKS; extract `character_id`, `name`, `owner_hash`
- Encrypt refresh tokens (AES-GCM, key from config) before storing; hand them
  back to `nea-esi` via `set_tokens` on each request — the client handles
  refresh-when-near-expiry and retry-on-401 itself
- Cookie session via `tower-sessions` with a Postgres store
- "Add another character" flow on the same `users.id`

**Done when**: log in with EVE, see your character on a profile page, log out,
log back in, attach a second character.

**Risks**: encrypting tokens correctly. Use a single key from env for now;
plan a key-rotation strategy before going public.

### Phase 2 — Groups & invites (≈1 evening)
- Create group, generates short invite code (`/g/join/<code>`)
- Join, leave, list members
- Roles: `owner`, `member` (don't over-design — this is for friend groups)
- All later resources scoped by `group_id`; enforce in a request extractor

**Done when**: two test accounts can form a group via invite link.

### Phase 3 — Lists from MultiBuy (≈2 evenings)
- Tolerant MultiBuy parser (Rust): trims, ignores blank lines, handles
  3-column variants; reports per-line errors to the UI
- Type-ID resolver: batch unknown names via ESI `/universe/ids/`, cache hits in
  a `type_cache(name, type_id, type_name)` table
- Jita-sell price fetch + cache (5 min TTL) for estimates
- SvelteKit UI: paste box → preview table with prices and total → save
- CRUD on items in an existing list

**Done when**: paste a MultiBuy, see resolved items with Jita estimates and a
running total, save it to a group.

### Phase 4 — Hauler manual fulfillment (≈1–2 evenings)
- "Available runs" view: open lists in your groups
- Claim a list (or a subset of items) → `claims` row, items go `claimed`
- Mark items bought with typed-in `unit_price_isk` → `fulfillments(source=manual)`
- Per-requester reimbursement totals with optional tip % (default 0, configurable per list)
- Mark reimbursement as paid (manual button)

**Done when**: end-to-end happy path works without any ESI auto-magic — useful
on day one even before contract tracking.

### Phase 5 — Contract tracking (the headline feature) (≈3–4 evenings)
- Worker polls `/characters/{id}/contracts/` for every linked character with
  the `read_character_contracts` scope, respecting `Expires` headers
- For new/changed contracts, fetch `/contracts/{id}/items/`
- **Matching algorithm**:
  1. Filter to `item_exchange` contracts where the issuer is a known hauler in
     the group and the assignee is a known requester in the group
  2. Score against open `list_items` for that `(hauler, requester)` pair:
     exact `type_id` + `quantity` matches first, then partials
  3. Suggest the link in the UI; **require human confirm** before binding
     (avoids mis-attributing a personal contract)
  4. Once confirmed, `contracts.matched_list_id` is set; further status
     transitions auto-update `reimbursements`
- On `status = finished`: flip reimbursement to `settled`, record contract price as the actual settlement amount, store delta vs. expected for audit
- Surface unmatched contracts in a "needs attention" tray

**Done when**: hauler creates a real item-exchange contract in EVE for a
fulfilled list; within 5 minutes the list shows "delivered via contract #...,"
and the reimbursement flips to settled.

**Risks**:
- Matching false positives. Mitigate with confirm-before-bind and the
  hauler↔requester filter.
- Partial deliveries (one contract covers two lists, or one list spans two
  contracts). Model `contract_items` granularly and allow many-to-many at the
  UI level if it becomes common; defer until we hit it.

### Phase 6 — Corp wallets & contracts (≈3 evenings)

Lets a corp pool ISK for logistics. One *corp ambassador* per corp authorizes
their character (which must hold the right corp roles) so JitaCart can read
that corp's contracts and wallet divisions.

- "Link a corp" flow on group settings, gated to characters whose ESI affiliation says they're in that corp
- Progressive scope grant: ambassador re-authorizes with the three corp scopes; we record which were actually granted
- Worker: poll corp contracts on the same 300s cadence and reuse the Phase 5 matching algorithm with `(issuer ∈ {hauler char, hauler corp}, assignee ∈ {requester char, requester corp})`
- Worker: poll corp wallet division balances + journals; cross-validate "received from contract" entries against settled reimbursements
- UI: per-list "Pay from corp wallet" toggle with division picker; reimbursement view distinguishes personal vs. corp settlement
- Auth-failure handling: repeated 403s on a corp endpoint → mark the corp `last_auth_error_at`, alert the ambassador, stop polling until re-linked

**Done when**: an ambassador with `Accountant` + `Contract Manager` links a
corp into a group, marks a list as corp-funded with division 2, and a
corp-issued item-exchange contract auto-matches and settles.

**Risks**:
- **Auth fragility from role loss** (above). Without it the corp data silently goes stale.
- **Data sensitivity** — corp wallet journals can be embarrassing or
  competitively sensitive. Default to minimum visibility: non-ambassadors only
  see entries tied to a settled reimbursement; full journal stays gated to the
  ambassador.
- **Multi-corp groups**: a group can have several linked corps. Make sure UI
  always shows *which* corp is paying, never assumes a default.

### Phase 7 — Polish & multi-character UX (≈2 evenings)
- Per-user character picker ("buying as Alt McAlt")
- List history, archived/closed lists
- Discord webhook per group (new list, list claimed, list delivered)
- Empty-states, error toasts, loading skeletons
- Mobile-friendly tables

### Phase 8 — Public-readiness (only if/when we open it)
- Split `worker` into its own binary; supervisor-friendly
- Rate limiting per user / per IP
- ESI error-budget guard (back off globally if we approach 100 errors / 60s)
- Per-tenant data isolation audit (every query filtered by `group_id`)
- Token-encryption key rotation
- Backups, observability (tracing → OTLP), uptime checks
- Privacy page: explicit list of scopes, what we read, retention policy
- Abuse: cap groups/lists per user, captcha on signup

---

## Decisions

1. **Hosting**: existing VPS. We'll need to provision Postgres on it (or
   point at an existing instance), set up a reverse proxy with TLS, and run
   the API + worker as systemd units. Backups are our problem.
2. **EVE app callback URL**: a single callback per EVE app, set to the
   production URL. The callback URL is still read from `config.toml` (so the
   code stays env-agnostic), but the EVE app itself is registered once
   against prod. Local dev hits the same prod hostname via an `/etc/hosts`
   override (or a Tailscale/Cloudflare tunnel) so the OAuth round-trip works
   without registering a second app.
3. **Tip**: per-list with a per-group default that pre-fills new lists.
   Default group default is 0%. Schema: `groups.default_tip_pct`,
   `lists.tip_pct`.
4. **Contract price as settlement amount**: when a contract finishes, its
   price is the settlement, full stop. We compute the expected total
   (`Σ fulfillment unit_price × qty × (1 + tip)`) and surface the delta as
   informational only — never auto-correcting it.

---

## Non-goals (for now)

- Courier-contract logistics chains (someone else hauls already-bought goods)
- Price prediction / "best time to buy"
- Mobile app
- Anything that writes to EVE (we are read-only against ESI)
