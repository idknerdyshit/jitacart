# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

JitaCart is a self-hosted webapp for EVE Online logistics groups: paste a MultiBuy string, get a priced shopping list, let haulers claim items, settle reimbursements against in-game contracts. EVE SSO only — no passwords. See `README.md` for the user-facing pitch and `DEPLOY.md` for the operator runbook.

## Where the guidance lives

- **`backend/CLAUDE.md`** — Rust workspace commands, crate map, multi-tenant isolation rule, token-at-rest rotation. Read it before editing anything under `backend/`.
- This file — topology, frontend, compose stack, CI, repo-wide conventions.

## Topology

Caddy terminates TLS and reverse-proxies `/api/*` to the `api` container (Axum, :8080) and `/*` to the SvelteKit Node SSR server (`frontend`, :3000). The `worker` container has no public surface; it polls ESI and exposes a loopback `/healthz/esi`. All services share one Postgres 16. The `backup` service is profile-gated and off by default.

## Frontend

SvelteKit 2 (Node SSR adapter) + Svelte 5. Routes under `src/routes/(authed)/` are gated server-side in `+layout.server.ts`. API client is `src/lib/api.ts`. The frontend container does server-side auth-gating before any private page renders.

Run from `frontend/`:

```sh
npm install
npm run dev                                      # vite dev server (3000)
npm run build
npm run check                                    # svelte-kit sync && svelte-check (this is what CI runs)
npm audit --audit-level=high
```

## Local dev stack

Only Postgres in Docker; api + frontend run on host:

```sh
docker compose -f docker-compose.dev.yml up -d
(cd backend  && cargo run -p jitacart-api)
(cd frontend && npm install && npm run dev)
```

Install the pre-commit hook once per clone (blocks `.env` and known secret patterns):

```sh
bash scripts/install-git-hooks.sh
```

## Full prod-style stack (compose)

Five services: `caddy`, `api`, `worker`, `frontend`, `postgres`, plus profile-gated `backup`. Image-only by default; nothing builds on the host. Local builds use the build override:

```sh
JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
  docker compose -f docker-compose.yml -f docker-compose.build.yml build
JC_DOMAIN=:80 JC_IMAGE_OWNER=local JC_IMAGE_TAG=dev \
  docker compose -f docker-compose.yml up -d
```

`JC_DOMAIN=:80` skips ACME for local smoke testing.

## Env file lint

`bash scripts/check-env-example.sh` enforces `.env.example` ↔ live env-var parity. CI runs this; run it after adding any new env var.

## CI

`.github/workflows/ci.yml` runs on push to `main`, tags `v*`, and PRs:

- `backend` job: `cargo fmt --check`, `cargo clippy --all-targets --all-features` (with `RUSTFLAGS="-D warnings"`), `cargo test --all-features`. Uses Postgres service container and `SQLX_OFFLINE=true`.
- `frontend` job: `npm ci` + `npm run check`.
- `audit-backend`: `cargo audit --ignore RUSTSEC-2023-0071` (Marvin in build-time-only `rsa` via `sqlx-macros-core`; do not change this ignore without checking `ci.yml`'s rationale comment).
- `audit-frontend`: `npm audit --audit-level=high`.
- `env-lint`: `scripts/check-env-example.sh`.
- `secret-scan`: gitleaks + a backstop check that refuses any tracked file matching `(^|/)\.env($|\.[^.]+$)` other than `.env.{example,sample,template}`.
- `build-images` + `release`: on `v*` tags only, push multi-arch images to `ghcr.io/<owner>/jitacart-{backend,frontend,backup}` and create a GitHub Release.

## Conventions

- License is **AGPL-3.0-or-later**. Modified instances run for others owe their patches.
- Env-var nesting uses double underscores: `EVE_SSO__CLIENT_ID`, `RATE_LIMIT__AUTH_BURST`, `TOKEN_ENC__PRIMARY`. Required-var changes belong in both `.env.example` and any docs that list them.
- Never commit a real `.env` — both the local pre-commit hook and CI will refuse it.
