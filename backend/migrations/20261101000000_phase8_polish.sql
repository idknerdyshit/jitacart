-- Phase 8: active character preference + Discord webhook config

ALTER TABLE users
    ADD COLUMN active_character_id uuid REFERENCES characters(id) ON DELETE SET NULL;

CREATE OR REPLACE FUNCTION check_active_character_ownership() RETURNS trigger AS $$
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
