//
//  TerminalController+HostActions.swift
//  dino's addition to libghostty-spm's wrapper (see README.md)
//

import Foundation
import GhosttyKit

/// An action Ghostty asked the host to perform: from a keybind (`new_tab`, `goto_split:next`), a
/// program (a title, a bell), or the engine itself. Every action reaches
/// ``TerminalController/onAction`` before the wrapper's own handling, app-wide ones included.
@MainActor
public struct TerminalActionEvent {
    /// The action as Ghostty sent it. Pointers in it are only valid during the call.
    public let raw: ghostty_action_s
    /// The surface it's for: nil for an app-wide action (`quit`, `open_config`, `reload_config`).
    public let state: TerminalViewState?
    /// The wrapper handles it itself when the host doesn't (a title, a bell, a URL).
    public let wrapperHandles: Bool

    public var tag: ghostty_action_tag_e { raw.tag }
    public var isAppTarget: Bool { state == nil }

    /// A program's progress or notification, a command's end, or the bell.
    public var isPaneSignal: Bool {
        switch raw.tag {
        case GHOSTTY_ACTION_PROGRESS_REPORT, GHOSTTY_ACTION_DESKTOP_NOTIFICATION, GHOSTTY_ACTION_COMMAND_FINISHED, GHOSTTY_ACTION_RING_BELL: true
        default: false
        }
    }

    /// What it asks for, typed, for the actions a host can implement; `.other` for the rest.
    public var action: TerminalHostAction { TerminalHostAction(raw) }

    /// Ghostty's name for the action, for logs: "new_split", "toggle_fullscreen".
    public var name: String { TerminalHostAction.name(of: raw.tag) }
}

/// A surface delegate standing in front of a ``TerminalViewState`` (forwarding its callbacks, say)
/// says which, so the actions for that surface name it.
@MainActor
public protocol TerminalSurfaceStateDelegate: TerminalSurfaceViewDelegate {
    var viewState: TerminalViewState? { get }
}

/// The actions a host implements, with what each carries.
public enum TerminalHostAction: Equatable, Sendable {
    public enum CloseTabMode: Sendable { case this, others, right }
    public enum SplitDirection: Sendable { case right, down, left, up }
    public enum GotoSplit: Sendable { case previous, next, up, left, down, right }
    public enum ResizeDirection: Sendable { case up, down, left, right }
    public enum GotoTab: Equatable, Sendable { case previous, next, last, index(Int) }
    public enum PromptTitle: Sendable { case surface, tab, window }
    public enum Toggle: Sendable { case on, off, toggle }
    /// A key table's change: one pushed by name, the top one popped, or all of them.
    public enum KeyTable: Equatable, Sendable { case activate(String), deactivate, deactivateAll }

    case quit
    case newWindow
    case newTab
    case closeTab(CloseTabMode)
    case closeWindow
    case closeAllWindows
    case newSplit(SplitDirection)
    case gotoSplit(GotoSplit)
    case resizeSplit(amount: UInt16, ResizeDirection)
    case equalizeSplits
    case toggleSplitZoom
    case gotoTab(GotoTab)
    case moveTab(Int)
    case toggleFullscreen
    case toggleMaximize
    case toggleQuickTerminal
    case toggleCommandPalette
    case toggleVisibility
    /// `soft`: apply the config already loaded again, without reading the files.
    case reloadConfig(soft: Bool)
    case openConfig
    case promptTitle(PromptTitle)
    case copyTitleToClipboard
    case checkForUpdates
    case floatWindow(Toggle)
    case secureInput(Toggle)
    case undo
    case redo
    case startSearch(String?)
    case endSearch
    /// Matches found so far (nil: not known yet).
    case searchTotal(Int?)
    /// The selected match, from 0 (nil: none).
    case searchSelected(Int?)
    /// `mouse-hide-while-typing`: hide the pointer until it moves, or show it.
    case mouseVisibility(visible: Bool)
    /// A key of a key sequence was pressed and more are expected (the key, as
    /// "⌃A"), or the sequence ended (nil).
    case keySequence(String?)
    case keyTable(KeyTable)
    case inspector
    case presentTerminal
    /// OSC 9;4: a program's progress (percent 0–100, nil when it gave none).
    case progressReport(TerminalProgressState, percent: Int?)
    /// OSC 9 or OSC 777: a program asks for a desktop notification (an empty title when it gave none).
    case desktopNotification(title: String, body: String)
    /// Shell integration (OSC 133): a command ended, with its exit code when known.
    case commandFinished(exitCode: Int?, durationNanos: UInt64)
    case ringBell
    case other

    init(_ raw: ghostty_action_s) {
        let a = raw.action
        switch raw.tag {
        case GHOSTTY_ACTION_QUIT: self = .quit
        case GHOSTTY_ACTION_NEW_WINDOW: self = .newWindow
        case GHOSTTY_ACTION_NEW_TAB: self = .newTab
        case GHOSTTY_ACTION_CLOSE_TAB:
            switch a.close_tab_mode {
            case GHOSTTY_ACTION_CLOSE_TAB_MODE_OTHER: self = .closeTab(.others)
            case GHOSTTY_ACTION_CLOSE_TAB_MODE_RIGHT: self = .closeTab(.right)
            default: self = .closeTab(.this)
            }
        case GHOSTTY_ACTION_CLOSE_WINDOW: self = .closeWindow
        case GHOSTTY_ACTION_CLOSE_ALL_WINDOWS: self = .closeAllWindows
        case GHOSTTY_ACTION_NEW_SPLIT:
            switch a.new_split {
            case GHOSTTY_SPLIT_DIRECTION_DOWN: self = .newSplit(.down)
            case GHOSTTY_SPLIT_DIRECTION_LEFT: self = .newSplit(.left)
            case GHOSTTY_SPLIT_DIRECTION_UP: self = .newSplit(.up)
            default: self = .newSplit(.right)
            }
        case GHOSTTY_ACTION_GOTO_SPLIT:
            switch a.goto_split {
            case GHOSTTY_GOTO_SPLIT_PREVIOUS: self = .gotoSplit(.previous)
            case GHOSTTY_GOTO_SPLIT_UP: self = .gotoSplit(.up)
            case GHOSTTY_GOTO_SPLIT_LEFT: self = .gotoSplit(.left)
            case GHOSTTY_GOTO_SPLIT_DOWN: self = .gotoSplit(.down)
            case GHOSTTY_GOTO_SPLIT_RIGHT: self = .gotoSplit(.right)
            default: self = .gotoSplit(.next)
            }
        case GHOSTTY_ACTION_RESIZE_SPLIT:
            let r = a.resize_split
            let direction: ResizeDirection = switch r.direction {
            case GHOSTTY_RESIZE_SPLIT_UP: .up
            case GHOSTTY_RESIZE_SPLIT_DOWN: .down
            case GHOSTTY_RESIZE_SPLIT_LEFT: .left
            default: .right
            }
            self = .resizeSplit(amount: r.amount, direction)
        case GHOSTTY_ACTION_EQUALIZE_SPLITS: self = .equalizeSplits
        case GHOSTTY_ACTION_TOGGLE_SPLIT_ZOOM: self = .toggleSplitZoom
        case GHOSTTY_ACTION_GOTO_TAB:
            switch a.goto_tab {
            case GHOSTTY_GOTO_TAB_PREVIOUS: self = .gotoTab(.previous)
            case GHOSTTY_GOTO_TAB_NEXT: self = .gotoTab(.next)
            case GHOSTTY_GOTO_TAB_LAST: self = .gotoTab(.last)
            // `goto_tab:N` counts from 1.
            default: self = .gotoTab(.index(Int(a.goto_tab.rawValue)))
            }
        case GHOSTTY_ACTION_MOVE_TAB: self = .moveTab(Int(a.move_tab.amount))
        case GHOSTTY_ACTION_TOGGLE_FULLSCREEN: self = .toggleFullscreen
        case GHOSTTY_ACTION_TOGGLE_MAXIMIZE: self = .toggleMaximize
        case GHOSTTY_ACTION_TOGGLE_QUICK_TERMINAL: self = .toggleQuickTerminal
        case GHOSTTY_ACTION_TOGGLE_COMMAND_PALETTE: self = .toggleCommandPalette
        case GHOSTTY_ACTION_TOGGLE_VISIBILITY: self = .toggleVisibility
        case GHOSTTY_ACTION_RELOAD_CONFIG: self = .reloadConfig(soft: a.reload_config.soft)
        case GHOSTTY_ACTION_OPEN_CONFIG: self = .openConfig
        case GHOSTTY_ACTION_PROMPT_TITLE:
            switch a.prompt_title {
            case GHOSTTY_PROMPT_TITLE_TAB: self = .promptTitle(.tab)
            case GHOSTTY_PROMPT_TITLE_WINDOW: self = .promptTitle(.window)
            default: self = .promptTitle(.surface)
            }
        case GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD: self = .copyTitleToClipboard
        case GHOSTTY_ACTION_CHECK_FOR_UPDATES: self = .checkForUpdates
        case GHOSTTY_ACTION_FLOAT_WINDOW:
            switch a.float_window {
            case GHOSTTY_FLOAT_WINDOW_ON: self = .floatWindow(.on)
            case GHOSTTY_FLOAT_WINDOW_OFF: self = .floatWindow(.off)
            default: self = .floatWindow(.toggle)
            }
        case GHOSTTY_ACTION_SECURE_INPUT:
            switch a.secure_input {
            case GHOSTTY_SECURE_INPUT_ON: self = .secureInput(.on)
            case GHOSTTY_SECURE_INPUT_OFF: self = .secureInput(.off)
            default: self = .secureInput(.toggle)
            }
        case GHOSTTY_ACTION_UNDO: self = .undo
        case GHOSTTY_ACTION_REDO: self = .redo
        case GHOSTTY_ACTION_START_SEARCH:
            let needle = a.start_search.needle.map { String(cString: $0) }
            self = .startSearch(needle.flatMap { $0.isEmpty ? nil : $0 })
        case GHOSTTY_ACTION_END_SEARCH: self = .endSearch
        case GHOSTTY_ACTION_SEARCH_TOTAL:
            self = .searchTotal(a.search_total.total >= 0 ? Int(a.search_total.total) : nil)
        case GHOSTTY_ACTION_SEARCH_SELECTED:
            self = .searchSelected(a.search_selected.selected >= 0 ? Int(a.search_selected.selected) : nil)
        case GHOSTTY_ACTION_MOUSE_VISIBILITY:
            self = .mouseVisibility(visible: a.mouse_visibility != GHOSTTY_MOUSE_HIDDEN)
        case GHOSTTY_ACTION_KEY_SEQUENCE:
            self = .keySequence(a.key_sequence.active ? Self.describe(a.key_sequence.trigger) : nil)
        case GHOSTTY_ACTION_KEY_TABLE:
            switch a.key_table.tag {
            case GHOSTTY_KEY_TABLE_ACTIVATE:
                let v = a.key_table.value.activate
                let name = v.name.map { ptr in
                    String(decoding: UnsafeRawBufferPointer(start: ptr, count: v.len), as: UTF8.self)
                } ?? ""
                self = .keyTable(.activate(name))
            case GHOSTTY_KEY_TABLE_DEACTIVATE: self = .keyTable(.deactivate)
            default: self = .keyTable(.deactivateAll)
            }
        case GHOSTTY_ACTION_INSPECTOR: self = .inspector
        case GHOSTTY_ACTION_PRESENT_TERMINAL: self = .presentTerminal
        case GHOSTTY_ACTION_PROGRESS_REPORT:
            let r = a.progress_report
            self = .progressReport(TerminalProgressState(r.state) ?? .set, percent: r.progress < 0 ? nil : Int(min(r.progress, 100)))
        case GHOSTTY_ACTION_DESKTOP_NOTIFICATION:
            let n = a.desktop_notification
            self = .desktopNotification(title: n.title.map { String(cString: $0) } ?? "", body: n.body.map { String(cString: $0) } ?? "")
        case GHOSTTY_ACTION_COMMAND_FINISHED:
            let f = a.command_finished
            self = .commandFinished(exitCode: f.exit_code < 0 ? nil : Int(f.exit_code), durationNanos: f.duration)
        case GHOSTTY_ACTION_RING_BELL: self = .ringBell
        default: self = .other
        }
    }

    /// A trigger as the Mac writes a shortcut: "⌃⇧A", "⌘↩".
    static func describe(_ trigger: ghostty_input_trigger_s) -> String {
        let m = trigger.mods.rawValue
        var text = ""
        if m & GHOSTTY_MODS_CTRL.rawValue != 0 { text += "⌃" }
        if m & GHOSTTY_MODS_ALT.rawValue != 0 { text += "⌥" }
        if m & GHOSTTY_MODS_SHIFT.rawValue != 0 { text += "⇧" }
        if m & GHOSTTY_MODS_SUPER.rawValue != 0 { text += "⌘" }
        switch trigger.tag {
        case GHOSTTY_TRIGGER_UNICODE:
            text += Unicode.Scalar(trigger.key.unicode).map { String($0).uppercased() } ?? "?"
        case GHOSTTY_TRIGGER_PHYSICAL:
            text += TerminalKey(ghosttyKey: trigger.key.physical).map(symbol) ?? "?"
        default:
            text += "any key"
        }
        return text
    }

    private static func symbol(_ key: TerminalKey) -> String {
        switch key {
        case .enter: return "↩"
        case .tab: return "⇥"
        case .escape: return "⎋"
        case .space: return "Space"
        case .backspace: return "⌫"
        case .delete: return "⌦"
        case .arrowUp: return "↑"
        case .arrowDown: return "↓"
        case .arrowLeft: return "←"
        case .arrowRight: return "→"
        case .comma: return ","
        case .period: return "."
        case .slash: return "/"
        case .semicolon: return ";"
        case .quote: return "'"
        case .bracketLeft: return "["
        case .bracketRight: return "]"
        case .backslash: return "\\"
        case .backquote: return "`"
        case .minus: return "-"
        case .equal: return "="
        default:
            let name = "\(key)"
            if name.hasPrefix("digit") { return String(name.dropFirst(5)) }
            return name.count == 1 ? name.uppercased() : name.prefix(1).uppercased() + name.dropFirst()
        }
    }

    /// Ghostty's name for action `tag`.
    public static func name(of tag: ghostty_action_tag_e) -> String {
        switch tag {
        case GHOSTTY_ACTION_QUIT: "quit"
        case GHOSTTY_ACTION_NEW_WINDOW: "new_window"
        case GHOSTTY_ACTION_NEW_TAB: "new_tab"
        case GHOSTTY_ACTION_CLOSE_TAB: "close_tab"
        case GHOSTTY_ACTION_NEW_SPLIT: "new_split"
        case GHOSTTY_ACTION_CLOSE_ALL_WINDOWS: "close_all_windows"
        case GHOSTTY_ACTION_TOGGLE_MAXIMIZE: "toggle_maximize"
        case GHOSTTY_ACTION_TOGGLE_FULLSCREEN: "toggle_fullscreen"
        case GHOSTTY_ACTION_TOGGLE_TAB_OVERVIEW: "toggle_tab_overview"
        case GHOSTTY_ACTION_TOGGLE_WINDOW_DECORATIONS: "toggle_window_decorations"
        case GHOSTTY_ACTION_TOGGLE_QUICK_TERMINAL: "toggle_quick_terminal"
        case GHOSTTY_ACTION_TOGGLE_COMMAND_PALETTE: "toggle_command_palette"
        case GHOSTTY_ACTION_TOGGLE_VISIBILITY: "toggle_visibility"
        case GHOSTTY_ACTION_TOGGLE_BACKGROUND_OPACITY: "toggle_background_opacity"
        case GHOSTTY_ACTION_MOVE_TAB: "move_tab"
        case GHOSTTY_ACTION_GOTO_TAB: "goto_tab"
        case GHOSTTY_ACTION_GOTO_SPLIT: "goto_split"
        case GHOSTTY_ACTION_GOTO_WINDOW: "goto_window"
        case GHOSTTY_ACTION_RESIZE_SPLIT: "resize_split"
        case GHOSTTY_ACTION_EQUALIZE_SPLITS: "equalize_splits"
        case GHOSTTY_ACTION_TOGGLE_SPLIT_ZOOM: "toggle_split_zoom"
        case GHOSTTY_ACTION_PRESENT_TERMINAL: "present_terminal"
        case GHOSTTY_ACTION_SIZE_LIMIT: "size_limit"
        case GHOSTTY_ACTION_RESET_WINDOW_SIZE: "reset_window_size"
        case GHOSTTY_ACTION_INITIAL_SIZE: "initial_size"
        case GHOSTTY_ACTION_CELL_SIZE: "cell_size"
        case GHOSTTY_ACTION_SCROLLBAR: "scrollbar"
        case GHOSTTY_ACTION_RENDER: "render"
        case GHOSTTY_ACTION_INSPECTOR: "inspector"
        case GHOSTTY_ACTION_SHOW_GTK_INSPECTOR: "show_gtk_inspector"
        case GHOSTTY_ACTION_RENDER_INSPECTOR: "render_inspector"
        case GHOSTTY_ACTION_EXPORT_TERMINAL_IO: "export_terminal_io"
        case GHOSTTY_ACTION_DESKTOP_NOTIFICATION: "desktop_notification"
        case GHOSTTY_ACTION_SET_TITLE: "set_title"
        case GHOSTTY_ACTION_SET_TAB_TITLE: "set_tab_title"
        case GHOSTTY_ACTION_SET_WINDOW_TITLE: "set_window_title"
        case GHOSTTY_ACTION_PROMPT_TITLE: "prompt_title"
        case GHOSTTY_ACTION_PWD: "pwd"
        case GHOSTTY_ACTION_MOUSE_SHAPE: "mouse_shape"
        case GHOSTTY_ACTION_MOUSE_VISIBILITY: "mouse_visibility"
        case GHOSTTY_ACTION_MOUSE_OVER_LINK: "mouse_over_link"
        case GHOSTTY_ACTION_RENDERER_HEALTH: "renderer_health"
        case GHOSTTY_ACTION_OPEN_CONFIG: "open_config"
        case GHOSTTY_ACTION_QUIT_TIMER: "quit_timer"
        case GHOSTTY_ACTION_FLOAT_WINDOW: "float_window"
        case GHOSTTY_ACTION_SECURE_INPUT: "secure_input"
        case GHOSTTY_ACTION_KEY_SEQUENCE: "key_sequence"
        case GHOSTTY_ACTION_KEY_TABLE: "key_table"
        case GHOSTTY_ACTION_COLOR_CHANGE: "color_change"
        case GHOSTTY_ACTION_RELOAD_CONFIG: "reload_config"
        case GHOSTTY_ACTION_CONFIG_CHANGE: "config_change"
        case GHOSTTY_ACTION_CLOSE_WINDOW: "close_window"
        case GHOSTTY_ACTION_RING_BELL: "ring_bell"
        case GHOSTTY_ACTION_SELECTION_CHANGED: "selection_changed"
        case GHOSTTY_ACTION_UNDO: "undo"
        case GHOSTTY_ACTION_REDO: "redo"
        case GHOSTTY_ACTION_CHECK_FOR_UPDATES: "check_for_updates"
        case GHOSTTY_ACTION_OPEN_URL: "open_url"
        case GHOSTTY_ACTION_SHOW_CHILD_EXITED: "show_child_exited"
        case GHOSTTY_ACTION_PROGRESS_REPORT: "progress_report"
        case GHOSTTY_ACTION_SHOW_ON_SCREEN_KEYBOARD: "show_on_screen_keyboard"
        case GHOSTTY_ACTION_COMMAND_FINISHED: "command_finished"
        case GHOSTTY_ACTION_START_SEARCH: "start_search"
        case GHOSTTY_ACTION_END_SEARCH: "end_search"
        case GHOSTTY_ACTION_SEARCH_TOTAL: "search_total"
        case GHOSTTY_ACTION_SEARCH_SELECTED: "search_selected"
        case GHOSTTY_ACTION_READONLY: "readonly"
        case GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD: "copy_title_to_clipboard"
        case GHOSTTY_ACTION_MOVE_TAB_TO_NEW_WINDOW: "move_tab_to_new_window"
        default: "action_\(tag.rawValue)"
        }
    }
}

extension TerminalController {
    /// Hands `action` to the host's ``onAction``; true if the host performed it.
    func offerAction(_ action: ghostty_action_s, bridge: TerminalCallbackBridge?) -> Bool {
        guard let onAction else { return false }
        let delegate = bridge?.delegate
        let state = delegate as? TerminalViewState ?? (delegate as? any TerminalSurfaceStateDelegate)?.viewState
        // A surface whose host state is gone: nothing to say it's for.
        if bridge != nil, state == nil { return false }
        return onAction(TerminalActionEvent(
            raw: action,
            state: state,
            wrapperHandles: TerminalCallbackBridge.handles(action.tag)
        ))
    }

    /// An enum or string config value as Ghostty resolved it, defaults included: an enum by its
    /// case's name (`confirm-close-surface` → "true", "false" or "always"). Only for keys of those
    /// kinds: Ghostty writes a key's value in its own type, whatever the caller expects.
    public func configText(_ key: String) -> String? {
        guard let config else { return nil }
        var text: UnsafePointer<CChar>?
        guard ghostty_config_get(config, &text, key, UInt(key.utf8.count)), let text else { return nil }
        return String(cString: text)
    }

    /// Ghostty's soft `reload_config`: the config already loaded, given again to `state`'s surface
    /// (nil: the app, which passes it to every surface). Each surface applies it for its own light
    /// or dark, so a `light:…,dark:…` theme takes the matching half. Ghostty asks for this when a
    /// surface's or the app's color scheme changes, as its own app answers it.
    public func reapplyConfig(to state: TerminalViewState?) {
        guard let config else { return }
        if let state {
            guard let surface = state.surface?.rawValue else { return }
            ghostty_surface_update_config(surface, config)
        } else if let app {
            ghostty_app_update_config(app, config)
        }
    }

    /// A color config value as Ghostty resolved it (`background`), as 0–255 red, green, blue.
    public func configColor(_ key: String) -> (red: UInt8, green: UInt8, blue: UInt8)? {
        guard let config else { return nil }
        var color = ghostty_config_color_s()
        guard ghostty_config_get(config, &color, key, UInt(key.utf8.count)) else { return nil }
        return (color.r, color.g, color.b)
    }

    /// A packed set of flags as Ghostty resolved it (`bell-features`, `notify-on-command-finish-action`):
    /// bit n is the nth flag in the order Ghostty's docs list them.
    public func configBits(_ key: String) -> UInt32? {
        guard let config else { return nil }
        var bits: UInt32 = 0
        guard ghostty_config_get(config, &bits, key, UInt(key.utf8.count)) else { return nil }
        return bits
    }

    /// A duration as Ghostty resolved it (`undo-timeout`), in milliseconds, as its C API gives one.
    public func configMilliseconds(_ key: String) -> UInt64? {
        guard let config else { return nil }
        var ms: UInt = 0
        guard ghostty_config_get(config, &ms, key, UInt(key.utf8.count)) else { return nil }
        return UInt64(ms)
    }

    /// A floating-point config value (`quick-terminal-animation-duration`, `bell-audio-volume`).
    /// Ghostty writes an f64 for its f64 keys and an f32 for its f32 keys: `single` reads the latter.
    public func configNumber(_ key: String, single: Bool = false) -> Double? {
        guard let config else { return nil }
        if single {
            var v: Float = 0
            guard ghostty_config_get(config, &v, key, UInt(key.utf8.count)) else { return nil }
            return Double(v)
        }
        var v: Double = 0
        guard ghostty_config_get(config, &v, key, UInt(key.utf8.count)) else { return nil }
        return v
    }

    /// A path config value (`bell-audio-path`), nil when unset.
    public func configPath(_ key: String) -> String? {
        guard let config else { return nil }
        var v = ghostty_config_path_s()
        guard ghostty_config_get(config, &v, key, UInt(key.utf8.count)), let p = v.path else { return nil }
        let path = String(cString: p)
        return path.isEmpty ? nil : path
    }

    /// One side of `quick-terminal-size`.
    public enum QuickTerminalSize: Equatable, Sendable {
        case percent(Double), pixels(Double)
    }

    /// `quick-terminal-size`: its primary and secondary size, each nil when not given.
    public func configQuickTerminalSize() -> (primary: QuickTerminalSize?, secondary: QuickTerminalSize?) {
        guard let config else { return (nil, nil) }
        var v = ghostty_config_quick_terminal_size_s()
        let key = "quick-terminal-size"
        guard ghostty_config_get(config, &v, key, UInt(key.utf8.count)) else { return (nil, nil) }
        func size(_ s: ghostty_quick_terminal_size_s) -> QuickTerminalSize? {
            switch s.tag {
            case GHOSTTY_QUICK_TERMINAL_SIZE_PERCENTAGE: .percent(Double(s.value.percentage))
            case GHOSTTY_QUICK_TERMINAL_SIZE_PIXELS: .pixels(Double(s.value.pixels))
            default: nil
            }
        }
        return (size(v.primary), size(v.secondary))
    }

    /// The key bound to `action` (`toggle_quick_terminal`), as a Mac virtual key code and the
    /// modifiers (Carbon's `cmdKey`, `shiftKey`, `optionKey`, `controlKey` bits); nil when it isn't
    /// bound, or bound to a key with no Mac key code.
    public func configTrigger(_ action: String) -> (keyCode: UInt32, carbonModifiers: UInt32)? {
        guard let config else { return nil }
        let t = ghostty_config_trigger(config, action, UInt(action.utf8.count))
        let key: ghostty_input_key_e
        switch t.tag {
        case GHOSTTY_TRIGGER_PHYSICAL:
            key = t.key.physical
        case GHOSTTY_TRIGGER_UNICODE:
            guard let k = Unicode.Scalar(t.key.unicode).flatMap({ Self.physicalKey(for: Character($0)) }) else { return nil }
            key = k
        default:
            return nil
        }
        guard key != GHOSTTY_KEY_UNIDENTIFIED else { return nil }
        let code = TerminalHardwareKeyRouter.appKitKeyCode(for: key)
        guard code != TerminalHardwareKeyRouter.unidentifiedAppKitKeyCode else { return nil }
        let m = t.mods.rawValue
        var carbon: UInt32 = 0
        // Carbon's modifier bits (Events.h): cmdKey 1<<8, shiftKey 1<<9, optionKey 1<<11, controlKey 1<<12.
        if m & GHOSTTY_MODS_SUPER.rawValue != 0 { carbon |= 1 << 8 }
        if m & GHOSTTY_MODS_SHIFT.rawValue != 0 { carbon |= 1 << 9 }
        if m & GHOSTTY_MODS_ALT.rawValue != 0 { carbon |= 1 << 11 }
        if m & GHOSTTY_MODS_CTRL.rawValue != 0 { carbon |= 1 << 12 }
        return (code, carbon)
    }

    /// The key a character is on, on a US layout, as Ghostty reads a trigger like `cmd+a`.
    private static func physicalKey(for c: Character) -> ghostty_input_key_e? {
        let lower = Character(c.lowercased())
        if let a = lower.asciiValue, a >= 97, a <= 122 {
            return ghostty_input_key_e(rawValue: GHOSTTY_KEY_A.rawValue + UInt32(a - 97))
        }
        if let d = lower.asciiValue, d >= 48, d <= 57 {
            return ghostty_input_key_e(rawValue: GHOSTTY_KEY_DIGIT_0.rawValue + UInt32(d - 48))
        }
        let keys: [Character: ghostty_input_key_e] = [
            "`": GHOSTTY_KEY_BACKQUOTE, "-": GHOSTTY_KEY_MINUS, "=": GHOSTTY_KEY_EQUAL, "[": GHOSTTY_KEY_BRACKET_LEFT,
            "]": GHOSTTY_KEY_BRACKET_RIGHT, "\\": GHOSTTY_KEY_BACKSLASH, ";": GHOSTTY_KEY_SEMICOLON, "'": GHOSTTY_KEY_QUOTE,
            ",": GHOSTTY_KEY_COMMA, ".": GHOSTTY_KEY_PERIOD, "/": GHOSTTY_KEY_SLASH, " ": GHOSTTY_KEY_SPACE,
        ]
        return keys[lower]
    }

    /// A bool config value as Ghostty resolved it, defaults included. Only for bool keys.
    public func configFlag(_ key: String) -> Bool? {
        guard let config else { return nil }
        var flag = false
        guard ghostty_config_get(config, &flag, key, UInt(key.utf8.count)) else { return nil }
        return flag
    }
}
