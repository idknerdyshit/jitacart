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
  - `esi-markets.structure_markets.v1` — read orders on public/freeport citadels the character can dock at (we ignore private citadels entirely)
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
  - market prices (per hub region): 300s
  - public-structure directory (`/universe/structures/`): 3600s
  - per-citadel market orders: 600s (lower priority than NPC hubs)
- **User-Agent header is mandatory** on every ESI request — `nea-esi` requires
  it in the form `app-name (contact; +repo_url; eve:CharacterName)`. We'll set
  this from config at startup.
- **MultiBuy format**: tab-separated `Name\tQuantity`, sometimes with extra
  trailing columns (`...\tvolume\tprice`). Parser must be tolerant.
- **Type-ID resolution**: `Name → type_id` via ESI `/universe/ids/`. Cache
  results in Postgres permanently; names rarely change.
- **Markets we cover**: the five major NPC trade hubs and any *public /
  freeport* player citadels. Private citadels are explicitly out of scope —
  if a character can't dock there, we don't try, and we don't ask users to
  manage ACLs for us.
  - **NPC hubs** (region id → hub station):
    - Jita 4-4 — Caldari Navy Assembly Plant (region `10000002`, The Forge)
    - Amarr — Emperor Family Academy (region `10000043`, Domain)
    - Dodixie IX-19 — Federation Navy Assembly Plant (region `10000032`, Sinq Laison)
    - Rens VI-8 — Brutor Tribe Treasury (region `10000030`, Heimatar)
    - Hek VIII-12 — Boundless Creation Factory (region `10000042`, Metropolis)
  - **Public citadels**: discovered via the unauthenticated
    `/universe/structures/` endpoint (lists all *public* structures), then
    `/universe/structures/{id}/` for name + solar system + type. Order data
    comes from `/markets/structures/{id}/`, which requires
    `esi-markets.structure_markets.v1` *and* docking access for the calling
    character. We pool all linked characters and pick any one with access
    per citadel; if none have access, we drop the citadel.
  - A citadel that flips from public → private (disappears from
    `/universe/structures/`, or starts returning 403/404 on its market
    endpoint) is soft-disabled and its cached orders are marked stale.
- **Price estimates**: hub-level "best available" sell + buy aggregates from
  ESI `/markets/{region_id}/orders/?type_id=...`, filtered to the hub
  station's `location_id`. Citadels are aggregated from
  `/markets/structures/{structure_id}/`. A list's *buyer* picks **one or
  more acceptable markets** ("I'll pay for these goods if you buy them at
  Jita, Amarr, or 1DQ Keepstar"); the budget estimate is the per-item
  minimum across the accepted set, and haulers see a per-hub price
  comparison restricted to those markets when shopping. Default acceptable
  set on a new list is `{Jita}`. Fuzzwork aggregates are an optional
  lower-latency mirror for NPC hubs only (Fuzzwork doesn't cover citadels).
- **Wallet transactions across hubs**: ESI wallet transactions/journal
  entries carry `location_id`. We resolve each `location_id` against
  `stations` (NPC) and our public-citadel cache to attribute a fulfillment
  to a specific market. Unknown `location_id`s are recorded as `unknown`
  (likely a private citadel) and excluded from per-hub analytics rather
  than guessed at.

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

-- A market we track prices/transactions for. Either an NPC hub station or
-- a public/freeport citadel discovered via /universe/structures/.
markets(
  id,                              -- internal pk
  kind,                            -- 'npc_station' | 'public_structure'
  esi_location_id,                 -- station_id or structure_id
  region_id,                       -- region the market lives in
  solar_system_id,
  name,                            -- e.g. 'Jita IV - Moon 4 - Caldari Navy Assembly Plant'
  short_label,                     -- e.g. 'Jita', 'Amarr', '1DQ1-A Keepstar'
  is_hub,                          -- true for the five NPC hubs
  is_public,                       -- always true for what we track; flips false → soft-disabled
  last_orders_synced_at,
  last_seen_public_at,             -- updated each /universe/structures/ poll for citadels
  last_auth_error_at               -- 403/404 on /markets/structures/{id}/ → we back off
)

-- Aggregated best-available prices, recomputed each market poll.
market_prices(
  market_id, type_id,
  best_sell_isk, best_buy_isk,
  sell_volume, buy_volume,
  computed_at,
  primary key (market_id, type_id)
)

-- A shopping list (one MultiBuy paste, typically).
lists(
  id, group_id, created_by_user_id, status,
  destination_label, notes, created_at,
  payer_corp_id, payer_corp_division   -- nullable; set if corp-funded
)

-- Markets the buyer is willing to pay for goods at. Many-to-many: a list
-- can accept Jita + Amarr + a public Keepstar. Empty = the buyer hasn't
-- saved the list yet; saved lists must have ≥1 row here. The market
-- chosen as the "default display" hub for the saved snapshot is flagged
-- `is_primary` (exactly one per list).
list_markets(
  list_id, market_id,
  is_primary,                    -- exactly one true per list
  added_at,
  primary key (list_id, market_id)
)

list_items(
  id, list_id, type_id, type_name,
  qty_requested, qty_fulfilled,
  est_unit_price_isk,              -- min(best_sell_isk) across list_markets at save time
  est_priced_market_id,            -- which market in the accepted set produced the min
  requested_by_user_id,
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
  bought_at_market_id,           -- nullable; resolved from wallet txn location_id, or set manually
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
`contracts`; `(character_id, owner_hash)` on `characters`;
`(esi_location_id)` unique on `markets`; `(market_id, type_id)` is the pk on
`market_prices`; partial unique `(list_id) where is_primary` on
`list_markets` to enforce exactly-one-primary.

---

## Phased plan

### Phase 0 — Foundation (≈1 evening)
- Cargo workspace, SvelteKit scaffold, docker-compose Postgres
- sqlx migrations runner wired in
- `/healthz` endpoint, basic SvelteKit landing page
- CI: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`, `npm run check`
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
- Seed `markets` with the five NPC hubs (Jita 4-4, Amarr, Dodixie, Rens,
  Hek) via a migration
- NPC-hub price fetch worker: per region, pull
  `/markets/{region_id}/orders/?type_id=...`, filter by the hub station's
  `location_id`, aggregate best sell + best buy into `market_prices`. 5 min
  TTL, ETag-conditional via `nea-esi`
- SvelteKit UI: paste box → preview table with a side-by-side per-hub
  price comparison across all five NPC hubs. Buyer multi-selects the hubs
  they're willing to pay for goods at (chip picker, Jita pre-checked, ≥1
  required) and picks one as the *primary* (used as the default display).
  Estimated unit price = `min(best_sell)` across the selected set; the
  preview shows which hub each min came from
- Saving the list writes the chosen markets to `list_markets` and
  snapshots `est_unit_price_isk` + `est_priced_market_id` per item
- CRUD on items in an existing list, plus add/remove markets from the
  accepted set (recomputes estimates on change)

**Done when**: paste a MultiBuy, tick *Jita + Amarr + Hek* as acceptable,
see a per-item budget that picks the cheapest of those three, save the list
with all three markets recorded.

### Phase 4 — Public citadel market coverage (≈2 evenings)
- Citadel discovery worker: poll the unauthenticated
  `/universe/structures/` hourly; upsert into `markets` with
  `kind='public_structure'`. For new structures, fetch
  `/universe/structures/{id}/` for name + system + type. Update
  `last_seen_public_at` on every poll
- Soft-disable: a structure that drops out of `/universe/structures/` for N
  consecutive polls (or starts 403/404'ing) is flagged `is_public=false`;
  cached `market_prices` rows are kept but marked stale
- Per-citadel order fetch: the user picks which citadels to actively price.
  Discovered ≠ tracked — there are thousands of public structures and we
  don't want to spam ESI. Default tracked set is empty; group admins can
  add citadels by name search
- Access pooling: for each tracked citadel, find any linked character with
  `esi-markets.structure_markets.v1` who can dock there. Try characters in
  round-robin; on 403 from one character, mark that character as
  no-access for that structure and try the next. If no character can
  access it, mark the citadel `untrackable_until` (24h) and stop trying
- Aggregate best sell + best buy into `market_prices` on the same
  cadence as NPC hubs (with a 600s default for citadels)
- UI: add tracked citadels to the per-list hub picker. Citadels render
  with a clear `[citadel]` badge and the docking-character used
- Explicit non-coverage: any structure not in the public directory is
  silently ignored. There is no UI affordance to "add a private citadel"

**Done when**: an admin adds a public Keepstar to the group's tracked
citadels; pasting a MultiBuy shows a price column for that citadel
alongside Jita/Amarr/Dodixie/Rens/Hek; if the citadel goes private, the
column greys out within an hour.

**Risks**:
- **Docking access loss**: a character that *had* access can lose it.
  Treat 403 on `/markets/structures/{id}/` as a per-character signal, not
  a global disable
- **Stale citadel names**: structure names change. Refresh
  `/universe/structures/{id}/` on a slower cadence (24h) for tracked
  citadels
- **Wash trading**: citadels can have nonsense order books. Surface
  *volume* alongside price so users can sanity-check; don't trust a
  one-unit "best price"

### Phase 5 — Hauler manual fulfillment (≈1–2 evenings)
- "Available runs" view: open lists in your groups, each tagged with the
  set of markets the buyer accepts (e.g. *Jita / Amarr / 1DQ Keepstar*)
- Claim a list (or a subset of items) → `claims` row, items go `claimed`
- "Where to buy" helper: for the claimed items, render the live per-hub
  prices restricted to that list's accepted markets, with the cheapest
  hub per item highlighted
- Mark items bought with typed-in `unit_price_isk` and a `bought_at_market_id`
  picker scoped to the list's accepted markets → `fulfillments(source=manual)`.
  An "Other" option exists but warns the hauler that buying outside the
  buyer's accepted set may not be reimbursed without buyer confirmation
- Per-requester reimbursement totals with optional tip % (default 0, configurable per list)
- Mark reimbursement as paid (manual button)

**Done when**: end-to-end happy path works without any ESI auto-magic — useful
on day one even before contract tracking. A hauler claiming a Jita+Amarr
list sees prices for both, picks the cheaper per item, and records each
purchase against the correct hub.

### Phase 6 — Contract tracking (the headline feature) (≈3–4 evenings)
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

### Phase 7 — Corp wallets & contracts (≈3 evenings)

Lets a corp pool ISK for logistics. One *corp ambassador* per corp authorizes
their character (which must hold the right corp roles) so JitaCart can read
that corp's contracts and wallet divisions.

- "Link a corp" flow on group settings, gated to characters whose ESI affiliation says they're in that corp
- Progressive scope grant: ambassador re-authorizes with the three corp scopes; we record which were actually granted
- Worker: poll corp contracts on the same 300s cadence and reuse the Phase 6 matching algorithm with `(issuer ∈ {hauler char, hauler corp}, assignee ∈ {requester char, requester corp})`
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

### Phase 8 — Polish & multi-character UX (≈2 evenings)
- Per-user character picker ("buying as Alt McAlt")
- List history, archived/closed lists
- Discord webhook per group (new list, list claimed, list delivered)
- Empty-states, error toasts, loading skeletons
- Mobile-friendly tables

### Phase 9 — Public-readiness (only if/when we open it)
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
