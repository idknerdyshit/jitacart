CREATE EXTENSION IF NOT EXISTS pgcrypto; -- gen_random_uuid()

CREATE TABLE users (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    display_name TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE characters (
    id                        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- EVE's numeric character_id. Globally unique across EVE.
    character_id              BIGINT NOT NULL UNIQUE,
    character_name            TEXT NOT NULL,
    -- EVE's owner_hash. If this changes for a given character_id, the
    -- character was transferred and existing tokens must be invalidated.
    owner_hash                TEXT NOT NULL,
    scopes                    TEXT[] NOT NULL DEFAULT '{}',
    -- AES-GCM ciphertext of the refresh token. nonce is stored separately so
    -- we can rotate the key by re-encrypting in place.
    refresh_token_ciphertext  BYTEA NOT NULL,
    refresh_token_nonce       BYTEA NOT NULL,
    -- Last access token + its expiry, to skip a refresh round-trip.
    -- access_token itself is short-lived and not strictly secret-at-rest, but
    -- treat it the same as refresh.
    access_token_ciphertext   BYTEA,
    access_token_nonce        BYTEA,
    access_token_expires_at   TIMESTAMPTZ,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_refreshed_at         TIMESTAMPTZ
);

CREATE INDEX characters_user_id_idx ON characters(user_id);

UPDATE _jitacart_meta SET value = '1' WHERE key = 'schema_phase';
