# DinoGhostty: the GhosttyTerminal wrapper, carried

The Swift wrapper around libghostty from [libghostty-spm](https://github.com/Lakr233/libghostty-spm)
by Lakr233, MIT licensed (see `LICENSE` here). Taken from the package's `Sources/GhosttyTerminal`
at tag `1.6.20260928` (5a025555f0a85ee51da7eb306c35f660d116e879) and renamed to the module
`DinoGhostty`: SwiftPM won't have two targets named `GhosttyTerminal` in one graph. The engine
itself (`GhosttyKit`, the libghostty xcframework) still comes from that package at the same version
(`app/Package.swift`).

dino carries it so every action Ghostty sends can reach the app, not only the ones the wrapper
models. Changes from upstream:

- `TerminalController.onAction` (`Controller/TerminalController+HostActions.swift`): every action
  Ghostty sends, app-wide ones included, reaches the host first as a `TerminalActionEvent` (the raw
  `ghostty_action_s`, a typed `TerminalHostAction`, the surface's `TerminalViewState`); returning
  true performs it. Upstream answered 13 surface actions and dropped the rest, app-wide ones
  unseen. The wrapper's own handling of those 13 is unchanged when the host returns false.
- `TerminalSurfaceStateDelegate`: a delegate standing in front of a `TerminalViewState` says which,
  so actions name the right surface.
- `TerminalController.configText`/`configFlag`: a config value as Ghostty resolved it.

To take a newer upstream: copy its `Sources/GhosttyTerminal` over this folder, bump the package's
version in `app/Package.swift`, and put the changes above back.
