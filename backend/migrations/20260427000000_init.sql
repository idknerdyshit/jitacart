-- Initial schema for jitacart. Single consolidated migration; pre-production.

CREATE EXTENSION IF NOT EXISTS pgcrypto; -- gen_random_uuid()

-- ── Enums ───────────────────────────────────────────────────────────────────

CREATE TYPE group_role              AS ENUM ('owner', 'member');
CREATE TYPE market_kind             AS ENUM ('npc_hub', 'public_structure');
CREATE TYPE structure_access_status AS ENUM ('ok', 'forbidden', 'unknown');
CREATE TYPE list_status             AS ENUM ('open', 'closed', 'archived');
CREATE TYPE list_item_status        AS ENUM ('open', 'claimed', 'bought', 'delivered', 'settled');
CREATE TYPE claim_status            AS ENUM ('active', 'released', 'completed');
CREATE TYPE fulfillment_source      AS ENUM ('manual', 'contract');
CREATE TYPE reimbursement_status    AS ENUM ('pending', 'settled', 'cancelled');
CREATE TYPE contract_type           AS ENUM ('item_exchange', 'auction', 'courier', 'unknown');
CREATE TYPE contract_status         AS ENUM (
    'outstanding', 'in_progress', 'finished_issuer', 'finished_contractor',
    'finished', 'cancelled', 'rejected', 'failed', 'deleted', 'reversed'
);
CREATE TYPE contract_match_state    AS ENUM ('pending', 'confirmed', 'rejected', 'superseded');
CREATE TYPE principal_kind          AS ENUM ('user', 'corp');

-- ── Users / characters ──────────────────────────────────────────────────────

CREATE TABLE users (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    display_name         text NOT NULL,
    -- FK added after characters exists (circular: characters.user_id → users.id).
    active_character_id  uuid,
    created_at           timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE characters (
    id                        uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                   uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- EVE's numeric character_id. Globally unique across EVE.
    character_id              bigint NOT NULL UNIQUE,
    character_name            text NOT NULL,
    -- EVE's owner_hash. If this changes for a given character_id, the
    -- character was transferred and existing tokens must be invalidated.
    owner_hash                text NOT NULL,
    scopes                    text[] NOT NULL DEFAULT '{}',
    -- AES-GCM ciphertext of the refresh token. nonce is stored separately so
    -- we can rotate the key by re-encrypting in place.
    refresh_token_ciphertext  bytea NOT NULL,
    refresh_token_nonce       bytea NOT NULL,
    -- Last access token + its expiry, to skip a refresh round-trip.
    -- access_token itself is short-lived and not strictly secret-at-rest, but
    -- treat it the same as refresh.
    access_token_ciphertext   bytea,
    access_token_nonce        bytea,
    access_token_expires_at   timestamptz,
    -- KID for the at-rest cipher. Default 'v1' mirrors cipher.rs LEGACY_KID
    -- for the single-key (`token_enc_key`) configuration.
    token_key_id              text NOT NULL DEFAULT 'v1',
    -- Per-character contract poll cursor (staggered scheduling).
    contracts_next_poll_at    timestamptz,
    contracts_last_polled_at  timestamptz,
    created_at                timestamptz NOT NULL DEFAULT now(),
    last_refreshed_at         timestamptz
);
CREATE INDEX characters_user_id_idx ON characters(user_id);
CREATE INDEX characters_token_key_id_idx ON characters(token_key_id);
CREATE INDEX characters_contracts_due_idx
    ON characters(contracts_next_poll_at)
    WHERE 'esi-contracts.read_character_contracts.v1' = ANY(scopes);

ALTER TABLE users
    ADD CONSTRAINT users_active_character_fk
    FOREIGN KEY (active_character_id) REFERENCES characters(id) ON DELETE SET NULL;

CREATE FUNCTION check_active_character_ownership() RETURNS trigger AS $$
BEGIN
    IF NEW.active_character_id IS NOT NULL THEN
        IF NOT EXISTS (
            SELECT 1 FROM characters WHERE id = NEW.active_character_id AND user_id = NEW.id
        ) THEN
            RAISE EXCEPTION 'active_character_id must belong to the user';
        END IF;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_active_character_ownership
    BEFORE INSERT OR UPDATE OF active_character_id ON users
    FOR EACH ROW EXECUTE FUNCTION check_active_character_ownership();

-- ── Groups ──────────────────────────────────────────────────────────────────

CREATE TABLE groups (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name                text NOT NULL,
    invite_code         text NOT NULL UNIQUE,
    created_by_user_id  uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    default_tip_pct     numeric(5,4) NOT NULL DEFAULT 0
        CHECK (default_tip_pct BETWEEN 0 AND 1),
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE group_memberships (
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id   uuid NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    role       group_role NOT NULL,
    joined_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, group_id)
);
CREATE INDEX group_memberships_group_id_idx ON group_memberships(group_id);

CREATE TABLE group_discord_webhooks (
    group_id                     uuid PRIMARY KEY REFERENCES groups(id) ON DELETE CASCADE,
    webhook_url                  text NOT NULL,
    notify_list_created          boolean NOT NULL DEFAULT true,
    notify_list_claimed          boolean NOT NULL DEFAULT true,
    notify_list_delivered        boolean NOT NULL DEFAULT true,
    notify_reimbursement_settled boolean NOT NULL DEFAULT true,
    created_at                   timestamptz NOT NULL DEFAULT now(),
    updated_at                   timestamptz NOT NULL DEFAULT now()
);

-- ── Markets / type cache ────────────────────────────────────────────────────

CREATE TABLE type_cache (
    name_key   text PRIMARY KEY,             -- lowercased + NBSP-normalized lookup key
    name       text NOT NULL,                -- canonical display casing returned by ESI
    type_id    bigint NOT NULL,
    type_name  text NOT NULL,
    cached_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX type_cache_type_id_idx ON type_cache (type_id);

CREATE TABLE markets (
    id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind                   market_kind NOT NULL,
    esi_location_id        bigint NOT NULL UNIQUE,
    -- Nullable for citadels: discovery inserts a row before the detail fetch
    -- resolves region/name/short_label. NPC hubs always have all three; the
    -- markets_npc_hub_fields_present check enforces that.
    region_id              bigint,
    name                   text,
    short_label            text,
    is_hub                 boolean NOT NULL DEFAULT false,
    is_public              boolean NOT NULL DEFAULT true,
    last_seen_public_at    timestamptz,
    last_auth_error_at     timestamptz,
    solar_system_id        bigint,
    structure_type_id      integer,
    last_orders_synced_at  timestamptz,
    details_synced_at      timestamptz,
    missing_poll_count     integer NOT NULL DEFAULT 0,
    untrackable_until      timestamptz,
    created_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT markets_npc_hub_fields_present CHECK (
        kind <> 'npc_hub'
        OR (region_id IS NOT NULL AND name IS NOT NULL AND short_label IS NOT NULL)
    )
);
CREATE INDEX markets_kind_public_idx
    ON markets (kind, is_public) WHERE kind = 'public_structure';
-- Backlog index for the durable detail-backfill worker.
CREATE INDEX markets_details_pending_idx
    ON markets (created_at)
    WHERE kind = 'public_structure' AND details_synced_at IS NULL AND is_public = true;

INSERT INTO markets (kind, esi_location_id, region_id, name, short_label, is_hub) VALUES
 ('npc_hub', 60003760, 10000002, 'Jita IV - Moon 4 - Caldari Navy Assembly Plant',         'Jita',    true),
 ('npc_hub', 60008494, 10000043, 'Amarr VIII (Oris) - Emperor Family Academy',             'Amarr',   true),
 ('npc_hub', 60011866, 10000032, 'Dodixie IX - Moon 20 - Federation Navy Assembly Plant',  'Dodixie', true),
 ('npc_hub', 60004588, 10000030, 'Rens VI - Moon 8 - Brutor Tribe Treasury',               'Rens',    true),
 ('npc_hub', 60005686, 10000042, 'Hek VIII - Moon 12 - Boundless Creation Factory',        'Hek',     true);

CREATE TABLE market_prices (
    market_id    uuid   NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
    type_id      bigint NOT NULL,
    best_sell    numeric(20,2),
    best_buy     numeric(20,2),
    sell_volume  bigint NOT NULL DEFAULT 0,
    buy_volume   bigint NOT NULL DEFAULT 0,
    computed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (market_id, type_id)
);
CREATE INDEX market_prices_stale_idx ON market_prices (computed_at);
-- No persistent etag column: nea-esi keeps an in-memory ETag cache keyed per
-- request URL, and its market_orders helper is paginated, so a single column
-- per (market_id, type_id) wouldn't represent the page-level cache.

CREATE TABLE group_tracked_markets (
    group_id          uuid        NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    market_id         uuid        NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
    added_by_user_id  uuid        NOT NULL REFERENCES users(id),
    added_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, market_id)
);
CREATE INDEX group_tracked_markets_market_idx ON group_tracked_markets (market_id);

-- Two distinct access dimensions: details (universe scope) vs market orders
-- (markets scope + docking access). A 403 on one does not imply the other.
CREATE TABLE character_structure_access (
    character_id        uuid NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    market_id           uuid NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
    details_status      structure_access_status NOT NULL DEFAULT 'unknown',
    details_checked_at  timestamptz,
    market_status       structure_access_status NOT NULL DEFAULT 'unknown',
    market_checked_at   timestamptz,
    PRIMARY KEY (character_id, market_id)
);
CREATE INDEX csa_market_ok_idx
    ON character_structure_access (market_id) WHERE market_status = 'ok';
CREATE INDEX csa_details_ok_idx
    ON character_structure_access (market_id) WHERE details_status = 'ok';

-- ── Corps / principals ──────────────────────────────────────────────────────

CREATE TABLE corps (
    id                        uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    esi_corporation_id        bigint NOT NULL UNIQUE,
    name                      text NOT NULL,
    ticker                    text NOT NULL,
    last_synced_at            timestamptz,
    last_auth_error_at        timestamptz,
    disabled_at               timestamptz,
    contracts_next_poll_at    timestamptz,
    contracts_last_polled_at  timestamptz,
    wallet_next_poll_at       timestamptz,
    wallet_last_polled_at     timestamptz,
    created_at                timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX corps_contracts_due_idx ON corps(contracts_next_poll_at) WHERE disabled_at IS NULL;
CREATE INDEX corps_wallet_due_idx    ON corps(wallet_next_poll_at)    WHERE disabled_at IS NULL;

-- Group → Corp link. One row per pair; soft-unlink toggles unlinked_at.
CREATE TABLE group_corps (
    group_id          uuid NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    corp_id           uuid NOT NULL REFERENCES corps(id)  ON DELETE RESTRICT,
    linked_at         timestamptz NOT NULL DEFAULT now(),
    linked_by_user_id uuid NOT NULL REFERENCES users(id),
    unlinked_at       timestamptz,
    PRIMARY KEY (group_id, corp_id)
);

-- Characters authorized to act as ESI ambassadors for a corp.
-- contributed_via_group_id scopes ambassadorship to the contributing group so
-- unlink_corp can revoke only that group's ambassadors.
CREATE TABLE corp_ambassadors (
    corp_id                  uuid NOT NULL REFERENCES corps(id) ON DELETE CASCADE,
    character_id             uuid NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
    granted_scopes           text[] NOT NULL DEFAULT '{}',
    last_used_at             timestamptz,
    last_auth_error_at       timestamptz,
    disabled_at              timestamptz,
    contributed_via_group_id uuid REFERENCES groups(id) ON DELETE SET NULL,
    PRIMARY KEY (corp_id, character_id)
);
CREATE INDEX corp_ambassadors_active_idx ON corp_ambassadors(corp_id) WHERE disabled_at IS NULL;
CREATE INDEX corp_ambassadors_group_idx
    ON corp_ambassadors(contributed_via_group_id)
    WHERE contributed_via_group_id IS NOT NULL;

CREATE TABLE corp_wallet_divisions (
    corp_id        uuid     NOT NULL REFERENCES corps(id) ON DELETE CASCADE,
    division       smallint NOT NULL CHECK (division BETWEEN 1 AND 7),
    name           text,
    balance_isk    numeric(20,2) NOT NULL DEFAULT 0,
    last_synced_at timestamptz,
    PRIMARY KEY (corp_id, division)
);

-- Wallet journal entries (audit-only; contract status drives settlement).
-- The journal is the audit trail. Corp deletion with journal history must be
-- an explicit failure (RESTRICT), not a silent cascade.
CREATE TABLE corp_wallet_journal (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    corp_id             uuid     NOT NULL REFERENCES corps(id) ON DELETE RESTRICT,
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
    ON corp_wallet_journal(context_id) WHERE context_id_type = 'contract_id';

CREATE TABLE principals (
    id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind    principal_kind NOT NULL,
    user_id uuid REFERENCES users(id) ON DELETE RESTRICT,
    corp_id uuid REFERENCES corps(id) ON DELETE RESTRICT,
    CHECK (
        (kind = 'user' AND user_id IS NOT NULL AND corp_id IS NULL) OR
        (kind = 'corp' AND corp_id IS NOT NULL AND user_id IS NULL)
    )
);
CREATE UNIQUE INDEX principals_user_unique ON principals(user_id) WHERE kind = 'user';
CREATE UNIQUE INDEX principals_corp_unique ON principals(corp_id) WHERE kind = 'corp';

-- ── Lists / items ───────────────────────────────────────────────────────────

CREATE TABLE lists (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            uuid NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by_user_id  uuid NOT NULL REFERENCES users(id),
    destination_label   text,
    notes               text,
    status              list_status NOT NULL DEFAULT 'open',
    total_estimate_isk  numeric(24,2) NOT NULL DEFAULT 0,
    tip_pct             numeric(5,4) NOT NULL DEFAULT 0
        CHECK (tip_pct BETWEEN 0 AND 1),
    payer_corp_id       uuid REFERENCES corps(id) ON DELETE RESTRICT,
    payer_division      smallint,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lists_payer_both_or_neither
        CHECK ((payer_corp_id IS NULL) = (payer_division IS NULL))
);
CREATE INDEX lists_group_status_idx ON lists (group_id, status);

CREATE TABLE list_items (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id              uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    type_id              bigint NOT NULL,
    type_name            text NOT NULL,
    qty_requested        bigint NOT NULL CHECK (qty_requested > 0),
    qty_fulfilled        bigint NOT NULL DEFAULT 0 CHECK (qty_fulfilled >= 0),
    est_unit_price_isk   numeric(20,2),
    est_priced_market_id uuid REFERENCES markets(id),
    requested_by_user_id uuid REFERENCES users(id),
    status               list_item_status NOT NULL DEFAULT 'open',
    source_line_no       int,
    created_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX list_items_list_status_idx ON list_items (list_id, status);
-- Worker tick query joins list_items on type_id; without this it seq-scans.
CREATE INDEX list_items_type_id_idx ON list_items (type_id);

CREATE TABLE list_markets (
    list_id     uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    market_id   uuid NOT NULL REFERENCES markets(id) ON DELETE RESTRICT,
    is_primary  boolean NOT NULL DEFAULT false,
    added_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (list_id, market_id)
);
-- Partial unique: at most one primary per list. Postgres syntax is index-level,
-- not table-level. Swap-primary is two updates in one tx (clear all → set one).
CREATE UNIQUE INDEX list_markets_one_primary
    ON list_markets (list_id) WHERE is_primary;

-- ── Claims / fulfillments ───────────────────────────────────────────────────

CREATE TABLE claims (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id         uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    hauler_user_id  uuid NOT NULL REFERENCES users(id),
    status          claim_status NOT NULL DEFAULT 'active',
    note            text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    released_at     timestamptz
);
CREATE INDEX claims_list_status_idx ON claims (list_id, status);

CREATE TABLE claim_items (
    claim_id     uuid NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    list_item_id uuid NOT NULL REFERENCES list_items(id) ON DELETE CASCADE,
    active       boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (claim_id, list_item_id)
);
-- Race protection: at most one active claim per list_item.
CREATE UNIQUE INDEX claim_items_one_active
    ON claim_items (list_item_id) WHERE active;

-- Trigger keeps claim_items.active in sync when a claim's status changes.
CREATE FUNCTION sync_claim_items_active() RETURNS trigger AS $$
BEGIN
    IF NEW.status <> 'active' AND OLD.status = 'active' THEN
        UPDATE claim_items SET active = false WHERE claim_id = NEW.id;
    ELSIF NEW.status = 'active' AND OLD.status <> 'active' THEN
        UPDATE claim_items SET active = true  WHERE claim_id = NEW.id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER claims_sync_items AFTER UPDATE OF status ON claims
    FOR EACH ROW EXECUTE FUNCTION sync_claim_items_active();

CREATE TABLE fulfillments (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_item_id         uuid NOT NULL REFERENCES list_items(id) ON DELETE CASCADE,
    claim_id             uuid REFERENCES claims(id) ON DELETE SET NULL,
    hauler_user_id       uuid NOT NULL REFERENCES users(id),
    hauler_character_id  uuid REFERENCES characters(id),
    source               fulfillment_source NOT NULL DEFAULT 'manual',
    qty                  bigint NOT NULL CHECK (qty > 0),
    unit_price_isk       numeric(20,2) NOT NULL CHECK (unit_price_isk >= 0),
    bought_at_market_id  uuid REFERENCES markets(id),
    bought_at_note       text,
    bought_at            timestamptz NOT NULL DEFAULT now(),
    reversed_at          timestamptz,
    CHECK (
        bought_at_market_id IS NOT NULL
        OR nullif(btrim(bought_at_note), '') IS NOT NULL
    )
);
CREATE INDEX fulfillments_item_active_idx
    ON fulfillments (list_item_id) WHERE reversed_at IS NULL;
CREATE INDEX fulfillments_hauler_idx
    ON fulfillments (hauler_user_id, bought_at DESC);
-- Supports settle_reimbursement's NOT EXISTS subquery and the per-(list,
-- requester, hauler) reimbursement aggregation.
CREATE INDEX fulfillments_item_hauler_active_idx
    ON fulfillments (list_item_id, hauler_user_id) WHERE reversed_at IS NULL;

-- ── Contracts / reimbursements / matcher ────────────────────────────────────

CREATE TABLE contracts (
    id                          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    esi_contract_id             bigint NOT NULL UNIQUE,
    issuer_character_id         bigint NOT NULL,
    issuer_user_id              uuid REFERENCES users(id),
    issuer_principal_id         uuid REFERENCES principals(id) ON DELETE RESTRICT,
    assignee_character_id       bigint,
    assignee_user_id            uuid REFERENCES users(id),
    assignee_principal_id       uuid REFERENCES principals(id) ON DELETE RESTRICT,
    contract_type               contract_type   NOT NULL,
    status                      contract_status NOT NULL,
    price_isk                   numeric(20,2) NOT NULL DEFAULT 0,
    reward_isk                  numeric(20,2) NOT NULL DEFAULT 0,
    collateral_isk              numeric(20,2) NOT NULL DEFAULT 0,
    expected_total_isk          numeric(20,2),
    settlement_delta_isk        numeric(20,2),
    date_issued                 timestamptz NOT NULL,
    date_expired                timestamptz,
    date_accepted               timestamptz,
    date_completed              timestamptz,
    start_location_id           bigint,
    end_location_id             bigint,
    raw_json                    jsonb NOT NULL,
    items_synced_at             timestamptz,
    wallet_verified_at          timestamptz,
    wallet_payout_aggregate_isk numeric(20,2),
    -- Which corp discovered this contract; lets item-sync fall back to the
    -- corp ESI endpoint when neither party is a tracked character.
    source_corp_id              uuid REFERENCES corps(id) ON DELETE SET NULL,
    first_seen_at               timestamptz NOT NULL DEFAULT now(),
    updated_at                  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX contracts_status_idx       ON contracts(status);
CREATE INDEX contracts_issuer_user_idx  ON contracts(issuer_user_id)      WHERE issuer_user_id IS NOT NULL;
CREATE INDEX contracts_needs_items_idx  ON contracts(items_synced_at)     WHERE items_synced_at IS NULL;
CREATE INDEX contracts_issuer_principal_idx
    ON contracts(issuer_principal_id)   WHERE issuer_principal_id   IS NOT NULL;
CREATE INDEX contracts_assignee_principal_idx
    ON contracts(assignee_principal_id) WHERE assignee_principal_id IS NOT NULL;
CREATE INDEX contracts_source_corp_idx
    ON contracts(source_corp_id)        WHERE source_corp_id IS NOT NULL;

CREATE TABLE contract_items (
    contract_id  uuid NOT NULL REFERENCES contracts(id) ON DELETE CASCADE,
    record_id    bigint NOT NULL,
    type_id      integer NOT NULL,
    quantity     bigint NOT NULL,
    is_included  boolean NOT NULL,
    PRIMARY KEY (contract_id, record_id)
);
CREATE INDEX contract_items_type_idx ON contract_items(type_id);

-- One row per (list, requester-principal, hauler-principal) where not
-- cancelled. Concurrent claims can leave requester A owing hauler B and
-- hauler D separately; settling B does not settle D.
CREATE TABLE reimbursements (
    id                          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id                     uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    -- Nullable: corp-funded rows have no user requester.
    requester_user_id           uuid REFERENCES users(id),
    requester_principal_id      uuid NOT NULL REFERENCES principals(id) ON DELETE RESTRICT,
    -- v1: haulers are always characters, so user id stays NOT NULL.
    hauler_user_id              uuid NOT NULL REFERENCES users(id),
    hauler_principal_id         uuid NOT NULL REFERENCES principals(id) ON DELETE RESTRICT,
    subtotal_isk                numeric(24,2) NOT NULL DEFAULT 0,
    tip_isk                     numeric(24,2) NOT NULL DEFAULT 0,
    total_isk                   numeric(24,2) NOT NULL DEFAULT 0,
    status                      reimbursement_status NOT NULL DEFAULT 'pending',
    settled_at                  timestamptz,
    settled_by_user_id          uuid REFERENCES users(id),
    contract_id                 uuid REFERENCES contracts(id) ON DELETE SET NULL,
    is_corp_funded              boolean NOT NULL DEFAULT false,
    verified_by_wallet          boolean NOT NULL DEFAULT false,
    wallet_settlement_delta_isk numeric(20,2),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    updated_at                  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX reimbursements_list_idx     ON reimbursements (list_id);
CREATE INDEX reimbursements_contract_idx ON reimbursements(contract_id) WHERE contract_id IS NOT NULL;
CREATE INDEX reimbursements_requester_principal_idx ON reimbursements(requester_principal_id);
CREATE INDEX reimbursements_hauler_principal_idx    ON reimbursements(hauler_principal_id);
CREATE UNIQUE INDEX reimbursements_principal_unique
    ON reimbursements(list_id, requester_principal_id, hauler_principal_id)
    WHERE status <> 'cancelled';
-- Matcher and confirm/manual-link "already-bound" check filter pending,
-- unbound rows by (hauler, requester). Two indexes: the user-id form covers
-- legacy/personal lookups; the principal-id form is what the matcher uses
-- for corp-funded rows where requester_user_id is NULL.
CREATE INDEX reimbursements_matcher_idx
    ON reimbursements (hauler_user_id, requester_user_id)
    WHERE contract_id IS NULL AND status = 'pending';
CREATE INDEX reimbursements_matcher_principal_idx
    ON reimbursements (hauler_principal_id, requester_principal_id)
    WHERE contract_id IS NULL AND status = 'pending';

CREATE TABLE contract_match_suggestions (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id        uuid NOT NULL REFERENCES contracts(id) ON DELETE CASCADE,
    reimbursement_id   uuid NOT NULL REFERENCES reimbursements(id) ON DELETE CASCADE,
    score              numeric(5,4) NOT NULL CHECK (score >= 0 AND score <= 1),
    exact_match        boolean NOT NULL,
    state              contract_match_state NOT NULL DEFAULT 'pending',
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
