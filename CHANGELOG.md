# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-05-13

First tagged release. Everything below was built up over the phased development that preceded this tag.

### Added

- EVE SSO login with refresh-token rotation, per-user multi-character linking, scope upgrades on demand (`400464e`, `878616e`).
- Groups and memberships with roles (member / hauler / ambassador / admin) and ambassador-issued invites (`bca851f`).
- Shopping lists built from MultiBuy paste, with market-price polling against NPC hubs and configurable public citadels (`f088dcc`, `ba93e41`).
- Hauler fulfillment flow: per-line claims, buy-run tracking, tips, reimbursement accounting (`5cf73bf`).
- In-game contract matching: auto-link issued contracts to lists, confirm/reject, settle-via-contract reconciliation (`6ae17b2`).
- Corp wallet + corp contract ingestion via ESI for ambassador-visible accounting (`b1a2f1b`).
- Outbound Discord webhooks for fulfillment/contract events, drained by a worker job with `FOR UPDATE SKIP LOCKED` and idempotency (`878616e`, `b9622fa`, `ea801d4`).
- Operator deploy story: pre-built multi-arch GHCR images, `scripts/deploy.sh` with healthcheck-driven rollback, `scripts/bump-image-digests.sh` for per-release digest pinning, profile-gated backups with age + rclone, `backup/RESTORE.md` runbook (`a5263d6`, `e6f6c1a`, `42a5a66`, `2763ed6`).
- Loopback-only Prometheus `/metrics` endpoint on its own listener (`METRICS__BIND`) (`2763ed6`).
- Typed ESI id newtypes in `domain` (`EsiCharacterId`, `EsiContractId`, `EsiLocationId`, …) so cross-id transposition is a compile error (`2763ed6`).
- Shared `jitacart-config` crate factoring common SSO/ESI config out of api and worker (`2763ed6`).
- AGPL-3.0-or-later license (`2b074e9`).

### Security

- Multi-tenant isolation enforced in the SQL layer via `CurrentGroup` / `CurrentList` / `CurrentClaim` extractors, with `tenant_isolation.rs` integration tests re-running each extractor's literal SQL against a two-tenant fixture (`5e75bdb`, `8d7b615`, `10d7045`).
- Defense-in-depth: Postgres row-level security policies + per-request transactions binding the caller's user id (`202efde`).
- AES-256-GCM token-at-rest with `MultiKeyCipher` (kid-keyed), character-id-bound AAD, and a worker `token_reencrypt` sweeper for rotation; full rotation runbook in `backend/SECURITY.md` (`db05a9d`, `30d6310`).
- Turnstile abuse guard on SSO callbacks; two-tier `tower_governor` rate limit (stricter bucket on `auth_*`); 256 KiB default body limit (`db05a9d`).
- Closed TOCTOU windows and authz gaps across handlers (`5e75bdb`).
- Hardened init migration with CHECK constraints, RESTRICT on cross-tenant FKs, and an index for the principal-matcher (`fe2ab72`).
- CI: gitleaks + a backstop refusing tracked `.env*` files; `cargo audit` with documented `RUSTSEC-2023-0071` ignore for build-time-only `rsa` in `sqlx-macros-core`; `scripts/check-env-example.sh` enforcing `.env.example` ↔ live env-var parity (`a5263d6`, `78915b2`, `a2fe4bb`).

### Changed

- Frontend: SSR auth gate before private pages render, responsive-table layout hoisted to the root layout, extracted `MarketPicker.svelte`, `SvelteSet`-backed reactive market selection (`8959f2b`, `880e332`, `2763ed6`, `7a08a1a`).
- Backend: split `lists.rs` and `fulfillment.rs` into module folders; archive-guard helper and batch settle-via-contract lookup to cut duplication (`b4cfde2`, `baf55e8`).
- Domain enums migrated to `sqlx::Type` derives; query simplification and rustfmt pass (`4390a9c`).
- Replaced `.map_err(ApiError::internal)` with `?` via `From` impls (`0902cc3`).
- Worker: per-`JobSlot` driver tasks each with their own interval and concurrency semaphore; shared `EsiBudgetGuard` across all ESI calls (`f45e74b`, `3994cb5`).
- docker-compose: image lines are digest-pinned (rewritten per release by `scripts/bump-image-digests.sh`); dropped the `JC_IMAGE_TAG` knob — `git revert` is the rollback button and the audit trail is the tree (`42a5a66`, `2763ed6`).
- Backup service runs by default and parks (Up + WARN) when `BACKUP_AGE_RECIPIENT` / `BACKUP_RCLONE_REMOTE` are unset, so missing config is visible at `docker compose ps` rather than silent (`2763ed6`).

### Fixed

- Settlement: keep `settlement_delta_isk` NULL when nothing is bound, instead of writing 0 (`778cc99`).
- Market: log and skip prices that fail `Decimal::from_f64` rather than panicking (`5545254`).
- Scoped `corps.list_journal` reads to the caller's `CurrentGroup.group_id` (`8d7b615`).
- Allow hauler or ambassador (not just the original issuer) to unlink corp-issued contracts (`10d7045`).
