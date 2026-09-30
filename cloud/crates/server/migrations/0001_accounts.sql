-- Accounts, the identities that sign in to them, the devices (installs) they're signed in on, and
-- every credential the server hands out. Secrets are stored as SHA-256 hashes (tokens are 256-bit
-- random) or HMACs (short email codes), never as they were issued.

CREATE TABLE accounts (
    id              uuid PRIMARY KEY,
    email           text NOT NULL,
    email_verified  boolean NOT NULL DEFAULT false,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- Set when the owner deletes it: everything is revoked at once, rows go 30 days later.
    deleted_at      timestamptz
);
CREATE INDEX accounts_email ON accounts (lower(email));
CREATE INDEX accounts_deleted ON accounts (deleted_at) WHERE deleted_at IS NOT NULL;

CREATE TABLE identities (
    id          uuid PRIMARY KEY,
    account_id  uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    provider    text NOT NULL,          -- github | google | email
    subject     text NOT NULL,          -- the provider's stable user id (email address for email)
    email       text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);
CREATE INDEX identities_account ON identities (account_id);

-- One row per install that signed in; its refresh tokens form one rotation family.
CREATE TABLE devices (
    id            uuid PRIMARY KEY,
    account_id    uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    client_id     text NOT NULL,
    scope         text NOT NULL DEFAULT 'account',
    name          text NOT NULL,
    os            text NOT NULL DEFAULT '',
    dino_version  text NOT NULL DEFAULT '',
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at    timestamptz,
    revoke_reason text
);
CREATE INDEX devices_account ON devices (account_id);

CREATE TABLE refresh_tokens (
    hash         bytea PRIMARY KEY,
    device_id    uuid NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    account_id   uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    -- Set when this token was exchanged for the next one in the family.
    rotated_at   timestamptz
);
CREATE INDEX refresh_tokens_device ON refresh_tokens (device_id);

CREATE TABLE access_tokens (
    hash        bytea PRIMARY KEY,
    account_id  uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    device_id   uuid REFERENCES devices (id) ON DELETE CASCADE,
    client_id   text NOT NULL,
    aud         text NOT NULL,
    scope       text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX access_tokens_account ON access_tokens (account_id);
CREATE INDEX access_tokens_expires ON access_tokens (expires_at);

CREATE TABLE auth_codes (
    hash            bytea PRIMARY KEY,
    client_id       text NOT NULL,
    redirect_uri    text NOT NULL,
    code_challenge  text NOT NULL,
    account_id      uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    scope           text NOT NULL DEFAULT '',
    device_name     text NOT NULL,
    device_os       text NOT NULL DEFAULT '',
    dino_version    text NOT NULL DEFAULT '',
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    used_at         timestamptz,
    -- The device the code signed in, so a second use of the code can sign it out.
    device_id       uuid REFERENCES devices (id) ON DELETE SET NULL
);

CREATE TABLE device_codes (
    hash          bytea PRIMARY KEY,
    user_code     text NOT NULL UNIQUE,
    client_id     text NOT NULL,
    scope         text NOT NULL DEFAULT '',
    device_name   text NOT NULL,
    device_os     text NOT NULL DEFAULT '',
    dino_version  text NOT NULL DEFAULT '',
    requested_ip  text NOT NULL DEFAULT '',
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    interval_s    integer NOT NULL,
    last_poll_at  timestamptz,
    -- pending | approved | denied | used
    status        text NOT NULL DEFAULT 'pending',
    account_id    uuid REFERENCES accounts (id) ON DELETE CASCADE
);

CREATE TABLE email_codes (
    id          uuid PRIMARY KEY,
    email       text NOT NULL,
    code_mac    bytea NOT NULL,
    attempts    integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz
);
CREATE INDEX email_codes_email ON email_codes (lower(email), created_at);

-- Browser sessions for the server's own pages. A session exists before sign-in too, to carry the
-- CSRF token and the sign-in that was asked for.
CREATE TABLE web_sessions (
    hash        bytea PRIMARY KEY,
    account_id  uuid REFERENCES accounts (id) ON DELETE CASCADE,
    authed_at   timestamptz,
    data        jsonb NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX web_sessions_account ON web_sessions (account_id);

-- Replayed answers for mutations sent with an Idempotency-Key.
CREATE TABLE idempotency (
    scope       text NOT NULL,          -- account id, or the client IP before sign-in
    key         text NOT NULL,
    route       text NOT NULL,
    status      smallint NOT NULL,
    body        bytea NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, key, route)
);
CREATE INDEX idempotency_created ON idempotency (created_at);
