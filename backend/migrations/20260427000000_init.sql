-- Phase 0: placeholder so the embedded sqlx migrator has something to run
-- against a fresh database. Real schema lands in Phase 1+.
CREATE TABLE IF NOT EXISTS _jitacart_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO _jitacart_meta (key, value)
VALUES ('schema_phase', '0')
ON CONFLICT (key) DO NOTHING;
