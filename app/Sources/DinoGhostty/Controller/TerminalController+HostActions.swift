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
    case inspector
    case presentTerminal
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
        case GHOSTTY_ACTION_INSPECTOR: self = .inspector
        case GHOSTTY_ACTION_PRESENT_TERMINAL: self = .presentTerminal
        default: self = .other
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

    /// A bool config value as Ghostty resolved it, defaults included. Only for bool keys.
    public func configFlag(_ key: String) -> Bool? {
        guard let config else { return nil }
        var flag = false
        guard ghostty_config_get(config, &flag, key, UInt(key.utf8.count)) else { return nil }
        return flag
    }
}
