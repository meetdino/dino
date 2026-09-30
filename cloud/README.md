# dino-cloud

The dino account server: sign-in, devices, and end-to-end encrypted settings sync. Rust (axum,
tokio, sqlx) on Postgres.

It never sees agent traffic, sessions or terminal content: those stay on the Mac, where dinod is
the only proxy. It stores accounts, the devices signed in to them, and settings that are sealed on
the device with a key this server never has. The account page shows everything it holds, and the
export returns all of it.

## What's here

| Area | Endpoints |
|---|---|
| OAuth 2 server | `GET /oauth/authorize` (code + PKCE S256, loopback redirects for native apps), `POST /oauth/token` (`authorization_code`, `refresh_token`, `urn:ietf:params:oauth:grant-type:device_code`), `POST /oauth/device_authorization`, `POST /oauth/revoke`, `POST /oauth/introspect`, `GET /oauth/userinfo`, `/.well-known/oauth-authorization-server` (also at `openid-configuration`) |
| Sign-in pages | `/signin` (GitHub, Google, emailed code), `/device` (approve a device-flow code), `/account` (devices, synced data, export, sign out everywhere, delete) |
| Account API (`/v1`, bearer) | `GET /me`, `GET /devices`, `DELETE /devices/{id}`, `POST /signout-everywhere`, `DELETE /account`, `GET /export` |
| Sync (`/v1/sync`, bearer) | `GET ?since=&limit=`, `POST` (a `dino_sync::PushRequest`), `GET /ws` (nudges), `POST /reset`, `GET`/`PUT /recovery`, `POST`/`GET /approvals`, `GET /approvals/{id}`, `POST /approvals/{id}/claim`, `/grant`, `/deny` |
| Operations | `/healthz`, `/readyz`, Prometheus `/metrics` on `DINO_METRICS_BIND` |

Clients: `dino` (the app and CLI) and `dino-harness` are native (loopback `http://127.0.0.1:<any
port>/callback`, and the device flow); `dino-web` is reserved for a separate web page. Harness
tokens carry audience `dino-harness` and don't open the `/v1` API; the harness checks them with
`/oauth/introspect`.

## How it works

- **Access tokens** are opaque (256 random bits, stored as SHA-256) and live 15 minutes. Being
  looked up on every request is what makes sign-out, device revocation and account deletion take
  effect immediately, with no signing keys to rotate.
- **Refresh tokens** rotate on every use, one family per device (RFC 9700 §4.14.2). A spent token
  presented again within 30 s returns the same replacement (it's derived from the spent one), so a
  client that lost a response isn't signed out; after that, reuse signs the device out.
- **Native sign-in** always shows a confirmation naming the device, so a program on the machine
  can't quietly use a signed-in browser. The redirect carries `iss` (RFC 9207).
- **Device flow** (RFC 8628 §5.4, draft-ietf-oauth-cross-device-security): the consent page names
  the app and device, shows the code large, warns when the request came from another network,
  needs a sign-in from the last 10 minutes, and code entry is rate limited.
- **Email codes**: six digits, stored as HMACs, 10 minutes, 5 tries, 5 per address per hour.
- **Accounts** link a new identity by verified email only. Deletion revokes everything at once and
  erases the rows after 30 days.
- **Sync** follows `dino-sync`: per-key last-writer-wins by hybrid logical clock, the same record
  twice accepted once, stamps over 10 minutes ahead refused, only PASETO `v4.local` values stored
  (5 MB per account). Writes to an account are serialized on its head row, so sequence numbers
  have no gaps. Nudges go through Postgres `LISTEN`/`NOTIFY`, so every node's sockets hear pushes
  made on any node.
- **Key approval**: a new device posts its public key, a signed-in one claims the request with its
  own (both screens then show the same code), then posts the account key sealed to the new device.
  The server only relays. The recovery-wrapped key is stored as sent.
- **Hardening**: per-address and per-account rate limits (GCRA), `Idempotency-Key` on `/v1`
  mutations, CSRF tokens and Origin checks on forms, a strict CSP, `__Host-` cookies on https, no
  query strings, bodies or tokens in logs.

## Run it locally

```sh
docker compose up -d db            # or any Postgres; see below
cp .env.example .env && set -a && . ./.env && set +a
cp .cargo/config.toml.example .cargo/config.toml   # optional: build dino-sync from ../dino-app-poc
cargo run -p dino-cloud
open http://127.0.0.1:8787/signin  # codes land in dino-cloud-mail.log
```

Without Docker, a throwaway Postgres works as well:

```sh
initdb -D /tmp/dcpg-data -U dino --auth=trust
pg_ctl -D /tmp/dcpg-data -o "-p 55432 -k /tmp -c listen_addresses=127.0.0.1" -l /tmp/dcpg.log start
createdb -h 127.0.0.1 -p 55432 -U dino dino_cloud
```

`dino-sync` comes from the dino repo, which is private for now. Cargo fetches it with the git CLI
(`.cargo/config.toml.example` turns that on), so your GitHub credentials apply.

## Tests

```sh
cargo test          # needs Postgres; DINO_TEST_DATABASE_URL, default postgres://dino:dino@127.0.0.1:55432/postgres
```

Each test gets its own database and a real server on a random port, with GitHub, Google and mail
faked. They cover the full PKCE sign-in, code replay, bad clients and redirects, refresh rotation
with the grace window, concurrent refreshes and reuse detection, the device flow with consent and
fresh sign-in, GitHub and Google linking to one account, sign-out everywhere, idempotency,
deletion and erasure, CSRF and Origin checks, rate limits, introspection, two devices converging
through sync, paging, nudges across two nodes, key approval and recovery, gapless sequences under
concurrent pushes, and that neither the logs nor a full database dump contain a token or any
plaintext value.

## Self-hosting

One container and a Postgres. Set `DINO_ENV=production`, `DINO_CLOUD_URL` (https),
`DINO_SECRET_KEY` (`openssl rand -base64 32`), `DATABASE_URL`, and a mail API; add GitHub or
Google OAuth apps whose callback is `<DINO_CLOUD_URL>/signin/<github|google>/callback`. Behind a
proxy that sets the client address, add `DINO_TRUST_PROXY=1`. Nodes are stateless apart from their
open WebSockets, so run as many as you like against one database.

```sh
docker build --secret id=github_token,env=GITHUB_TOKEN -t dino-cloud .
```

## License

MIT
