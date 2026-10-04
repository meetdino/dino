# dino-cloud

The dino account server: sign-in (GitHub first, or a link by email), devices, and settings sync.
Rust (axum, tokio, sqlx) on Postgres.

It never sees agent traffic, sessions or terminal content: those stay on the Mac, where dinod is
the only proxy. It stores accounts, the devices signed in to them, and dino's settings as plain
JSON, like VS Code's settings sync. Secrets (API keys, tokens) never sync: they stay on each Mac.
The account page shows everything it holds, settings included, and the export returns all of it.

## What's here

| Area | Endpoints |
|---|---|
| OAuth 2 server | `GET /oauth/authorize` (code + PKCE S256, loopback redirects for native apps), `POST /oauth/token` (`authorization_code`, `refresh_token`, `urn:ietf:params:oauth:grant-type:device_code`), `POST /oauth/device_authorization` (with `email`: a sign-in link by mail, opened at `/login/{token}`), `POST /oauth/revoke`, `POST /oauth/introspect`, `GET /oauth/userinfo`, `/.well-known/oauth-authorization-server` (also at `openid-configuration`) |
| Sign-in pages | `/signin` (GitHub, Google, emailed code), `/device` (approve a device-flow code), `/account` (devices, synced data, export, sign out everywhere, delete) |
| Account API (`/v1`, bearer) | `GET /me`, `GET /devices`, `DELETE /devices/{id}`, `POST /signout-everywhere`, `DELETE /account`, `GET /export` |
| Sync (`/v1/sync`, bearer) | `GET ?since=&limit=`, `POST` (a `dino_sync::PushRequest`), `GET /ws` (nudges) |
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
- **Native sign-in** shows a confirmation naming the device, so a program on the machine can't
  quietly use a signed-in browser. The redirect carries `iss` (RFC 9207). With `provider=github`
  (dino's "Sign in with GitHub") it's one click: straight to GitHub, and the code goes back to the
  app after GitHub's answer, with GitHub's own consent standing in for the confirmation.
- **Sign-in links** (`dino login --email`, the app's "Use email instead"): a device-code request
  with an address mails a link; opening it shows a button, and pressing it approves that request
  (a mail scanner fetching the link uses nothing up). Random, stored hashed, used once, 15 minutes,
  5 per address per hour, and the device's answer is the same whether an account exists or not.
- **Device flow** (RFC 8628 §5.4, draft-ietf-oauth-cross-device-security): the consent page names
  the app and device, shows the code large, warns when the request came from another network,
  needs a sign-in from the last 10 minutes, and code entry is rate limited.
- **Email codes**: six digits, stored as HMACs, 10 minutes, 5 tries, 5 per address per hour.
- **Accounts** link a new identity by verified email only. Deletion revokes everything at once and
  erases the rows after 30 days.
- **Sync** follows `dino-sync`: per-key last-writer-wins by hybrid logical clock, the same record
  twice accepted once, stamps over 10 minutes ahead refused, values stored as JSON (5 MB per
  account). A client on an older protocol (the encrypted protocol 2) is told to upgrade (426). Writes to an account are serialized on its head row, so sequence numbers
  have no gaps. Devices look for changes on their own (see [Push](#push-optional)); with push on,
  nudges go through Postgres `LISTEN`/`NOTIFY`, so every node's sockets hear pushes made on any
  node.
- **The database out of reach** (a serverless Postgres waking up, Postgres restarting): requests answer 503 with
  `Retry-After` instead of failing, and the server waits for it at start rather than exiting.
- **Hardening**: per-address and per-account rate limits (GCRA), `Idempotency-Key` on `/v1`
  mutations, CSRF tokens and Origin checks on forms, a strict CSP, `__Host-` cookies on https, no
  query strings, bodies or tokens in logs.

## Run it locally

Commands here run in `cloud/`, the server's own Cargo workspace. One command runs a copy on this machine, standing in for the remote one:

```sh
scripts/dev.sh start     # Postgres, migrations, the server on http://127.0.0.1:8787
scripts/dev.sh status
scripts/dev.sh stop      # stops everything it started
scripts/dev.sh reset     # stops it and deletes its data
```

Postgres comes from `docker compose` when Docker is running, else from Homebrew's `postgresql@18`
in a data folder of its own (`~/.local/share/dino-cloud-dev`, port 55433). The server is built from
this checkout and runs in development mode: there's no mail, and sign-in codes are written to
`~/.local/share/dino-cloud-dev/mail.log`. Logs go next to it. `DINO_DEV_DIR`, `DINO_DEV_PORT` and
`DINO_DEV_PG_PORT` move things; `DINO_DEV_DOCKER=0` skips Docker.

To use it from dino, point a test dinod at it, so your real one isn't touched:

```sh
DINO_HOME=/tmp/dino-dev DINO_CLOUD_URL=http://127.0.0.1:8787 dino daemon
DINO_HOME=/tmp/dino-dev dino login
```

or sign a dino in to it directly with `dino login http://127.0.0.1:8787`.

Development mode serves plain http only on 127.0.0.1 or localhost; anywhere else, and always in
production, the server needs https.

The sync protocol is the repository's own `crates/dino-sync` (`../crates/dino-sync`), the same
crate dinod uses, without its `settings` feature. A change to the protocol shows in the server's
build and tests at once; CI runs them whenever `cloud/` or `crates/dino-sync` changes.

To run the server by hand instead: `cp .env.example .env`, load it, and `cargo run -p dino-cloud`.

## Tests

```sh
cargo test          # needs Postgres; DINO_TEST_DATABASE_URL, default postgres://dino:dino@127.0.0.1:55432/postgres
```

Each test gets its own database and a real server on a random port, with GitHub, Google and mail
faked. They cover the full PKCE sign-in, code replay, bad clients and redirects, refresh rotation
with the grace window, concurrent refreshes and reuse detection, the device flow with consent and
fresh sign-in, GitHub and Google linking to one account, sign-out everywhere, idempotency,
deletion and erasure, CSRF and Origin checks, rate limits, introspection, two devices converging
through sync, paging, nudges across two nodes, older clients told to upgrade, gapless sequences
under concurrent pushes, sign-in links (single use, expiry, per-address limits, no enumeration),
one-click GitHub joining an email account, a 503 when the database is out of reach, and that the
logs contain no token.

## Where it runs

dino's own instance is `https://cloud.meetdino.com`: the API, sign-in, device approval and the
account page on one host (the product site is `meetdino.com`). dino uses it unless `DINO_CLOUD_URL`
or `dino login <server>` names another, such as a self-hosted one or the local copy above.

## Deploying on a serverless host (Vercel)

The server also runs as a serverless service. `vercel.json` describes it for Vercel: Vercel's Rust
builder compiles `crates/server` and runs it as a standalone server on `$PORT`. When it sees
`VERCEL=1` it runs as a serverless host should: no push socket and no `LISTEN`, the limits on
sign-in attempts and sync writes counted in Postgres so they hold across instances, cleanup only
when Vercel Cron calls `/internal/cron` (daily, with `CRON_SECRET`), small database pools, and the
client address from Vercel's headers. Migrations run when an instance starts, over the unpooled
connection with Postgres' advisory lock, so instances starting together take turns and a
transaction-mode pooler never sees the lock.

1. Import the repository as a Vercel project with the **Services** framework preset (it picks up
   `vercel.json`). Set the project's **Root Directory** to `cloud`, and turn on **Include files
   outside the root directory in the Build Step**: the server builds on `crates/dino-sync`.
2. Connect a Postgres database. With a pooler in front (Neon's integration, say), set both
   `DATABASE_URL` (pooled) and `DATABASE_URL_UNPOOLED` (direct), which migrations use.
3. Set the environment variables (mark the secrets as sensitive):

   | Name | Value |
   |---|---|
   | `DINO_ENV` | `production` |
   | `DINO_CLOUD_URL` | your server's https URL, e.g. `https://dino.example.com` |
   | `DINO_SECRET_KEY` | the output of `openssl rand -base64 32` |
   | `DINO_MAIL_KEY` | a [Resend](https://resend.com) API key, or set `DINO_MAIL_URL` too for another mail API of the same shape |
   | `DINO_MAIL_FROM` | `dino <no-reply@example.com>`, from a domain your mail API has verified |
   | `CRON_SECRET` | another `openssl rand -base64 32` |
   | `DINO_GITHUB_CLIENT_ID`, `DINO_GITHUB_CLIENT_SECRET` | optional: a GitHub OAuth app whose callback is `<DINO_CLOUD_URL>/signin/github/callback` |
   | `DINO_GOOGLE_CLIENT_ID`, `DINO_GOOGLE_CLIENT_SECRET` | optional: the same for Google |

4. Deploy, then open `<your URL>/readyz`: it answers `200` once the database is reachable and
   migrated.

Check it from a Mac: `DINO_HOME=/tmp/dino-try dino login <your URL>`.

## Push (optional)

By default a device looks for changes every minute, right away when the Mac wakes, changes network
or comes back to the app, and every few seconds while the
Account pane is open. That needs nothing from the server but plain requests, so it runs anywhere,
serverless included.

With push on (`DINO_PUSH=1`, the default anywhere but Vercel), the server also keeps a WebSocket
per device at `/v1/sync/ws` and nudges it the moment the account changes, fanned out between nodes
with Postgres `LISTEN`/`NOTIFY`. `GET /v1/meta` tells devices which it is, and they use the socket
when it's there and fall back to looking when it isn't. Turn it on where the server stays up (a
VM, ECS, the local copy): for instant updates, and for things that need them, like picking a
session up on a phone. It needs a direct (unpooled) database connection for `LISTEN`.

## Self-hosting

One container and a Postgres. Set `DINO_ENV=production`, `DINO_CLOUD_URL` (https),
`DINO_SECRET_KEY` (`openssl rand -base64 32`), `DATABASE_URL`, and a mail API; add GitHub or
Google OAuth apps whose callback is `<DINO_CLOUD_URL>/signin/<github|google>/callback`. Behind a
proxy that sets the client address, add `DINO_TRUST_PROXY=1`. Nodes are stateless apart from their
open WebSockets (with push on), so run as many as you like against one database. A platform that
assigns the port through `PORT` is followed.

```sh
docker build -f cloud/Dockerfile -t dino-cloud .     # from the repository root
```

## Contributing

Issues and pull requests are welcome: see the repository's [CONTRIBUTING.md](../CONTRIBUTING.md),
which covers setup, tests and signing off your commits (DCO). Report security issues privately, as
[SECURITY.md](../SECURITY.md) says.

## License

MIT, see [LICENSE](../LICENSE). The third-party code the server builds on, and its licenses, are
listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
