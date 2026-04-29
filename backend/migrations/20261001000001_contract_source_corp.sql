-- Phase 7 follow-up: track which corp discovered a contract so item-sync can
-- fall back to the corp ESI endpoint when neither party is a tracked character.

ALTER TABLE contracts
    ADD COLUMN source_corp_id uuid REFERENCES corps(id) ON DELETE SET NULL;

CREATE INDEX contracts_source_corp_idx
    ON contracts(source_corp_id)
    WHERE source_corp_id IS NOT NULL;
