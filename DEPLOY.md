# JitaCart — Deploy Guide

This is the operational reference for the docker-compose stack that runs
JitaCart in production. The backend runtime is covered by `backend/SECURITY.md`
(key rotation, abuse caps, ESI budget); this file is just deployment +
day-2 ops.

## Stack at a glance

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

Five services in `docker-compose.yml`. Local dev keeps just postgres via
`docker-compose.dev.yml`.

## First-time deploy

1. **Provision a host** with Docker + Compose v2 (any Linux x86_64 or
   arm64). DNS A/AAAA records pointed at the host for the domain you'll
   put in `JC_DOMAIN`.
2. **Register the EVE app** at
   <https://developers.eveonline.com/applications> with callback
   `https://YOUR_DOMAIN/auth/eve/callback`. Save the client id + secret.
3. **Generate secrets** (run on a trusted machine, *not* the server, if
   you can; copy via SSH):
   ```sh
   openssl rand -base64 32   # → TOKEN_ENC_KEY (or TOKEN_ENC__KEYS__V1)
   openssl rand -base64 24   # → POSTGRES_PASSWORD (no '/' or '+' please)
   ```
4. **Register a Cloudflare Turnstile site** at
   <https://dash.cloudflare.com/?to=/:account/turnstile>. Save site key
   + secret key.
5. **Copy and fill `.env`**:
   ```sh
   cp .env.example .env
   # then edit .env: set JC_DOMAIN, JC_ACME_EMAIL, EVE_SSO__*,
   # TOKEN_ENC_KEY, POSTGRES_PASSWORD, TURNSTILE__*, and flip
   # TURNSTILE__DISABLED=false.
   ```
6. **Bring it up**:
   ```sh
   docker compose up -d --build
   ```
   First build takes ~10 minutes (Rust release build). Subsequent
   builds reuse the BuildKit cache and finish in seconds when only
   sources changed.
7. **First-time TLS**: Caddy obtains a Let's Encrypt cert on first
   request. Watch `docker compose logs -f caddy` until you see
   `certificate obtained successfully`. If the host's :80 isn't
   reachable from the internet, ACME will fail loudly — fix DNS /
   firewall and `docker compose restart caddy`.

## Day 2 — common operations

### View logs
```sh
docker compose logs -f api
docker compose logs -f worker
docker compose logs --tail=200 caddy
```

Logs are JSON-formatted (api/worker via `tracing-subscriber`, caddy via
its own JSON formatter) and rotated by docker's `json-file` driver
(10MB × 3 files per service).

### Health checks

| URL                                | Source        | Expected            |
|------------------------------------|---------------|---------------------|
| `/healthz` via Caddy               | api liveness  | `{status:"ok",db:true}` always-200 |
| `/readyz` via Caddy                | api readiness | 200 ready / **503** if DB or pool unhappy |
| `:9091/healthz` (worker, loopback) | worker        | `ok`                |
| `:9091/healthz/esi`                | worker budget | `{remaining:N,has_budget:bool}` |

Point uptime monitors (UptimeRobot, BetterStack, etc.) at `/readyz`,
not `/healthz` — readiness flips to 503 when we can't actually serve
traffic, which is what you want to alert on.

The compose stack runs the api healthcheck every 30 s; an unhealthy
container is restarted by docker.

### Logs

`JC_LOG_FORMAT=json` (set in `.env`) makes both api and worker emit
one JSON object per log line, with the standard fields plus a
`request_id` on per-request log lines. Use any log shipper (Vector,
Fluent Bit, Promtail) pointed at the docker json-file driver's
output (`/var/lib/docker/containers/<id>/<id>-json.log`).

### Update + redeploy

```sh
git pull
docker compose build api worker frontend       # parallel
docker compose up -d                            # rolling restart of changed services
```

`postgres` and `caddy` images are pinned tags; they don't get rebuilt.

### Database backups

Profile-gated `backup` service handles nightly dumps. See the full
runbook (setup, restore, threat model) in `backend/SECURITY.md`.
Quick reference:

```sh
# First-time bring-up after filling .env + backup/rclone.conf:
docker compose --profile backup up -d --build backup

# Manual one-shot backup (e.g. before a risky migration):
docker compose --profile backup run --rm \
    -e BACKUP_RUN_ON_START=true backup \
    /usr/local/bin/backup.sh
```

### Rotating the token-encryption key

See **Rotation runbook** in `backend/SECURITY.md`. Summary: add the new
key alongside the old one in `.env`, bounce both api and worker, flip
`TOKEN_ENC__PRIMARY`, bounce again, wait for the worker's hourly
sweeper to drain old rows.

### Rotating Postgres credentials

```sh
# Pick a new password.
NEW_PW=$(openssl rand -base64 24)
docker compose exec -T postgres psql -U jitacart -c "ALTER USER jitacart PASSWORD '$NEW_PW';"
sed -i.bak "s|^POSTGRES_PASSWORD=.*|POSTGRES_PASSWORD=$NEW_PW|" .env
docker compose restart api worker
```

### Adjusting limits / rate limits without a restart … you can't

The api binary reads config at startup. To raise an abuse cap or rate
limit, edit `.env` and `docker compose restart api`. The worker reads
its own config the same way; restart it with `docker compose restart
worker`. Postgres state is unaffected.

## Troubleshooting

- **`exit code 1` on api startup, log says "loading config"**: missing
  required env var (most often `EVE_SSO__CLIENT_ID`,
  `EVE_SSO__CLIENT_SECRET`, or no token-encryption key). Compare your
  `.env` against `.env.example`.
- **Caddy never gets a cert**: check `JC_DOMAIN` is a real FQDN that
  resolves to this host, port 80 is reachable from the public internet,
  and `JC_ACME_EMAIL` is set. ACME logs are loud about which check
  failed.
- **Login storms blocked by rate limit**: the auth bucket is small by
  default (5 burst, refill once per 10 s). Returning users with a
  session don't hit it — only fresh logins do. Raise
  `RATE_LIMIT__AUTH_BURST` if your community is bigger than expected.
- **Worker reports `esi budget low`**: ESI is rejecting our calls. The
  worker is correctly backing off; investigate why (transient ESI
  outage, owner-hash transfer, expired tokens) by following the
  `error =` fields in the most recent failures.
- **Postgres won't start after host reboot**: check the `jitacart-pgdata`
  volume permissions. Compose recreates the container fresh; if you
  ever switch postgres major versions, you'll need a `pg_dump` /
  `pg_restore` cycle — see the Postgres release notes.

## Local testing of the prod stack

Useful when iterating on the Caddyfile or Dockerfile without a real
domain:

```sh
JC_DOMAIN=:80 docker compose up -d --build
# Visit http://localhost
```

`:80` tells Caddy to bind plain HTTP on port 80 only and skip ACME.
Fine for confidence-checking the layout; do NOT run prod this way.
