-- Settings sync. Values are PASETO v4.local tokens sealed on the device with the account key; the
-- server stores them as they arrive and never has the key. What it can see: where a record lives
-- (collection and key), when and by which device it was written, and its size.

-- One row per account: the sequence every accepted write advances, and the bytes held.
CREATE TABLE sync_heads (
    account_id  uuid PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    seq         bigint NOT NULL DEFAULT 0,
    bytes       bigint NOT NULL DEFAULT 0,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- The latest write of each setting (per-key last-writer-wins by HLC). A NULL value is a delete,
-- kept so it reaches every device.
CREATE TABLE sync_records (
    account_id   uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    collection   text NOT NULL,
    key          text NOT NULL,
    seq          bigint NOT NULL,
    hlc_wall     bigint NOT NULL,
    hlc_counter  bigint NOT NULL,
    hlc_device   text NOT NULL,
    schema       integer NOT NULL,
    value        text,
    -- Fields a newer client sent that this server doesn't know, returned unchanged.
    extra        jsonb NOT NULL DEFAULT '{}',
    bytes        integer NOT NULL,
    device_id    uuid REFERENCES devices (id) ON DELETE SET NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, collection, key)
);
CREATE INDEX sync_records_seq ON sync_records (account_id, seq);

-- The account key wrapped with the recovery key, for signing in when no device is around.
CREATE TABLE sync_keys (
    account_id        uuid PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    wrapped_recovery  text NOT NULL,
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- A new device asking a signed-in one for the account key. The server relays public keys and the
-- sealed grant; it can't open the grant.
CREATE TABLE sync_approvals (
    id               uuid PRIMARY KEY,
    account_id       uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    device_id        uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    public_key       text NOT NULL,
    -- pending | claimed | granted | denied
    status           text NOT NULL DEFAULT 'pending',
    approver_device  uuid REFERENCES devices (id) ON DELETE CASCADE,
    approver_key     text,
    grant_body       jsonb,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL
);
CREATE INDEX sync_approvals_account ON sync_approvals (account_id, status);
