-- Phase 5: Hauler Manual Fulfillment

ALTER TABLE groups ADD COLUMN default_tip_pct numeric(5,4) NOT NULL DEFAULT 0
    CHECK (default_tip_pct BETWEEN 0 AND 1);
ALTER TABLE lists  ADD COLUMN tip_pct         numeric(5,4) NOT NULL DEFAULT 0
    CHECK (tip_pct BETWEEN 0 AND 1);
UPDATE lists SET tip_pct = g.default_tip_pct FROM groups g WHERE g.id = lists.group_id;

CREATE TABLE claims (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    hauler_user_id uuid NOT NULL REFERENCES users(id),
    status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','released','completed')),
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz
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
        UPDATE claim_items SET active = true WHERE claim_id = NEW.id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER claims_sync_items AFTER UPDATE OF status ON claims
    FOR EACH ROW EXECUTE FUNCTION sync_claim_items_active();

CREATE TABLE fulfillments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_item_id uuid NOT NULL REFERENCES list_items(id) ON DELETE CASCADE,
    claim_id uuid REFERENCES claims(id) ON DELETE SET NULL,
    hauler_user_id uuid NOT NULL REFERENCES users(id),
    hauler_character_id uuid REFERENCES characters(id),
    source text NOT NULL DEFAULT 'manual'
        CHECK (source IN ('manual','contract')),
    qty bigint NOT NULL CHECK (qty > 0),
    unit_price_isk numeric(20,2) NOT NULL CHECK (unit_price_isk >= 0),
    bought_at_market_id uuid REFERENCES markets(id),
    bought_at_note text,
    bought_at timestamptz NOT NULL DEFAULT now(),
    reversed_at timestamptz,
    CHECK (
        bought_at_market_id IS NOT NULL
        OR nullif(btrim(bought_at_note), '') IS NOT NULL
    )
);
CREATE INDEX fulfillments_item_active_idx
    ON fulfillments (list_item_id) WHERE reversed_at IS NULL;
CREATE INDEX fulfillments_hauler_idx
    ON fulfillments (hauler_user_id, bought_at DESC);

-- One row per (list, requester, hauler). With concurrent claims, requester A
-- can owe hauler B and hauler D separately; settling B does not settle D.
CREATE TABLE reimbursements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    requester_user_id uuid NOT NULL REFERENCES users(id),
    hauler_user_id    uuid NOT NULL REFERENCES users(id),
    subtotal_isk numeric(24,2) NOT NULL DEFAULT 0,
    tip_isk      numeric(24,2) NOT NULL DEFAULT 0,
    total_isk    numeric(24,2) NOT NULL DEFAULT 0,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','settled','cancelled')),
    settled_at timestamptz,
    settled_by_user_id uuid REFERENCES users(id),
    contract_id uuid,                  -- Phase 6: will FK to contracts(id)
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (list_id, requester_user_id, hauler_user_id)
);
CREATE INDEX reimbursements_list_idx ON reimbursements (list_id);

UPDATE _jitacart_meta SET value = '5' WHERE key = 'schema_phase';
