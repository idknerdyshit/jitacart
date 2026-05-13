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
   git pull --ff-only      # fetches CI's "Release: pin images to vX.Y.Z" commit
   scripts/deploy.sh
   ```
   The compose file is image-only — every service points at a published
   `ghcr.io/<owner>/jitacart-*:vX.Y.Z@sha256:<digest>` image, so `pull`
   produces bit-for-bit identical containers no matter when it runs.
   `git revert` is the rollback button.

   The digest pinning happens in CI's `pin-digests` job on every
   `vX.Y.Z` tag — it rewrites the four `image:` lines on `main` right
   after the images are built. Operators do not run
   `bump-image-digests.sh` in the normal flow. (Forks, hotfixes, or
   recovering from a failed CI push: see *Manual digest pinning*.)
7. **First-time TLS**: Caddy obtains a Let's Encrypt cert on first
   request. Watch `docker compose logs -f caddy` until you see
   `certificate obtained successfully`. If the host's :80 isn't
   reachable from the internet, ACME will fail loudly — fix DNS /
   firewall and `docker compose restart caddy`.

### Installing the local pre-commit guard

Once per clone, on every dev machine:

```sh
bash scripts/install-git-hooks.sh
```

That points `core.hooksPath` at `scripts/git-hooks/`, so a stray
`git add .env` is rejected before it ever leaves your laptop. CI has
its own scan as a backstop, but the local hook keeps secrets out of
the object database in the first place.

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
# 1. Pull the latest tree. CI's pin-digests job has already pinned
#    the four jitacart-* images on main to the latest release tag's
#    multi-arch digests, so this commit is what you're deploying.
git pull --ff-only
# 2. Deploy with healthcheck-driven rollback.
scripts/deploy.sh
```

`scripts/deploy.sh` pulls + brings up the stack, then polls `/readyz`
on api + worker for up to 90 s. If readiness never goes green it
reverts `docker-compose.yml` to the previous commit, re-pulls, and
brings the previous digests back up. See below for the manual
fallback path.

`postgres` and `caddy` are pinned by digest in the compose file, so
`pull` is a no-op for them on every redeploy. `api`, `worker`,
`frontend`, and `backup` are digest-pinned too — CI rewrites those
lines on `main` after each `vX.Y.Z` tag, so `git pull` is the only
thing the operator does between releases.

### Manual digest pinning

CI handles this in the normal release flow. Reach for
`scripts/bump-image-digests.sh` only when:

- You forked the project and CI publishes images under a different
  GHCR owner (`JC_IMAGE_OWNER=youracct scripts/bump-image-digests.sh vX.Y.Z`).
- The pin-digests CI job failed to push (e.g. branch protection
  rejected the bot push) and you need to recover.
- You're deploying a tag that was built outside CI.

```sh
scripts/bump-image-digests.sh vX.Y.Z
git add docker-compose.yml
git commit -m "Release: pin images to vX.Y.Z"
git push origin main
scripts/deploy.sh
```

### Building images locally

The compose file is image-only by default; you don't need a working
Rust or Node toolchain on the host. The build override exists for the
rare case of producing an image off-CI (hotfix, dev iteration on a
Dockerfile):

```sh
JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
    docker compose -f docker-compose.yml -f docker-compose.build.yml build
JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
    docker compose -f docker-compose.yml up -d
```

The override re-tags each built image with the same `ghcr.io/...`
reference the prod compose pulls, so a local build slots into the
prod stack without further config changes. Don't push these to GHCR
unless you want to override what CI publishes.

### Database backups

The `backup` service runs by default. Verify it's healthy:

```sh
docker compose ps backup           # STATUS: Up
docker compose logs --tail=20 backup
```

Healthy log line on a configured stack: `next backup at 2026-…T03:00Z`.
If you see `WARN backup disabled: BACKUP_AGE_RECIPIENT and/or
BACKUP_RCLONE_REMOTE unset`, fill in `.env` (see `backup/RESTORE.md`)
and `docker compose up -d backup`.

Manual one-shot backup (e.g. before a risky migration):

```sh
docker compose run --rm -e BACKUP_RUN_ON_START=true backup
```

Full setup, threat model, and restore procedure: see
[`backup/RESTORE.md`](backup/RESTORE.md).

#### Quarterly restore drill

A backup you've never restored is hope, not a backup. Run the drill
every quarter; log the date and outcome somewhere durable.

1. Pick yesterday's dump (exercises the full pipeline).
2. Restore into `jitacart_restore` (side DB) using the procedure in
   `backup/RESTORE.md` → *Quarterly restore drill*.
3. Diff row counts against the live DB (`users`, `groups`, `lists`,
   `claims`). Expect a tiny delta — rows since the dump.
4. Run `cargo test -p jitacart-api --test tenant_isolation` against
   the restored DB. Drift surfaces as a test failure.
5. `DROP DATABASE jitacart_restore;` and record the outcome.

### Metrics & monitoring

Both api and worker expose Prometheus exposition on loopback ports
inside their containers — never proxied through Caddy, never reachable
from the public internet:

| Endpoint                        | Surface                                          |
|---------------------------------|--------------------------------------------------|
| api `127.0.0.1:9090/metrics`    | request count / latency / in-flight per route (auto-instrumented via `axum-prometheus`) |
| worker `127.0.0.1:9091/metrics` | `jitacart_worker_job_runs_total{slot,outcome}`, `jitacart_worker_esi_budget_remaining` |

Sanity-check from the host:

```sh
docker compose exec api    curl -s 127.0.0.1:9090/metrics | head
docker compose exec worker curl -s 127.0.0.1:9091/metrics | head
```

To scrape, run Prometheus / vmagent / Grafana Agent **outside the
stack** (separate compose project, separate host, whatever) and have
it reach into the api/worker containers — either as a sidecar joined
to the `default` compose network or via a tunnel. Don't host the
scraper in this stack; a probe co-located with what it probes goes
dark exactly when you need it most.

Configure an **external** uptime probe (BetterUptime, UptimeRobot,
Hetzner status, etc.) against `https://${JC_DOMAIN}/readyz`. `/readyz`
returns 503 when the DB is unreachable or the connection pool is
exhausted — exactly the failure mode you want to alert on. `/healthz`
is liveness only; it would 200 with a broken DB.

To disable the api `/metrics` listener entirely (e.g. small operator
who doesn't want to scrape), set `METRICS__BIND=` (empty) in `.env`
and restart api.

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
JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
    docker compose -f docker-compose.yml -f docker-compose.build.yml build
JC_DOMAIN=:80 JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
    docker compose -f docker-compose.yml up -d
# Visit http://localhost
```

`:80` tells Caddy to bind plain HTTP on port 80 only and skip ACME.
Fine for confidence-checking the layout; do NOT run prod this way.
