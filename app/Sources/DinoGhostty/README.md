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
- `TerminalController.configText`/`configFlag`/`configColor`/`configBits`/`configMilliseconds`/
  `configCount`/`configNumber`/`configPath`/`configQuickTerminalSize`: a config value as Ghostty
  resolved it;
  `configTrigger`: the key an action is bound to, as a Mac key code and Carbon modifiers.
- `TerminalHostAction` types `progressReport`, `desktopNotification`, `commandFinished` and
  `ringBell` (`TerminalActionEvent.isPaneSignal`), so a host can take them before the wrapper
  publishes them to the surface's state.
- `TerminalHostAction` also types search (`searchTotal`, `searchSelected`), `mouseVisibility`,
  `keySequence` (the key, written as the Mac writes shortcuts) and `keyTable`.
- Right-click (`Platform/AppKit/AppTerminalView+Input.swift`): goes to Ghostty first, as in
  Ghostty's app, so `right-click-action` decides; when Ghostty leaves it to the host (the default,
  `context-menu`), AppKit shows `contextMenu()`, which a host overrides. Control-click is a
  right-click when no program captures the mouse. Upstream showed a Copy menu over a selection
  itself and sent the click to Ghostty otherwise.
- `Resources/Ghostty/themes`: Ghostty 1.3.1's color themes (from iTerm2-Color-Schemes, MIT, see
  `Resources/Ghostty/LICENSE-themes`), so `theme = Name` resolves without Ghostty.app. Taken from
  Ghostty 1.3.1's `Contents/Resources/ghostty/themes`.
- Focus (`Platform/AppKit/AppTerminalView+Lifecycle.swift`, `TerminalSurfaceCoordinator.hasKeyFocus`):
  a surface is focused only as the first responder of the key window, and a new one is told
  whether it is: Ghostty takes a surface as focused until told otherwise, and says so to a program
  that turns on focus reports. Upstream focused the first responder of any window, key or not,
  so a pane in a hidden or background window reported itself focused (and `dino attach`, which
  follows focus to decide a session's size, took it for the one the user was looking at).
- VoiceOver (`Platform/AppKit/AppTerminalView+Accessibility.swift`, `TerminalSurface.readScreenText`):
  the view is a text area whose value is the terminal's text, scrollback included, with its
  selection, lines and font, as Ghostty 1.2+'s `SurfaceView_AppKit` gives them. Upstream exposed
  nothing on the Mac.
- `TerminalController.reapplyConfig(to:)`: Ghostty's soft `reload_config`, the loaded config given
  again to a surface or the app, so a `light:…,dark:…` theme follows each surface's light or dark.

To take a newer upstream: copy its `Sources/GhosttyTerminal` over this folder, bump the package's
version in `app/Package.swift`, and put the changes above back.
