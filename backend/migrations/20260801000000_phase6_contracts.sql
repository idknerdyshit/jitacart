-- Phase 6: Contract tracking — poll cursors, contracts, items, match suggestions.

-- Per-character poll cursor (staggered scheduling). No ETag column: nea-esi's
-- character_contracts/character_contract_items don't use the cached path, so
-- a cached ETag would be misleading.
ALTER TABLE characters
    ADD COLUMN contracts_next_poll_at   timestamptz,
    ADD COLUMN contracts_last_polled_at timestamptz;

CREATE INDEX characters_contracts_due_idx
    ON characters(contracts_next_poll_at)
    WHERE 'esi-contracts.read_character_contracts.v1' = ANY(scopes);

CREATE TABLE contracts (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    esi_contract_id          bigint NOT NULL UNIQUE,
    issuer_character_id      bigint NOT NULL,
    issuer_user_id           uuid REFERENCES users(id),
    assignee_character_id    bigint,
    assignee_user_id         uuid REFERENCES users(id),
    contract_type            text NOT NULL
        CHECK (contract_type IN ('item_exchange','auction','courier','unknown')),
    status                   text NOT NULL
        CHECK (status IN (
            'outstanding','in_progress','finished_issuer','finished_contractor','finished',
            'cancelled','rejected','failed','deleted','reversed'
        )),
    price_isk                numeric(20,2) NOT NULL DEFAULT 0,
    reward_isk               numeric(20,2) NOT NULL DEFAULT 0,
    collateral_isk           numeric(20,2) NOT NULL DEFAULT 0,
    expected_total_isk       numeric(20,2),
    settlement_delta_isk     numeric(20,2),
    date_issued              timestamptz NOT NULL,
    date_expired             timestamptz,
    date_accepted            timestamptz,
    date_completed           timestamptz,
    start_location_id        bigint,
    end_location_id          bigint,
    raw_json                 jsonb NOT NULL,
    items_synced_at          timestamptz,
    first_seen_at            timestamptz NOT NULL DEFAULT now(),
    updated_at               timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX contracts_status_idx
    ON contracts(status);
CREATE INDEX contracts_issuer_user_idx
    ON contracts(issuer_user_id) WHERE issuer_user_id IS NOT NULL;
CREATE INDEX contracts_needs_items_idx
    ON contracts(items_synced_at) WHERE items_synced_at IS NULL;

CREATE TABLE contract_items (
    contract_id  uuid NOT NULL REFERENCES contracts(id) ON DELETE CASCADE,
    record_id    bigint NOT NULL,
    type_id      integer NOT NULL,
    quantity     bigint NOT NULL,
    is_included  boolean NOT NULL,
    PRIMARY KEY (contract_id, record_id)
);
CREATE INDEX contract_items_type_idx
    ON contract_items(type_id);

-- Phase 5 left reimbursements.contract_id as a placeholder UUID; promote it
-- to a real foreign key now that the contracts table exists.
ALTER TABLE reimbursements
    ADD CONSTRAINT reimbursements_contract_fk
    FOREIGN KEY (contract_id) REFERENCES contracts(id) ON DELETE SET NULL;
CREATE INDEX reimbursements_contract_idx
    ON reimbursements(contract_id) WHERE contract_id IS NOT NULL;

CREATE TABLE contract_match_suggestions (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id        uuid NOT NULL REFERENCES contracts(id) ON DELETE CASCADE,
    reimbursement_id   uuid NOT NULL REFERENCES reimbursements(id) ON DELETE CASCADE,
    score              numeric(5,4) NOT NULL CHECK (score >= 0 AND score <= 1),
    exact_match        boolean NOT NULL,
    state              text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','confirmed','rejected','superseded')),
    created_at         timestamptz NOT NULL DEFAULT now(),
    decided_at         timestamptz,
    decided_by_user_id uuid REFERENCES users(id),
    UNIQUE (contract_id, reimbursement_id)
);
CREATE INDEX contract_match_open_idx
    ON contract_match_suggestions(state) WHERE state IN ('pending', 'confirmed');
-- Defensive: only one confirmed suggestion may exist per reimbursement.
CREATE UNIQUE INDEX one_confirmed_suggestion_per_reimbursement
    ON contract_match_suggestions(reimbursement_id) WHERE state = 'confirmed';

UPDATE _jitacart_meta SET value = '6' WHERE key = 'schema_phase';
