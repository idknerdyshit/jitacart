# JitaCart

A small, self-hosted webapp for [EVE Online](https://www.eveonline.com/)
logistics groups — wormhole corps, alliances, or any handful of friends who
move stuff between trade hubs.

Paste a MultiBuy string, get a priced shopping list, let any group member
who's heading to a hub claim and fulfill items. Reimbursement is matched
against in-game **contracts** (personal *or* corp wallet) so settlement is
exact and trustworthy — no honor system, no spreadsheet drift.

This README is aimed at operators standing up an instance. For the full
day-2 runbook (rotation, backups, troubleshooting) see [`DEPLOY.md`](DEPLOY.md).
For the security model (token-at-rest, ESI scopes, abuse caps) see
[`backend/SECURITY.md`](backend/SECURITY.md).

## Features

- **MultiBuy → shopping list.** Paste the in-game MultiBuy export; the
  parser is tolerant of trailing volume/price columns. Type-IDs are
  resolved against ESI and cached permanently.
- **Multi-hub pricing.** A buyer picks one or more acceptable markets
  (Jita, Amarr, Dodixie, Rens, Hek, plus any public/freeport citadel a
  linked character can dock at). The estimate is the per-item minimum
  across the accepted set; haulers see a per-hub comparison.
- **Public citadels only.** Discovered via `/universe/structures/`.
  Private citadels are explicitly out of scope — no ACL management.
- **Hauler claims + fulfillment.** Items move through
  `open → claimed → bought → delivered → settled`. Claiming is soft;
  releasing is one click.
- **Contract-based settlement.** Reimbursements are reconciled to the
  EVE contract that delivered the goods, so price drift and partial
  fills resolve themselves.
- **Personal *and* corp wallets.** A corp ambassador (with the right
  in-corp role) can link the corp so a wallet division pays for goods
  pooled across members.
- **EVE SSO, no passwords.** OAuth via [`nea-esi`](https://crates.io/crates/nea-esi);
  refresh tokens are AES-GCM encrypted at rest with key-id rotation.
- **Owner-hash transfer detection.** Sold or biomassed character ⇒
  tokens are invalidated automatically.
- **Background polling.** Worker handles contracts (300s), wallets
  (3600s), market prices (300s), public-structure directory (3600s),
  per-citadel orders (600s). ETag-aware, ESI error-budget-aware.
- **Hardening built in.** Per-IP and per-auth rate limits, configurable
  abuse caps (groups/lists/items/characters per user), Cloudflare
  Turnstile on the login path, structured JSON logs, `/readyz` that
  flips to 503 when the DB is unhappy.
- **Encrypted offsite backups.** Profile-gated nightly Postgres dumps,
  age-encrypted, shipped via rclone to any S3-compatible store.

## Architecture

```
            ┌────────────────────┐
   :80/:443 │       caddy        │  TLS termination, reverse proxy
            └─────────┬──────────┘
            /api/*    │    /*
                ┌─────┴─────┐
                │           │
       ┌────────▼──┐   ┌────▼──────┐
       │   api     │   │ frontend  │  SvelteKit (Node SSR)
       │   :8080   │   │   :3000   │
       └─────┬─────┘   └───────────┘
             │
       ┌─────▼──────┐    ┌──────────┐
       │  postgres  │    │  worker  │  ESI pollers + reencrypt sweeper
       │   :5432    │◄───┤   :9091  │
       └────────────┘    └──────────┘
```

Five containers in `docker-compose.yml`, plus an optional profile-gated
`backup` runner. Everything ships as pre-built images on GHCR
(`ghcr.io/<owner>/jitacart-{backend,frontend,backup}`); the host needs
only Docker and Compose v2 — no Rust or Node toolchain.

| Component  | Stack                                  | Notes                                  |
|------------|----------------------------------------|----------------------------------------|
| `api`      | Rust, Axum, sqlx, Postgres 16          | Serves `/api/*`; non-root              |
| `worker`   | Rust, same workspace                   | ESI pollers + token re-encrypt sweeper |
| `frontend` | SvelteKit 2 + Svelte 5, Node SSR       | Server-side auth gate                  |
| `caddy`    | Caddy 2                                | Auto Let's Encrypt; HTTP/3             |
| `postgres` | Postgres 16-alpine, pinned by digest   | sqlx migrations on api startup         |
| `backup`   | alpine + postgresql-client + age + rclone | Profile-gated, off by default       |

## Quick install

Prerequisites: a Linux host (x86_64 or arm64) with Docker + Compose v2
and DNS pointed at it.

```sh
# 1. Get the repo (only the compose files, Caddyfile, and .env.example are
#    actually needed on the server; everything else is in the images).
git clone https://github.com/eyedeekay/jitacart.git
cd jitacart

# 2. Register an EVE app at https://developers.eveonline.com/applications
#    Callback: https://YOUR_DOMAIN/auth/eve/callback

# 3. Register a Cloudflare Turnstile site (free) and grab the site/secret keys.

# 4. Mint secrets on a trusted machine:
openssl rand -base64 32   # → TOKEN_ENC_KEY
openssl rand -base64 24   # → POSTGRES_PASSWORD

# 5. Configure.
cp .env.example .env
$EDITOR .env
#   Required: JC_DOMAIN, JC_ACME_EMAIL, EVE_SSO__CLIENT_ID,
#             EVE_SSO__CLIENT_SECRET, EVE_SSO__CALLBACK_URL,
#             ESI__USER_AGENT, TOKEN_ENC_KEY, POSTGRES_PASSWORD,
#             TURNSTILE__SITE_KEY, TURNSTILE__SECRET_KEY,
#             TURNSTILE__DISABLED=false

# 6. Bring it up. CI's pin-digests job has already pinned the four
#    jitacart-* images on main to the latest release's multi-arch
#    digests; `scripts/deploy.sh` pulls + ups + polls /readyz and
#    rolls back if it doesn't go green.
scripts/deploy.sh

# 7. Watch Caddy obtain a cert.
docker compose logs -f caddy   # look for "certificate obtained successfully"
```

That's it. Visit `https://YOUR_DOMAIN/`, log in with EVE SSO, and create
a group.

### Local HTTP-only smoke test

No domain handy? Bind Caddy to plain `:80` and skip ACME:

```sh
JC_DOMAIN=:80 docker compose up -d
# → http://localhost
```

Don't run prod this way.

### Local development

For working on the code itself (backend or frontend), only Postgres needs
to run in Docker:

```sh
docker compose -f docker-compose.dev.yml up -d
# Then in two more terminals:
(cd backend  && cargo run -p jitacart-api)
(cd frontend && npm install && npm run dev)
```

Install the pre-commit hook once per clone — it blocks `.env` and known
secret patterns from ever reaching the object database:

```sh
bash scripts/install-git-hooks.sh
```

## Updating

```sh
git pull --ff-only    # fetches CI's "Release: pin images to vX.Y.Z" commit
scripts/deploy.sh     # pull, up, verify, rollback on failure
```

CI's `pin-digests` job rewrites `docker-compose.yml` on `main` after
each `vX.Y.Z` tag, so `git pull` is the only thing the operator does
between releases. See `DEPLOY.md` § Manual digest pinning for the
escape-hatch flow (forks, hotfixes, recovery).

## Health and monitoring

| URL                                | What it tells you                         |
|------------------------------------|-------------------------------------------|
| `/healthz` via Caddy               | api liveness, always 200 if process is up |
| `/readyz` via Caddy                | 200 ready / **503** if DB or pool unhappy |
| `:9091/healthz/esi` (worker)       | ESI error-budget remaining                |

Point uptime monitors at `/readyz`.

## License

[AGPL-3.0-or-later](LICENSE.md). If you run a modified instance for
others, you owe them your patches.
