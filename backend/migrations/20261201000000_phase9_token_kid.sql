-- Phase 9 / M2: token-encryption key rotation by KID.
--
-- Refresh + access tokens for a given character are always re-encrypted
-- together (the worker's persist_rotations writes both with the current
-- primary key). One column captures the key id used for the row's
-- ciphertext blobs. Default 'v1' means: anything pre-rotation was
-- encrypted with whatever single key was configured at the time, which
-- the new MultiKeyCipher loads as kid 'v1' for backwards compatibility.

ALTER TABLE characters
    ADD COLUMN token_key_id text NOT NULL DEFAULT 'v1';

-- Sweeper looks for non-primary rows; an index keeps that scan cheap as
-- the table grows.
CREATE INDEX characters_token_key_id_idx ON characters (token_key_id);
