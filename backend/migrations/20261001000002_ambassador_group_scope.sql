-- Scope ambassadors to the group that contributed them so unlink_corp can
-- revoke only that group's ambassadors, not every group's.

ALTER TABLE corp_ambassadors
    ADD COLUMN contributed_via_group_id uuid REFERENCES groups(id) ON DELETE SET NULL;

CREATE INDEX corp_ambassadors_group_idx
    ON corp_ambassadors(contributed_via_group_id)
    WHERE contributed_via_group_id IS NOT NULL;
