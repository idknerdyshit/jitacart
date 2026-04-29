-- Supports the settle_reimbursement NOT EXISTS subquery and the
-- per-(list, requester, hauler) reimbursement aggregation, both of which
-- filter on (list_item_id, hauler_user_id) WHERE reversed_at IS NULL.
CREATE INDEX fulfillments_item_hauler_active_idx
    ON fulfillments (list_item_id, hauler_user_id) WHERE reversed_at IS NULL;
