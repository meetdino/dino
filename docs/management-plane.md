# dino as a management plane

Everything about the agents on this Mac, in one place: see them, know which need you, act on
them. Works with no account; an optional Dino login later syncs settings across Macs.

## Principles

- **Local-first, login optional.** Download, open, it works. Login is offered, never required
  (VS Code's model).
- **dinod owns state.** Sessions, repos, settings and keys live in the daemon. The app, the CLI
  and the proxy are clients, so a setting made in the app applies to agents started anywhere.
- **No setting that does nothing.** A control appears once dino enforces it.

## 1. Tree: repo → worktree → sessions

```
▾ dino-app-poc                     repo (main worktree's folder name)
  ▾ main                           worktree: branch, or folder when detached
      ● claude   working           sessions whose cwd is inside that worktree
  ▾ Fan-out "fix the attach…"      a fan-out group; each member is its own worktree
      ● claude  +42 −7
      ● codex   +38 −5
  ▸ feat/auth                      a worktree with no sessions: select it to start one there
▾ ~/scratch                        folders outside git are their own top-level node
```

- dinod answers `Tree`: repos found from session cwds, fan-out repos and the current folder,
  each with `git worktree list`. Sessions carry `cwd`; the app files each under the deepest
  worktree containing it. Fan-out worktrees fold into their group.
- Selecting a repo or worktree makes it the folder new sessions start in; the detail pane shows
  the start screen for it. That replaces "Choose Folder" as the main way to pick where to work.
- Expansion state is a `Set` of paths in `@SceneStorage`; nested nodes are `DisclosureGroup`s
  so expansion is controllable (List's `children:` isn't).
- Running agents outside dino stay under "On this Mac" for now; later they file into the tree
  too, marked as outside.

## 2. Settings

- One document, `~/.config/dino/settings.toml`, read and written by dinod (`Settings` /
  `SetSettings` requests, JSON over the socket; the app never parses TOML). It replaces
  `config` (`onboarded`, `route`), migrating it once.
- Split like VS Code: *synced* keys (routing, agent defaults, policies) and *machine* keys
  (paths, this Mac's repos). Sync is later, but the split is decided now.
- Keys: the app sets and removes keys through dinod (`SetKey`), which never sends values back,
  only which keys exist. Storage stays the 600 file for now: an ad-hoc signed dinod gets a
  Keychain prompt after every rebuild, and access groups need a provisioning profile. Keychain
  comes with Developer ID signing, behind the same `SetKey` interface.
- Native Settings window (⌘,): General, Agents, Models & Routing, Keys, Policies.

## 3. Policies (per repo, with defaults)

Only what dinod enforces: which agents may start, default agent, auto-trust dino worktrees
(fixes fan-out stalling on "Trust this folder?"), token budget per session.

## 4. First run

Welcome: what dino found (agents, keys, repos), then "Log in with Dino to sync across your
Macs", skippable. Until the service exists that option says so plainly instead of pretending.

## Order

Tree → settings document and window (General, Routing, Keys) → policies → welcome → overview.
