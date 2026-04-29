-- Phase 6 follow-up indexes.
--
-- The matcher and the confirm/manual-link "already-bound" check both filter
-- reimbursements by (hauler_user_id, requester_user_id) restricted to pending
-- unbound rows. Phase 5 only added an index on list_id, leaving these queries
-- on a seq scan. Partial index keeps it small.

CREATE INDEX reimbursements_matcher_idx
    ON reimbursements (hauler_user_id, requester_user_id)
    WHERE contract_id IS NULL AND status = 'pending';
