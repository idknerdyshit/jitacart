-- pgcrypto is already enabled in phase1.

CREATE TABLE type_cache (
    name_key    text PRIMARY KEY,             -- lowercased + NBSP-normalized lookup key
    name        text NOT NULL,                -- canonical display casing returned by ESI
    type_id     bigint NOT NULL,
    type_name   text NOT NULL,
    cached_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX type_cache_type_id_idx ON type_cache (type_id);

CREATE TABLE markets (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind                text NOT NULL CHECK (kind IN ('npc_hub','public_structure')),
    esi_location_id     bigint NOT NULL UNIQUE,
    region_id           bigint NOT NULL,
    name                text NOT NULL,
    short_label         text NOT NULL,
    is_hub              boolean NOT NULL DEFAULT false,
    is_public           boolean NOT NULL DEFAULT true,
    last_seen_public_at timestamptz,
    last_auth_error_at  timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now()
);

INSERT INTO markets (kind, esi_location_id, region_id, name, short_label, is_hub) VALUES
 ('npc_hub', 60003760, 10000002, 'Jita IV - Moon 4 - Caldari Navy Assembly Plant',         'Jita',    true),
 ('npc_hub', 60008494, 10000043, 'Amarr VIII (Oris) - Emperor Family Academy',             'Amarr',   true),
 ('npc_hub', 60011866, 10000032, 'Dodixie IX - Moon 20 - Federation Navy Assembly Plant',  'Dodixie', true),
 ('npc_hub', 60004588, 10000030, 'Rens VI - Moon 8 - Brutor Tribe Treasury',               'Rens',    true),
 ('npc_hub', 60005686, 10000042, 'Hek VIII - Moon 12 - Boundless Creation Factory',        'Hek',     true);

CREATE TABLE market_prices (
    market_id   uuid   NOT NULL REFERENCES markets(id) ON DELETE CASCADE,
    type_id     bigint NOT NULL,
    best_sell   numeric(20,2),
    best_buy    numeric(20,2),
    sell_volume bigint NOT NULL DEFAULT 0,
    buy_volume  bigint NOT NULL DEFAULT 0,
    computed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (market_id, type_id)
);
CREATE INDEX market_prices_stale_idx ON market_prices (computed_at);
-- No persistent etag column: nea-esi keeps an in-memory ETag cache keyed per
-- request URL (lib.rs:390), and its market_orders helper (endpoints/market.rs:25)
-- is paginated, so a single column per (market_id, type_id) wouldn't represent
-- the page-level cache anyway. Revisit if Phase 4 traffic warrants it.

CREATE TABLE lists (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            uuid NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by_user_id  uuid NOT NULL REFERENCES users(id),
    destination_label   text,
    notes               text,
    status              text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open','closed','archived')),
    total_estimate_isk  numeric(24,2) NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lists_group_status_idx ON lists (group_id, status);

CREATE TABLE list_items (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    list_id              uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    type_id              bigint NOT NULL,
    type_name            text NOT NULL,
    qty_requested        bigint NOT NULL CHECK (qty_requested > 0),
    qty_fulfilled        bigint NOT NULL DEFAULT 0,
    est_unit_price_isk   numeric(20,2),
    est_priced_market_id uuid REFERENCES markets(id),
    requested_by_user_id uuid REFERENCES users(id),
    status               text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open','claimed','bought','delivered','settled')),
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
-- not table-level. Swap-primary is two updates in one tx (clear all -> set one).
CREATE UNIQUE INDEX list_markets_one_primary
  ON list_markets (list_id) WHERE is_primary;

UPDATE _jitacart_meta SET value = '3' WHERE key = 'schema_phase';
