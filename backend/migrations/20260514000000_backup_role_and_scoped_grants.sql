-- Adds the read-only backup role, narrows jitacart_app's DELETE privilege,
-- and pins reimbursement money columns non-negative.
--
-- These changes were originally drafted into the init migration, but
-- 20260427000000_init.sql already shipped in v0.1.0 — editing an applied
-- migration trips sqlx's checksum check and refuses to boot. A new
-- migration applies cleanly to both fresh and upgraded databases.

-- ── jitacart_backup role ───────────────────────────────────────────────────
-- Read-only role for the backup container. BYPASSRLS so pg_dump (which sets
-- row_security = off) can read every tenant's rows; the SELECT-only grants
-- below mean a compromised backup container still cannot mutate or delete
-- data. Created idempotently to match the role-creation style in the init
-- migration (roles can pre-exist when init-roles.sh has run).
DO $$ BEGIN
  CREATE ROLE jitacart_backup LOGIN NOSUPERUSER BYPASSRLS;
EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL;
END $$;

GRANT USAGE ON SCHEMA public TO jitacart_backup;
GRANT USAGE ON SCHEMA app    TO jitacart_backup;

-- pg_dump needs SELECT on every table and sequence, nothing more.
GRANT SELECT ON ALL TABLES    IN SCHEMA public TO jitacart_backup;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA public TO jitacart_backup;

-- ── Narrow jitacart_app's DELETE ───────────────────────────────────────────
-- The init migration grants jitacart_app a blanket DELETE on ALL TABLES. A
-- blanket DELETE lets the api role wipe non-RLS shared tables (users,
-- characters, principals, corps, markets, type_cache, …) with no policy
-- check standing in the way. Revoke it and re-grant DELETE only on the
-- tables the api crate actually issues `DELETE FROM` against. ON DELETE
-- CASCADE does not need the privilege on the cascaded-to tables.
REVOKE DELETE ON ALL TABLES IN SCHEMA public FROM jitacart_app;
GRANT DELETE ON
    groups,
    group_memberships,
    group_tracked_markets,
    group_discord_webhooks,
    lists,
    list_markets,
    list_items,
    claim_items
  TO jitacart_app;

-- jitacart_worker keeps its broad grant: it has BYPASSRLS and is trusted
-- server-side code. (No change — left here as a note for the next reader.)

-- ── Reimbursement money columns are non-negative ───────────────────────────
-- Money is never negative. We deliberately do NOT also CHECK
-- total = subtotal + tip: both tip and total are derived as independent
-- numeric(24,2) roundings of subtotal * tip_pct and subtotal * (1 + tip_pct),
-- so they can legitimately differ from subtotal + tip by one cent.
ALTER TABLE reimbursements
    ADD CONSTRAINT reimbursements_subtotal_isk_nonneg CHECK (subtotal_isk >= 0),
    ADD CONSTRAINT reimbursements_tip_isk_nonneg      CHECK (tip_isk >= 0),
    ADD CONSTRAINT reimbursements_total_isk_nonneg    CHECK (total_isk >= 0);
