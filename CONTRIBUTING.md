# Contributing to dino

Thanks for helping. This page covers how to build dino, test a change without disturbing the dino
you use, and get it merged.

## Setup

- macOS 14 or later, the Xcode Command Line Tools (`xcode-select --install`), and Rust through
  [rustup](https://rustup.rs) (stable, 1.85 or later: the workspace uses edition 2024).
- Optional, for the end-to-end checks: [Claude Code](https://claude.com/claude-code) signed in, and
  Python 3 (macOS has it).

```sh
cargo build --release      # target/release/dino: the CLI, and dinod (`dino daemon`)
./app/build.sh             # app/build/Dino.app, "dino dev": a build to try things in
./app/build.sh --install   # the dino you use, built from here: ~/Applications/Dino.app
```

"dino dev" has its own bundle id, so it shares no settings or permissions with the dino you use,
and runs the `dino` it finds on your `PATH` or in `~/.local/bin` (or `DINO_BIN`, when set): link
the one you built, `ln -sf "$PWD/target/release/dino" ~/.local/bin/dino`. An installed build
carries its `dino` and runs dinod from it, as a release does.

To keep the dino you use built from main, install the git hooks once:

```sh
scripts/install-hooks.sh   # main moves in the main checkout: ./app/build.sh --install, in the background
```

Then every commit or merge to main in the main checkout rebuilds it (log: `app/build/build.log`)
and the running dino offers Restart to Update; `scripts/check.sh --push` from any worktree
fast-forwards the main checkout's main too, unless it has changes of its own. To sign with your
Developer ID, `git config dino.signingIdentity "Developer ID Application: Name (TEAMID)"`.

## Where things are

| Path | What it is |
| --- | --- |
| `crates/dino` | The `dino` command line, and `dinod`'s entry point |
| `crates/dino-daemon` | `dinod`: sessions and their PTYs, discovery, the tree, tmux, updates, sign-in and sync |
| `crates/dino-core` | Settings, the agent adapters, discovery, worktrees, PRs, and the IPC types |
| `crates/dino-term` | Terminal emulation behind each session |
| `crates/dino-proxy`, `crates/dino-router` | The local proxy: per-session routes, usage, the free models pool |
| `crates/dino-sync` | The settings sync protocol, shared with the account server |
| `crates/boundaries` | A test that keeps crates within the dependencies they're allowed |
| `app/` | Dino.app (Swift, SwiftUI, libghostty) |
| `scripts/` | Checks, the performance budget, releases |
| `cloud/` | The account server: sign-in, devices and settings sync (its own workspace, below) |

[ARCHITECTURE.md](ARCHITECTURE.md) explains how they fit, which crate may use which, and which
interfaces are public contracts (the IPC protocol, the settings schema, the sync protocol): change
those compatibly, so new fields have defaults and unknown ones round-trip.

## Test without touching your own dino

`dinod` keeps its socket, settings and sessions in `~/.config/dino`, or in `DINO_HOME` when it's
set. Give every experiment its own home, and you never disturb the agents you're running:

```sh
DINO_HOME=/tmp/dino-dev ./target/release/dino new shell
DINO_HOME=/tmp/dino-dev ./target/release/dino ls
DINO_HOME=/tmp/dino-dev ./target/release/dino stop
```

For the app, pass the same `DINO_HOME` (and `DINO_BIN`, the `dino` it should use) in its
environment, and give the copy you test its own bundle id, so it shares nothing with an installed
Dino.app. To try an installed build (its own dinod as a launch agent, the move to a new build),
make one for its own home:

```sh
DINO_BUNDLE_ID=dev.dino.app.test DINO_AGENT_HOME=/tmp/dino-dev DINO_APP=/tmp/dino-dev-app/Dino.app ./app/build.sh --install
```

## Before a pull request

```sh
scripts/check.sh           # builds and tests the workspace and the app
```

It runs `cargo build`, `cargo test --workspace` and the app's `swift build` (the dev profile, quick
to build; releases build the shipping one), and
refuses code marked `TEST-ONLY`. CI runs the same on every pull request.

dino is a terminal first, so speed is a feature. If your change touches the app or `dinod`, also
run the performance budget once:

```sh
scripts/budget.py          # about 5 minutes; keep its window visible
```

It starts an isolated copy of dinod and the app with a realistic load and fails when it's over
budget: idle CPU, CPU while output streams, keystroke latency added by dinod, and throughput.
`--no-claude` skips the part that runs a real Claude Code session. Paste its summary into your
pull request. Some guidelines that keep dino fast:

- Don't publish SwiftUI state that hasn't changed: writing the same value to a `@Published` set or
  dictionary still redraws whatever watches it.
- No timers or polling where an event exists.
- Measure before and after when a change could cost something.

## The account server (`cloud/`)

`cloud/` is a Cargo workspace of its own, on Linux or macOS: Rust through rustup
(`cloud/rust-toolchain.toml` picks the version) and Postgres, from Docker or Homebrew.
`scripts/check.sh` doesn't build it; its own CI job does, whenever `cloud/` or `crates/dino-sync`
changes. In `cloud/`:

```sh
scripts/dev.sh start       # Postgres, migrations, the server on http://127.0.0.1:8787
cargo test                 # needs Postgres; see cloud/README.md for DINO_TEST_DATABASE_URL
```

To try it with dino, point a test dinod at it, as above:
`DINO_HOME=/tmp/dino-dev DINO_CLOUD_URL=http://127.0.0.1:8787 dino daemon`, then
`DINO_HOME=/tmp/dino-dev dino login`.

| Path | What it is |
| --- | --- |
| `cloud/crates/server` | The server: OAuth, sign-in pages, the account API, sync, operations |
| `cloud/crates/server/migrations` | Postgres migrations, run at start |
| `cloud/crates/server/tests` | End-to-end tests against a real server and database, with GitHub, Google and mail faked |

The sync protocol (`crates/dino-sync`) and the account API are contracts with every dino out there:
change them compatibly, and when a change can't be, raise the protocol version so older clients
are told to upgrade. A change to `crates/dino-sync` reaches dinod and the server together, so run
both test suites. Never log tokens, codes, request bodies or query strings; the tests check the
logs for this.

## Commits and pull requests

- One change per pull request, with a description of what changes for someone using dino and how
  you tested it.
- Commit messages say what changed and why, in plain words. Keep the style of the code around you:
  its naming, its comments, its idioms.
- Add a test where the change can be tested, and check new behaviour end to end with a real agent
  where it involves one.

### Sign off your commits (DCO)

dino uses the [Developer Certificate of Origin](https://developercertificate.org): by signing off,
you certify that you wrote the change or otherwise have the right to submit it under the project's
license (MIT). Add the sign-off with `-s`:

```sh
git commit -s -m "Fix the tab strip in light mode"
```

which adds a line like `Signed-off-by: Your Name <you@example.com>`, matching the commit's author. A
check on every pull request looks for it. Forgot? `git commit --amend -s` for the last commit, or
`git rebase --signoff main` for all of them, then force-push your branch.

## Reporting bugs and ideas

Open an issue with what you did, what you expected and what happened, plus your macOS version, the
dino version (`dino --version`) and the agent involved. `~/.config/dino/dinod.log` often says why.
For security issues, don't open an issue: see [SECURITY.md](SECURITY.md).

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
