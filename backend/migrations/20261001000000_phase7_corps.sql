-- Phase 7: Corp wallets & contracts
--
-- Introduces:
--   * corps, group_corps, corp_ambassadors, corp_wallet_divisions,
--     corp_wallet_journal — new tables for corporation principals
--   * principals — polymorphic principal table (kind IN ('user','corp'))
--   * Backfills existing users into principals
--   * Adds principal-id columns to contracts and reimbursements,
--     backfills, drops old unique constraint, adds new partial unique index
--   * payer_corp_id + payer_division on lists (wallet funding source)
--   * wallet_verified_at + wallet_payout_aggregate_isk on contracts

-- ── Corp tables ───────────────────────────────────────────────────────────────

CREATE TABLE corps (
    id                        uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    esi_corporation_id        bigint NOT NULL UNIQUE,
    name                      text NOT NULL,
    ticker                    text NOT NULL,
    last_synced_at            timestamptz,
    last_auth_error_at        timestamptz,
    disabled_at               timestamptz,
    created_at                timestamptz NOT NULL DEFAULT now(),
    contracts_next_poll_at    timestamptz,
    contracts_last_polled_at  timestamptz,
    wallet_next_poll_at       timestamptz,
    wallet_last_polled_at     timestamptz
);
CREATE INDEX corps_contracts_due_idx ON corps(contracts_next_poll_at)
    WHERE disabled_at IS NULL;
CREATE INDEX corps_wallet_due_idx ON corps(wallet_next_poll_at)
    WHERE disabled_at IS NULL;

-- Group → Corp link. One row per pair; soft-unlink toggles unlinked_at.
-- The partial unique index is redundant with the PK but harmless.
CREATE TABLE group_corps (
    group_id         uuid NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    corp_id          uuid NOT NULL REFERENCES corps(id) ON DELETE RESTRICT,
    linked_at        timestamptz NOT NULL DEFAULT now(),
    linked_by_user_id uuid NOT NULL REFERENCES users(id),
    unlinked_at      timestamptz,
    PRIMARY KEY (group_id, corp_id)
);
CREATE UNIQUE INDEX group_corps_active_unique
    ON group_corps(group_id, corp_id) WHERE unlinked_at IS NULL;

-- Characters that are authorized to act as ESI ambassadors for a corp.
-- FK to characters(id) ON DELETE CASCADE: if we lose a character row, revoke
-- the ambassadorship automatically.
CREATE TABLE corp_ambassadors (
    corp_id            uuid NOT NULL REFERENCES corps(id) ON DELETE CASCADE,
    character_id       uuid NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    granted_scopes     text[] NOT NULL DEFAULT '{}',
    last_used_at       timestamptz,
    last_auth_error_at timestamptz,
    disabled_at        timestamptz,
    PRIMARY KEY (corp_id, character_id)
);
CREATE INDEX corp_ambassadors_active_idx ON corp_ambassadors(corp_id)
    WHERE disabled_at IS NULL;

-- Cached per-division balances.
CREATE TABLE corp_wallet_divisions (
    corp_id       uuid     NOT NULL REFERENCES corps(id) ON DELETE CASCADE,
    division      smallint NOT NULL CHECK (division BETWEEN 1 AND 7),
    name          text,
    balance_isk   numeric(20,2) NOT NULL DEFAULT 0,
    last_synced_at timestamptz,
    PRIMARY KEY (corp_id, division)
);

-- Wallet journal entries (audit-only; contract status drives settlement).
CREATE TABLE corp_wallet_journal (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    corp_id             uuid     NOT NULL REFERENCES corps(id) ON DELETE CASCADE,
    division            smallint NOT NULL,
    esi_journal_ref_id  bigint   NOT NULL,
    date                timestamptz NOT NULL,
    ref_type            text NOT NULL,
    amount              numeric(20,2) NOT NULL,
    balance             numeric(20,2) NOT NULL,
    first_party_id      bigint,
    second_party_id     bigint,
    context_id          bigint,
    context_id_type     text,
    reason              text,
    raw_json            jsonb NOT NULL,
    first_seen_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (corp_id, division, esi_journal_ref_id)
);
CREATE INDEX corp_wallet_journal_date_idx
    ON corp_wallet_journal(corp_id, division, date DESC);
CREATE INDEX corp_wallet_journal_contract_idx
    ON corp_wallet_journal(context_id)
    WHERE context_id_type = 'contract_id';

-- ── Principals ────────────────────────────────────────────────────────────────

CREATE TABLE principals (
    id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind    text NOT NULL CHECK (kind IN ('user','corp')),
    user_id uuid REFERENCES users(id) ON DELETE RESTRICT,
    corp_id uuid REFERENCES corps(id) ON DELETE RESTRICT,
    CHECK (
        (kind = 'user'  AND user_id IS NOT NULL AND corp_id IS NULL) OR
        (kind = 'corp'  AND corp_id IS NOT NULL AND user_id IS NULL)
    )
);
CREATE UNIQUE INDEX principals_user_unique ON principals(user_id) WHERE kind = 'user';
CREATE UNIQUE INDEX principals_corp_unique ON principals(corp_id) WHERE kind = 'corp';

-- Backfill: every existing user gets a user-principal.
INSERT INTO principals (kind, user_id)
SELECT 'user', id FROM users;

-- ── Refactor contracts: add principal-id columns ──────────────────────────────

ALTER TABLE contracts
    ADD COLUMN issuer_principal_id   uuid REFERENCES principals(id) ON DELETE RESTRICT NOT VALID,
    ADD COLUMN assignee_principal_id uuid REFERENCES principals(id) ON DELETE RESTRICT NOT VALID,
    ADD COLUMN wallet_verified_at          timestamptz,
    ADD COLUMN wallet_payout_aggregate_isk numeric(20,2);

-- Backfill from existing user-id columns via the principals table.
UPDATE contracts c
SET issuer_principal_id = p.id
FROM principals p
WHERE p.kind = 'user' AND p.user_id = c.issuer_user_id
  AND c.issuer_user_id IS NOT NULL;

UPDATE contracts c
SET assignee_principal_id = p.id
FROM principals p
WHERE p.kind = 'user' AND p.user_id = c.assignee_user_id
  AND c.assignee_user_id IS NOT NULL;

ALTER TABLE contracts VALIDATE CONSTRAINT contracts_issuer_principal_id_fkey;
ALTER TABLE contracts VALIDATE CONSTRAINT contracts_assignee_principal_id_fkey;

CREATE INDEX contracts_issuer_principal_idx
    ON contracts(issuer_principal_id) WHERE issuer_principal_id IS NOT NULL;
CREATE INDEX contracts_assignee_principal_idx
    ON contracts(assignee_principal_id) WHERE assignee_principal_id IS NOT NULL;

-- ── Refactor reimbursements: principal columns, corp-funded flag ──────────────

-- Drop the old unique constraint (it used user-id columns).
ALTER TABLE reimbursements
    DROP CONSTRAINT reimbursements_list_id_requester_user_id_hauler_user_id_key;

-- requester_user_id is now nullable (corp-funded rows have no user requester).
-- hauler_user_id stays NOT NULL for v1; haulers are always characters.
ALTER TABLE reimbursements
    ALTER COLUMN requester_user_id DROP NOT NULL;

ALTER TABLE reimbursements
    ADD COLUMN requester_principal_id uuid REFERENCES principals(id) ON DELETE RESTRICT NOT VALID,
    ADD COLUMN hauler_principal_id    uuid REFERENCES principals(id) ON DELETE RESTRICT NOT VALID,
    ADD COLUMN is_corp_funded         boolean NOT NULL DEFAULT false,
    ADD COLUMN verified_by_wallet     boolean NOT NULL DEFAULT false,
    ADD COLUMN wallet_settlement_delta_isk numeric(20,2);

-- Backfill principal ids from existing user-id columns.
UPDATE reimbursements r
SET requester_principal_id = p.id
FROM principals p
WHERE p.kind = 'user' AND p.user_id = r.requester_user_id
  AND r.requester_user_id IS NOT NULL;

UPDATE reimbursements r
SET hauler_principal_id = p.id
FROM principals p
WHERE p.kind = 'user' AND p.user_id = r.hauler_user_id
  AND r.hauler_user_id IS NOT NULL;

-- Now make the new columns NOT NULL (all rows have been backfilled).
ALTER TABLE reimbursements
    ALTER COLUMN requester_principal_id SET NOT NULL,
    ALTER COLUMN hauler_principal_id    SET NOT NULL;

ALTER TABLE reimbursements VALIDATE CONSTRAINT reimbursements_requester_principal_id_fkey;
ALTER TABLE reimbursements VALIDATE CONSTRAINT reimbursements_hauler_principal_id_fkey;

-- New partial unique index: one row per (list, requester-principal, hauler-principal)
-- where the row isn't cancelled. Cancelled rows don't block re-creation.
CREATE UNIQUE INDEX reimbursements_principal_unique
    ON reimbursements(list_id, requester_principal_id, hauler_principal_id)
    WHERE status <> 'cancelled';

CREATE INDEX reimbursements_requester_principal_idx
    ON reimbursements(requester_principal_id);
CREATE INDEX reimbursements_hauler_principal_idx
    ON reimbursements(hauler_principal_id);

-- ── Lists: payer corp columns ─────────────────────────────────────────────────

ALTER TABLE lists
    ADD COLUMN payer_corp_id  uuid REFERENCES corps(id) ON DELETE RESTRICT,
    ADD COLUMN payer_division smallint,
    ADD CONSTRAINT lists_payer_both_or_neither
        CHECK ((payer_corp_id IS NULL) = (payer_division IS NULL));

-- ── Meta ──────────────────────────────────────────────────────────────────────

UPDATE _jitacart_meta SET value = '7' WHERE key = 'schema_phase';
