import SwiftUI

/// Settings' pages, in sidebar order, as System Settings lays them out: many small pages in a few
/// groups, so none is long. The raw values are stable: other places open Settings at one, and
/// Settings reopens on the one you left.
enum SettingsPane: String, CaseIterable, Identifiable {
    case account
    case general, appearance, notifications, updates
    case shell, ai, quick, tmux
    case agents, defaults, limits, claude
    case providers, models, keys
    case git, worktrees, repos, ssh
    case computer, power, permissions, experimental, managed

    var id: String { rawValue }

    /// A page remembered from before Settings was split into small pages, or one another place
    /// names the old way.
    init?(rawValue: String) {
        let old: [String: SettingsPane] = [
            "policies": .agents, "terminal": .shell, "workspaces": .worktrees, "storage": .worktrees,
        ]
        if let p = old[rawValue] {
            self = p
            return
        }
        guard let pane = Self.allCases.first(where: { $0.rawValue == rawValue }) else { return nil }
        self = pane
    }

    /// The sidebar's groups, under the account row; what the organization manages only when it does.
    static func groups(managed: Bool) -> [SettingsGroup] {
        SettingsGroup.all.map { g in
            SettingsGroup(title: g.title, panes: g.panes.filter { $0 != .managed || managed })
        }
    }

    var title: String {
        switch self {
        case .account: "Dino Account"
        case .general: "General"
        case .appearance: "Appearance"
        case .notifications: "Notifications"
        case .updates: "Updates"
        case .shell: "Shell"
        case .ai: "AI in the Shell"
        case .quick: "Quick Terminal"
        case .tmux: "tmux"
        case .agents: "Agents"
        case .defaults: "New Sessions"
        case .limits: "Limits & Fallbacks"
        case .claude: "Accounts"
        case .computer: "Computer Use"
        case .providers: "Providers"
        case .models: "Models"
        case .keys: "API Keys"
        case .git: "Git"
        case .worktrees: "Worktrees"
        case .repos: "Environments"
        case .ssh: "SSH Hosts"
        case .power: "Power"
        case .permissions: "Permissions"
        case .experimental: "Experimental"
        case .managed: "Managed by Your Organization"
        }
    }

    /// Outline symbols, drawn in the sidebar's secondary ink as the Codex app's are.
    var icon: String {
        switch self {
        case .account: "person.crop.circle"
        case .general: "gearshape"
        case .appearance: "circle.lefthalf.filled"
        case .notifications: "bell"
        case .updates: "arrow.down.circle"
        case .shell: "terminal"
        case .ai: "sparkles"
        case .quick: "rectangle.topthird.inset.filled"
        case .tmux: "rectangle.split.3x1"
        case .agents: "cpu"
        case .defaults: "slider.horizontal.3"
        case .limits: "gauge.with.dots.needle.67percent"
        case .claude: "person.2"
        case .computer: "cursorarrow.motionlines"
        case .providers: "point.3.connected.trianglepath.dotted"
        case .models: "cube"
        case .keys: "key"
        case .git: "arrow.triangle.pull"
        case .worktrees: "arrow.triangle.branch"
        case .repos: "list.bullet.rectangle"
        case .ssh: "server.rack"
        case .power: "bolt"
        case .permissions: "hand.raised"
        case .experimental: "flask"
        case .managed: "building.2"
        }
    }

    /// What the page is for, under its title.
    var summary: String {
        switch self {
        case .account: "Sync your settings across your Macs."
        case .general: "How dino starts, quits and closes, and where its files are."
        case .appearance: "How dino and its terminals look."
        case .notifications: "When dino tells you an agent needs you."
        case .updates: "Keeping dino and its background service up to date."
        case .shell: "What dino's shells tell it, and agents you start in them."
        case .ai: "Plain English at a shell prompt: ⌘I for a command, ⌘⏎ for an agent."
        case .quick: "A terminal that drops down from the top of the screen in any app."
        case .tmux: "Your agents as tmux windows, and new tabs in tmux."
        case .agents: "Every agent dino knows, which ones you use, and the one ⌘N starts."
        case .defaults: "What each agent's new sessions start with."
        case .limits: "How much a session may use, and where an agent goes at its limit."
        case .claude: "Your Claude accounts. When one reaches its usage limit, Claude Code goes on with the next."
        case .computer: "Agents using your Mac's apps, and how dino shows it."
        case .providers: "Where models come from besides the agents' own accounts."
        case .models: "Every model your providers serve, and the agents it works in."
        case .keys: "Provider keys, kept on this Mac only."
        case .git: "Branch names, and what happens when a pull request closes."
        case .worktrees: "Where dino's worktrees go, trust, and the ones on disk."
        case .repos: "Variables for each repo's sessions, and a build cache shared by worktrees."
        case .ssh: "Machines to start sessions on over SSH."
        case .power: "Keeping your Mac awake for agents and automations."
        case .permissions: "macOS permissions for programs in dino's terminals."
        case .experimental: "Features still being tested. Each is off until you turn it on."
        case .managed: "Settings your organization sets for you."
        }
    }

    /// Open Settings at this page next time it shows.
    func select() {
        UserDefaults.standard.set(rawValue, forKey: SettingsPane.storageKey)
    }

    static let storageKey = "settingsTab"
}

struct SettingsGroup: Hashable {
    let title: String
    let panes: [SettingsPane]

    /// The app's own pages first, untitled, as Claude's and Codex's settings start; "This Mac" last,
    /// for what's set per machine, as Claude's "This computer".
    static let all: [SettingsGroup] = [
        SettingsGroup(title: "", panes: [.general, .appearance, .notifications, .updates]),
        SettingsGroup(title: "Terminal", panes: [.shell, .ai, .quick, .tmux]),
        SettingsGroup(title: "Agents", panes: [.agents, .defaults, .limits, .claude]),
        SettingsGroup(title: "Models", panes: [.providers, .models, .keys]),
        SettingsGroup(title: "Code", panes: [.git, .worktrees, .repos, .ssh]),
        SettingsGroup(title: "This Mac", panes: [.computer, .power, .permissions, .experimental, .managed]),
    ]
}

/// One setting as search finds it: where it is, what it's called, what it does, other words for
/// it, and the settings.toml keys (or the app's own defaults) it sets.
struct SettingsEntry: Identifiable, Hashable {
    /// Also the anchor of its control on the page (`settingAnchor`).
    let id: String
    let pane: SettingsPane
    let title: String
    var detail = ""
    var synonyms: [String] = []
    /// settings.toml keys, `*` for a map's keys ("agents.*.model"); "app:" for the app's own.
    var keys: [String] = []

    fileprivate var haystack: SearchText { SearchText(self) }
}

/// An entry's text, folded once for searching.
private struct SearchText {
    let title: [String]
    let synonyms: [String]
    let page: [String]
    let detail: String

    init(_ e: SettingsEntry) {
        title = SettingsSearch.words(e.title)
        synonyms = e.synonyms.flatMap(SettingsSearch.words)
        page = SettingsSearch.words(e.pane.title)
        detail = SettingsSearch.fold(e.detail)
    }
}

enum SettingsSearch {
    /// Lowercased, without accents, ⌘ as "cmd".
    static func fold(_ s: String) -> String {
        s.replacingOccurrences(of: "⌘", with: " cmd ")
            .replacingOccurrences(of: "⏎", with: " return ")
            .folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
    }

    static func words(_ s: String) -> [String] {
        fold(s).split { !$0.isLetter && !$0.isNumber }.map(String.init)
    }

    private static let texts: [(SettingsEntry, SearchText)] = SettingsEntry.all.map { ($0, $0.haystack) }

    /// Every entry each word of `query` finds, best first: in its title, then its other names, its
    /// page, and what it does.
    static func results(_ query: String, managed: Bool = false) -> [SettingsEntry] {
        let q = words(query)
        guard !q.isEmpty else { return [] }
        var scored: [(SettingsEntry, Int, Int)] = []
        for (i, (e, t)) in texts.enumerated() where managed || e.pane != .managed {
            var total = 0
            for w in q {
                var s = 0
                if t.title.first?.hasPrefix(w) == true { s = 12 }
                else if t.title.contains(where: { $0.hasPrefix(w) }) { s = 8 }
                else if t.synonyms.contains(where: { $0.hasPrefix(w) }) { s = 5 }
                else if t.page.contains(where: { $0.hasPrefix(w) }) { s = 3 }
                else if w.count >= 3, t.detail.contains(w) { s = 1 }
                if s == 0 { total = 0; break }
                total += s
            }
            if total > 0 { scored.append((e, total, i)) }
        }
        return scored.sorted { $0.1 != $1.1 ? $0.1 > $1.1 : $0.2 < $1.2 }.map(\.0)
    }
}

extension SettingsEntry {
    /// Every setting, in page order. `SettingsIndexTests` checks every settings.toml key has one.
    static let all: [SettingsEntry] = [
        // Dino Account
        .init(id: "account", pane: .account, title: "Dino Account", detail: "Sign in to sync your settings across your Macs.",
              synonyms: ["sync", "sign in", "login", "github", "email", "cloud", "devices"]),

        // General
        .init(id: "start-with", pane: .general, title: "When dino opens", detail: "Your last session, or a new shell.",
              synonyms: ["startup", "launch", "restore", "open"], keys: ["terminal.start_with"]),
        .init(id: "on-quit", pane: .general, title: "When you quit with agents running", detail: "Ask, keep them running, or stop them.",
              synonyms: ["quit", "exit", "close", "stop agents", "keep running"], keys: ["terminal.on_quit"]),
        .init(id: "ask-close", pane: .general, title: "Ask before ⌘W closes an agent", detail: "Closing an agent's tab or pane stops it and archives the session.",
              synonyms: ["close", "confirm", "tab", "pane", "cmd w"], keys: ["app:askBeforeClosingAgents"]),
        .init(id: "default-terminal", pane: .general, title: "Default terminal", detail: "Open scripts, programs and man pages from the Finder and other apps in dino.",
              synonyms: ["default", "handler", "command file", "finder", "open with"]),
        .init(id: "settings-folder", pane: .general, title: "Settings and keys folder", detail: "Where settings.toml and your keys are kept.",
              synonyms: ["settings.toml", "config", "folder", "dino home", "files"]),

        // Appearance
        .init(id: "appearance", pane: .appearance, title: "Appearance", detail: "Light, dark, or as your Mac is.",
              synonyms: ["theme", "dark mode", "light mode", "color scheme", "look"], keys: ["terminal.appearance"]),
        .init(id: "ghostty-config", pane: .appearance, title: "Ghostty config", detail: "Terminals use the font, colors, cursor and key bindings from your Ghostty config.",
              synonyms: ["font", "colors", "colours", "cursor", "keybindings", "theme", "palette", "ghostty"]),

        // Notifications
        .init(id: "notify-needs-you", pane: .notifications, title: "Notify me when an agent needs me", detail: "Names the session and what it asks; click to go there.",
              synonyms: ["notifications", "alerts", "banner", "needs you", "permission prompt"], keys: ["app:\(Notifier.needsYouKey)"]),

        // Updates
        .init(id: "check-updates", pane: .updates, title: "Check for updates automatically", detail: "dino checks once a day.",
              synonyms: ["update", "sparkle", "version", "upgrade", "release"], keys: ["machine.check_updates"]),
        .init(id: "check-now", pane: .updates, title: "Check now", detail: "Look for a new version of dino.", synonyms: ["update", "version", "about"]),
        .init(id: "restart-service", pane: .updates, title: "Restart the background service", detail: "When dinod is still on an older version.",
              synonyms: ["dinod", "daemon", "restart", "background service"]),

        // Shell
        .init(id: "shell-integration", pane: .shell, title: "Shell integration", detail: "Lets dino see your prompts and current folder in zsh, bash, fish, elvish and nushell.",
              synonyms: ["prompt", "zsh", "bash", "fish", "osc", "cwd", "working directory", "jump to prompt"],
              keys: ["machine.shell_integration", "machine.shell_integration_mode", "machine.shell_features"]),
        .init(id: "shell-agents", pane: .shell, title: "Show agents started in a shell in the sidebar",
              detail: "An agent you run in a dino shell becomes a session like any dino starts.",
              synonyms: ["claude in shell", "codex", "typed agent", "detect", "sidebar"], keys: ["machine.shell_agents"]),
        .init(id: "tmux-zsh", pane: .shell, title: "zsh in tmux panes", detail: "A line for your .zshrc so zsh in tmux panes tells dino exit codes and notifications.",
              synonyms: ["zshrc", "tmux", "passthrough"]),

        // AI in the shell
        .init(id: "ask-agent", pane: .ai, title: "⌘I asks", detail: "Which agent turns plain English into a command at a shell prompt.",
              synonyms: ["ai", "natural language", "command", "suggest", "cmd i", "alt i", "dino ai"], keys: ["terminal.ask_agent"]),
        .init(id: "ask-model", pane: .ai, title: "⌘I model", detail: "The model the ⌘I agent uses. A small, fast model answers sooner.",
              synonyms: ["ai", "model", "fast"], keys: ["terminal.ask_model"]),
        .init(id: "handoff-agent", pane: .ai, title: "⌘⏎ sends to", detail: "Which agent ⌘⏎ hands the line to, as a new session.",
              synonyms: ["handoff", "send to agent", "cmd return", "cmd enter", "alt enter"], keys: ["terminal.handoff_agent"]),

        // Quick terminal
        .init(id: "quick-key", pane: .quick, title: "Quick terminal shortcut", detail: "Drops down a terminal from the top of the screen in any app.",
              synonyms: ["hotkey", "global shortcut", "dropdown", "visor", "quake", "cmd `"], keys: ["terminal.quick_key"]),
        .init(id: "quick-autohide", pane: .quick, title: "Hide it when you click elsewhere", detail: "The quick terminal hides when it loses focus.",
              synonyms: ["autohide", "auto hide", "focus"], keys: ["terminal.quick_autohide"]),

        // tmux
        .init(id: "tmux-show", pane: .tmux, title: "Show agents in tmux", detail: "Each agent appears as a tmux window running dino attach.",
              synonyms: ["tmux", "window", "attach", "terminal multiplexer"], keys: ["tmux.show_agents"]),
        .init(id: "tmux-session", pane: .tmux, title: "tmux session for agents", detail: "Their own tmux session, or the one you're attached to.",
              synonyms: ["tmux", "session name"], keys: ["tmux.session"]),
        .init(id: "tmux-new-tabs", pane: .tmux, title: "New tabs open in tmux", detail: "New tabs attach to the tmux session you name.",
              synonyms: ["tmux", "tabs", "attach"], keys: ["tmux.new_tabs"]),

        // Agents
        .init(id: "agents-installed", pane: .agents, title: "Agents on this Mac", detail: "Install and sign in to Claude Code, Codex and other agents.",
              synonyms: ["install", "sign in", "login", "claude", "codex", "copilot", "cursor", "amp", "opencode", "pi", "version"]),
        .init(id: "default-agent", pane: .agents, title: "⌘N starts", detail: "The agent a new session starts with.",
              synonyms: ["default agent", "new session", "cmd n"], keys: ["policies.default_agent"]),
        .init(id: "allowed-agents", pane: .agents, title: "Agents you use", detail: "Agents you turn off are hidden from menus and can't be started.",
              synonyms: ["allowed", "enable", "disable", "hide agent"], keys: ["policies.allowed_agents"]),

        // New sessions
        .init(id: "allow-bypass", pane: .defaults, title: "Allow bypass permissions mode", detail: "An agent edits files and runs any command without asking you.",
              synonyms: ["yolo", "dangerously skip permissions", "auto approve", "permission mode", "safety"], keys: ["policies.allow_bypass"]),
        .init(id: "agent-defaults", pane: .defaults, title: "Default mode, model and effort", detail: "What new sessions of each agent start with.",
              synonyms: ["model", "permission mode", "plan mode", "effort", "reasoning", "thinking", "opus", "sonnet", "haiku", "gpt"],
              keys: ["agents.*.mode", "agents.*.model", "agents.*.effort"]),

        // Limits & fallbacks
        .init(id: "token-budget", pane: .limits, title: "Tokens per session", detail: "Once a session goes over the limit, its next request fails.",
              synonyms: ["budget", "limit", "cap", "spend", "cost"], keys: ["policies.session_token_budget"]),
        .init(id: "fallbacks", pane: .limits, title: "When an agent hits a limit", detail: "Where an agent's requests go when its provider hits a usage limit, in order.",
              synonyms: ["fallback", "rate limit", "usage limit", "outage", "failover", "quota"],
              keys: ["fallbacks.*.steps", "fallbacks.*.on_outage", "fallbacks.*.new_sessions.agent", "fallbacks.*.new_sessions.model", "policies.fallback_providers"]),

        // Accounts
        .init(id: "claude-accounts", pane: .claude, title: "Claude accounts", detail: "Sign in with more Claude accounts and name them. Claude Code switches to the next when one reaches its usage limit.",
              synonyms: ["accounts", "add account", "sign in", "log in", "login", "subscription", "max", "pro", "switch account", "limit", "claude code", "rename", "account name", "email"]),
        .init(id: "claude-token", pane: .claude, title: "Claude Code on SSH hosts", detail: "Claude Code on SSH hosts signs in with your first Claude account.",
              synonyms: ["setup-token", "oauth", "ssh", "token", "subscription token"], keys: ["machine.claude_token.ssh", "machine.claude_token.local"]),

        // Computer use
        .init(id: "computer-use", pane: .computer, title: ComputerUseCopy.title, detail: ComputerUseCopy.summary,
              synonyms: ["computer use", "gui", "click", "screen", "open-computer-use", "browser", "automation"],
              keys: ["machine.computer_use", "experimental.computer_use"]),
        .init(id: "computer-use-display", pane: .computer, title: "Show when an agent uses your Mac", detail: "A banner over its terminal, a sidebar mark, or nothing.",
              synonyms: ["banner", "indicator", "notification"], keys: ["app:\(UsingDisplay.key)"]),

        // Providers
        .init(id: "routing", pane: .providers, title: "Route agent traffic through dino", detail: "Lets dino count tokens, apply limits and fallbacks, and connect agents to providers.",
              synonyms: ["proxy", "routing", "traffic", "usage", "count tokens"], keys: ["routing.proxy"]),
        .init(id: "providers-list", pane: .providers, title: "Providers", detail: "OpenRouter, ChatGPT, and model servers running on this Mac.",
              synonyms: ["openrouter", "chatgpt", "ollama", "lm studio", "local models", "connect"]),
        .init(id: "coding-plans", pane: .providers, title: "Coding plans", detail: "Add a coding plan's key so your agents can use the plan.",
              synonyms: ["plan", "zai", "glm", "kimi", "minimax", "subscription"]),

        // Models
        .init(id: "model-browser", pane: .models, title: "Models", detail: "Every model your providers serve, with the agents it works in.",
              synonyms: ["model list", "free models", "local", "context", "price", "run in"]),

        // API keys
        .init(id: "api-keys", pane: .keys, title: "API keys", detail: "Keys are stored on this Mac, readable only by you.",
              synonyms: ["key", "token", "secret", "anthropic", "openai", "nvidia", "typesafe", "credentials"]),

        // Worktrees
        .init(id: "worktree-location", pane: .worktrees, title: "Worktree location", detail: "Where dino creates worktrees.",
              synonyms: ["folder", "path", "git worktree"], keys: ["worktrees.location"]),
        .init(id: "branch-prefix", pane: .git, title: "Branch prefix", detail: "What dino's branches are named.",
              synonyms: ["branch", "git", "name"], keys: ["worktrees.branch_prefix"]),
        .init(id: "worktree-trust", pane: .worktrees, title: "Trust worktrees when the repo is trusted", detail: "Claude Code takes a worktree's trust from its repo.",
              synonyms: ["trust", "folder trust", "prompt"], keys: ["policies.worktree_trust"]),
        .init(id: "close-merged", pane: .git, title: "Archive sessions after their PR merges or closes", detail: "And remove the worktree when nothing in it would be lost.",
              synonyms: ["pull request", "pr", "merged", "archive", "cleanup"], keys: ["policies.close_merged"]),

        .init(id: "build-cache", pane: .repos, title: "Share one build cache across worktrees", detail: "For Rust projects, using sccache.",
              synonyms: ["sccache", "rust", "cargo", "compile", "cache"], keys: ["machine.build_cache.enabled"]),
        .init(id: "build-cache-size", pane: .repos, title: "Build cache size", detail: "How much the shared build cache keeps.",
              synonyms: ["sccache", "gb", "disk"], keys: ["machine.build_cache.size_gb"]),
        .init(id: "worktree-storage", pane: .worktrees, title: "Worktrees on disk", detail: "The worktrees dino made, their sizes, and Free Up Space.",
              synonyms: ["disk", "space", "free up", "clean", "delete worktree", "remove"]),

        .init(id: "repo-env", pane: .repos, title: "Environment variables", detail: "Variables set for every session in a repo and its worktrees.",
              synonyms: ["env", "environment", "variables", "export", "repository", "repo"], keys: ["repos.*.env.*"]),

        // SSH hosts
        .init(id: "ssh-hosts", pane: .ssh, title: "SSH hosts", detail: "Machines to start sessions on over ssh, and their default folder.",
              synonyms: ["remote", "server", "ssh", "devbox", "host"], keys: ["ssh.*.folder"]),

        // Power
        .init(id: "awake-working", pane: .power, title: "Keep your Mac awake while agents work", detail: "Your Mac won't go to sleep on its own while any agent is working.",
              synonyms: ["sleep", "caffeinate", "awake", "energy", "battery"], keys: ["machine.awake_while_working"]),
        .init(id: "awake-scheduled", pane: .power, title: "Keep your Mac awake while automations are scheduled", detail: "So scheduled automations run on time.",
              synonyms: ["sleep", "cron", "schedule", "automation"], keys: ["machine.keep_awake"]),
        .init(id: "lid", pane: .power, title: "Keep agents running with the lid closed", detail: "While an agent is working or a session is open, on battery or not, for up to some hours.",
              synonyms: ["clamshell", "lid", "sleep", "battery", "pmset"],
              keys: ["machine.lid.enabled", "machine.lid.when", "machine.lid.on_battery", "machine.lid.min_battery", "machine.lid.max_hours"]),

        // Permissions
        .init(id: "permissions", pane: .permissions, title: "macOS permissions", detail: "Screen Recording, Accessibility and Full Disk Access for programs in dino's terminals.",
              synonyms: ["privacy", "screen recording", "accessibility", "full disk access", "screencapture", "tcc"]),
        .init(id: "background-service", pane: .permissions, title: "Running in the background", detail: "Login Items, so programs in your terminals get dino's permissions.",
              synonyms: ["login items", "launch agent", "dinod", "daemon"]),

        // Experimental
        .init(id: "free-models", pane: .experimental, title: "Free models pool", detail: "Run agents on free NVIDIA models, with dino choosing a model for each turn.",
              synonyms: ["free", "nvidia", "nim", "typesafe"], keys: ["experimental.free_models"]),
        .init(id: "session-tools", pane: .experimental, title: "Cross-session communication", detail: "Lets Claude sessions list, read and message your other dino sessions.",
              synonyms: ["mcp", "session tools", "message", "multi agent"], keys: ["policies.session_tools"]),

        // Managed
        .init(id: "managed", pane: .managed, title: "Managed by your organization", detail: "Every setting your organization sets.",
              synonyms: ["organization", "policy", "mdm", "managed", "locked"]),
    ]

    /// settings.toml keys no control sets: dino keeps them itself.
    static let notControls: [String: String] = [
        "machine.onboarded": "Set when you finish Welcome; Help → Show Welcome brings it back.",
    ]
}

/// Where search sent you: the control to scroll to and light up.
private struct SettingsHighlightKey: EnvironmentKey {
    static let defaultValue: String? = nil
}

extension EnvironmentValues {
    var settingsHighlight: String? {
        get { self[SettingsHighlightKey.self] }
        set { self[SettingsHighlightKey.self] = newValue }
    }
}

/// A control search can find: scrolled to and lit up when a result names it.
private struct SettingAnchor: ViewModifier {
    let id: String
    @Environment(\.settingsHighlight) private var highlight

    func body(content: Content) -> some View {
        content
            .id(id)
            .background {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.accentColor.opacity(highlight == id ? 0.16 : 0))
                    .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Color.accentColor.opacity(highlight == id ? 0.7 : 0), lineWidth: 1.5))
                    .padding(.horizontal, -6)
                    .padding(.vertical, -4)
                    .animation(.easeOut(duration: 0.35), value: highlight == id)
                    .allowsHitTesting(false)
            }
    }
}

extension View {
    /// Marks the control `SettingsEntry` `id` names.
    func settingAnchor(_ id: String) -> some View { modifier(SettingAnchor(id: id)) }
}
