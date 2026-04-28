-- Phase 4: public citadel coverage.
--
-- Existing markets columns are NOT NULL (region_id, name, short_label) because
-- Phase 3 only seeded NPC hubs. Citadel rows are inserted by discovery before
-- their detail fetch resolves name/region/system, so loosen them to nullable.

ALTER TABLE markets
  ALTER COLUMN region_id    DROP NOT NULL,
  ALTER COLUMN name         DROP NOT NULL,
  ALTER COLUMN short_label  DROP NOT NULL,
  ADD COLUMN solar_system_id        bigint,
  ADD COLUMN structure_type_id      integer,
  ADD COLUMN last_orders_synced_at  timestamptz,
  ADD COLUMN details_synced_at      timestamptz,
  ADD COLUMN missing_poll_count     integer NOT NULL DEFAULT 0,
  ADD COLUMN untrackable_until      timestamptz;

-- Invariant retained for NPC hubs (which keep all three populated):
ALTER TABLE markets
  ADD CONSTRAINT markets_npc_hub_fields_present
  CHECK (kind <> 'npc_hub' OR (region_id IS NOT NULL AND name IS NOT NULL AND short_label IS NOT NULL));

CREATE INDEX markets_kind_public_idx
  ON markets (kind, is_public) WHERE kind = 'public_structure';

-- Backlog index for the durable detail-backfill worker:
CREATE INDEX markets_details_pending_idx
  ON markets (created_at)
  WHERE kind = 'public_structure' AND details_synced_at IS NULL AND is_public = true;

CREATE TABLE group_tracked_markets (
  group_id          uuid        NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  market_id         uuid        NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
  added_by_user_id  uuid        NOT NULL REFERENCES users(id),
  added_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (group_id, market_id)
);
CREATE INDEX group_tracked_markets_market_idx
  ON group_tracked_markets (market_id);

-- Two distinct access dimensions: details (universe scope) vs market orders
-- (markets scope + docking access). A 403 on one does not imply the other.
CREATE TYPE structure_access_status AS ENUM ('ok','forbidden','unknown');

CREATE TABLE character_structure_access (
  character_id          uuid        NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
  market_id             uuid        NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
  details_status        structure_access_status NOT NULL DEFAULT 'unknown',
  details_checked_at    timestamptz,
  market_status         structure_access_status NOT NULL DEFAULT 'unknown',
  market_checked_at     timestamptz,
  PRIMARY KEY (character_id, market_id)
);
CREATE INDEX csa_market_ok_idx
  ON character_structure_access (market_id) WHERE market_status = 'ok';
CREATE INDEX csa_details_ok_idx
  ON character_structure_access (market_id) WHERE details_status = 'ok';

UPDATE _jitacart_meta SET value = '4' WHERE key = 'schema_phase';
