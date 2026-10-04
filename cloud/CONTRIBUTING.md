# Contributing to dino-cloud

Thanks for helping. dino-cloud is dino's account server: sign-in, devices and settings sync. The
README explains how it works; this page covers working on it.

## Setup

Rust (through [rustup](https://rustup.rs); `rust-toolchain.toml` picks the version) and Postgres,
either from Docker or from Homebrew.

```sh
scripts/dev.sh start     # Postgres, migrations, the server on http://127.0.0.1:8787
cargo test               # needs Postgres; see the README for DINO_TEST_DATABASE_URL
```

To try a change with dino itself, point a test dinod at your copy, so the dino you use every day
isn't touched:

```sh
DINO_HOME=/tmp/dino-dev DINO_CLOUD_URL=http://127.0.0.1:8787 dino daemon
DINO_HOME=/tmp/dino-dev dino login
```

## Where things are

| Path | What it is |
| --- | --- |
| `crates/server` | The server: OAuth, sign-in pages, the account API, sync, operations |
| `crates/server/migrations` | Postgres migrations, run at start |
| `crates/server/tests` | End-to-end tests against a real server and database, with GitHub, Google and mail faked |
| `crates/dino-sync` | The sync protocol, a copy of the crate in the dino repository: change it there, then `scripts/update-dino-sync.sh` |

The sync protocol and the account API are contracts with every dino out there: change them
compatibly (new fields have defaults, unknown ones are kept), and when a change can't be, raise the
protocol version so older clients are told to upgrade.

## Pull requests

- One change per pull request, saying what changes and how you tested it. Add a test where it can
  be tested: the suite runs the real server, so most flows can be.
- Never log tokens, codes, request bodies or query strings; the tests check the logs for this.
- Commit messages say what changed and why, in plain words. Keep the style of the code around you.

### Sign off your commits (DCO)

dino-cloud uses the [Developer Certificate of Origin](https://developercertificate.org): by signing
off, you certify that you wrote the change or otherwise have the right to submit it under the
project's license (MIT). Commit with `-s`:

```sh
git commit -s -m "Rate limit device codes per network"
```

A check on every pull request looks for the `Signed-off-by` line. Forgot? `git commit --amend -s`,
or `git rebase --signoff main`, then force-push your branch.

Security issues go through [SECURITY.md](SECURITY.md), not issues. Everyone taking part follows the
[Code of Conduct](CODE_OF_CONDUCT.md).
