-- Sync protocol 3 (dino-sync PROTOCOL = 3): settings travel as plain JSON over TLS, like VS Code's
-- settings sync, and secrets (API keys, tokens) never sync. The end-to-end encrypted records can't
-- be read without keys the server never had, so they go, along with the key exchange: each Mac
-- sends its settings again when it signs in. Accounts, identities and devices stay.
DROP TABLE sync_approvals;
DROP TABLE sync_keys;
DELETE FROM sync_records;
ALTER TABLE sync_records ALTER COLUMN value TYPE jsonb USING 'null'::jsonb;
-- The sequence keeps counting up (nothing a device saw before looks new); a new generation tells
-- devices that only look to start over.
UPDATE sync_heads SET bytes = 0, generation = generation + 1, updated_at = now();

-- A sign-in link sent by email for a device's sign-in request (the device grant): opening it
-- approves that request for the address's account. Stored hashed, used once, short-lived.
CREATE TABLE email_links (
    hash         bytea PRIMARY KEY,
    device_hash  bytea NOT NULL REFERENCES device_codes (hash) ON DELETE CASCADE,
    email        text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    used_at      timestamptz
);
CREATE INDEX email_links_email ON email_links (email, created_at);
