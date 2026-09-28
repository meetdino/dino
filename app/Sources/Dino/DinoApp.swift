import AppKit
import GhosttyTerminal
import SwiftUI

@main
struct DinoApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @StateObject private var model = DinoModel()

    var body: some Scene {
        WindowGroup("dino") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 820, minHeight: 480)
                .onAppear { model.start() }
        }
        .windowStyle(.hiddenTitleBar)
        .commands {
            CommandMenu("Session") {
                Menu("New Session") {
                    ForEach(model.launchers) { l in
                        Button(l.label) { model.newSession(l) }
                    }
                }
                Button("New Claude Code Session") {
                    if let l = model.launchers.first(where: { $0.short == "claude" }) ?? model.launchers.first { model.newSession(l) }
                }
                .keyboardShortcut("n")
                Button("Choose Folder…") { model.chooseFolder() }
                    .keyboardShortcut("o")
                Divider()
                Button("Jump to Session Needing You") { model.jumpToAttention() }
                    .keyboardShortcut("j")
                ForEach(Array(model.sessions.prefix(9).enumerated()), id: \.element.id) { i, s in
                    Button("\(i + 1)  \(s.name)") { model.select(s.id) }
                        .keyboardShortcut(KeyEquivalent(Character("\(i + 1)")))
                }
                Divider()
                Button("Kill Session") { if let id = model.selected { model.kill(id) } }
                    .keyboardShortcut(.delete, modifiers: [.command, .shift])
                    .disabled(model.selected == nil)
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_: Notification) {
        // Run as a regular app with a Dock icon and menu bar even when launched from a binary.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool { true }
}

struct ContentView: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 230, ideal: 260, max: 340)
        } detail: {
            Terminals()
        }
    }
}

// MARK: - Terminals

struct Terminals: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        ZStack {
            Color(nsColor: .textBackgroundColor).ignoresSafeArea()
            if model.sessions.isEmpty || model.daemonDown {
                EmptyState()
            }
            // Every session stays mounted; only the selected one draws.
            ForEach(model.sessions) { s in
                let visible = s.id == model.selected
                let state = model.terminal(for: s.id)
                TerminalPane(state: state, visible: visible)
                    // A new state (after reconnecting) must mean a new surface.
                    .id(ObjectIdentifier(state))
                    .opacity(visible ? 1 : 0)
                    .allowsHitTesting(visible)
            }
        }
        .toolbar {
            ToolbarItem(placement: .principal) {
                if let s = model.sessions.first(where: { $0.id == model.selected }) {
                    HStack(spacing: 8) {
                        Text(s.name).font(.system(.body, design: .monospaced).weight(.semibold)).foregroundStyle(Brand.green)
                        if let t = s.title { Text(t).foregroundStyle(.secondary).lineLimit(1) }
                    }
                }
            }
            ToolbarItem(placement: .primaryAction) { NewSessionMenu() }
        }
    }
}

struct TerminalPane: View {
    @ObservedObject var state: TerminalViewState
    let visible: Bool

    var body: some View {
        TerminalSurfaceView(context: state)
            .onAppear { state.isSurfaceVisible = visible }
            .onChange(of: visible) { _, v in
                state.isSurfaceVisible = v
                if v { state.requestFocus() }
            }
    }
}

struct NewSessionMenu: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        Menu {
            ForEach(model.launchers) { l in
                Button(l.label) { model.newSession(l) }
            }
            Divider()
            Button("In \(model.folder.lastPathComponent)…") { model.chooseFolder() }
        } label: {
            Label("New Session", systemImage: "plus")
        }
        .help("New session in \(model.folder.path)")
    }
}

struct EmptyState: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(spacing: 18) {
            DinoMark(size: 34)
            Text("One place for every agent on this machine").foregroundStyle(.secondary)
            if let error = model.error {
                Text(error).foregroundStyle(SessionStatus.exited.color).font(.callout)
            }
            if model.daemonDown {
                Text("dinod isn't running. Your sessions are saved and resume when it starts.")
                    .foregroundStyle(.secondary).font(.callout)
                Button("Start dinod") { model.startDaemon() }.controlSize(.large)
            }
            VStack(spacing: 8) {
                ForEach(model.daemonDown ? [] : model.launchers) { l in
                    Button { model.newSession(l) } label: {
                        Text(l.label).frame(width: 240)
                    }
                    .controlSize(.large)
                }
            }
            Button("Start in \(model.folder.path)") { model.chooseFolder() }
                .buttonStyle(.link)
                .font(.callout)
        }
        .padding(40)
    }
}

struct DinoMark: View {
    let size: CGFloat

    var body: some View {
        HStack(spacing: 6) {
            Text("▲▲").foregroundStyle(Brand.spike)
            Text("dino").foregroundStyle(Brand.green)
        }
        .font(.system(size: size, weight: .bold, design: .monospaced))
    }
}

// MARK: - Sidebar

struct Sidebar: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(spacing: 0) {
            List(selection: Binding(get: { model.selected }, set: { model.select($0) })) {
                Section("Agents") {
                    ForEach(Array(model.sessions.enumerated()), id: \.element.id) { i, s in
                        SessionRow(session: s, index: i + 1)
                            .tag(s.id)
                            .contextMenu {
                                Button("Kill Session", role: .destructive) { model.kill(s.id) }
                            }
                    }
                }
            }
            .listStyle(.sidebar)
            UsagePanel()
        }
        .safeAreaInset(edge: .top) {
            HStack {
                DinoMark(size: 15)
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 6)
        }
    }
}

struct SessionRow: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let index: Int

    var body: some View {
        let status = model.status(of: session)
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                StatusDot(status: status)
                Text(session.name).font(.system(.body, design: .monospaced).weight(.medium))
                Spacer()
                Text(status.label).font(.caption).foregroundStyle(status.color)
            }
            if let needs = session.needs {
                Label(needs, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(SessionStatus.needsYou.color).lineLimit(1)
            }
            if session.requests > 0 {
                HStack(spacing: 6) {
                    if let tier = session.tier {
                        Text("\(tier) → \(shortModel(session.last_model))").foregroundStyle(SessionStatus.done.color)
                    } else {
                        Text(shortModel(session.last_model))
                    }
                    Spacer()
                    Text("↑\(tokens(session.input_tokens)) ↓\(tokens(session.output_tokens))")
                }
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }
        }
        .padding(.vertical, 3)
    }
}

struct StatusDot: View {
    let status: SessionStatus
    @State private var pulse = false

    var body: some View {
        ZStack {
            switch status {
            case .needsYou:
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(status.color)
            case .done:
                Image(systemName: "checkmark.circle.fill").foregroundStyle(status.color)
            case .exited:
                Image(systemName: "xmark.circle").foregroundStyle(status.color)
            case .idle:
                Circle().strokeBorder(.secondary, lineWidth: 1.5).frame(width: 10, height: 10)
            case .thinking, .working:
                Circle().fill(status.color).frame(width: 10, height: 10)
                    .opacity(pulse ? 0.35 : 1)
                    .animation(.easeInOut(duration: 0.7).repeatForever(), value: pulse)
                    .onAppear { pulse = true }
            }
        }
        .frame(width: 14, height: 14)
    }
}

struct UsagePanel: View {
    @EnvironmentObject var model: DinoModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("USAGE").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            let labels = ["anthropic": "Claude", "chatgpt": "Codex"]
            ForEach(model.quotas, id: \.provider) { q in
                ForEach(q.windows.filter { $0.name.hasSuffix("h") || $0.name.hasSuffix("d") }, id: \.name) { w in
                    QuotaBar(label: "\(labels[q.provider] ?? q.provider) \(w.name)", window: w)
                }
            }
            if model.quotas.isEmpty {
                Text("No quota data yet").font(.caption).foregroundStyle(.tertiary)
            }
            let free = model.sessions.filter { $0.tier != nil }.reduce(UInt64(0)) { $0 + $1.input_tokens + $1.output_tokens }
            if free > 0 {
                HStack {
                    Text("Free models").foregroundStyle(.secondary)
                    Spacer()
                    Text("\(tokens(free)) tok")
                    Text("$0.00").foregroundStyle(Brand.green)
                }
                .font(.caption.monospacedDigit())
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.bar)
    }
}

struct QuotaBar: View {
    let label: String
    let window: WindowInfo

    var body: some View {
        let pct = Double(window.utilization)
        let color: Color = pct < 0.6 ? Brand.green : pct < 0.85 ? SessionStatus.needsYou.color : SessionStatus.exited.color
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text(label)
                Spacer()
                Text("\(Int((pct * 100).rounded()))%").foregroundStyle(color)
                if let reset = window.resets_at {
                    Text("· \(resetText(reset))").foregroundStyle(.tertiary)
                }
            }
            .font(.caption.monospacedDigit())
            ProgressView(value: min(max(pct, 0), 1)).tint(color).controlSize(.small)
        }
    }
}

// MARK: - Formatting

func tokens(_ n: UInt64) -> String {
    switch n {
    case ..<1000: "\(n)"
    case ..<1_000_000: String(format: "%.1fk", Double(n) / 1e3)
    default: String(format: "%.1fM", Double(n) / 1e6)
    }
}

func shortModel(_ m: String?) -> String {
    guard var m else { return "" }
    if m.hasPrefix("claude-") { m.removeFirst(7) }
    return m.split(separator: "-").filter { !($0.count == 8 && $0.allSatisfy(\.isNumber)) }.joined(separator: "-")
}

func resetText(_ at: UInt64) -> String {
    let secs = max(0, Int(at) - Int(Date().timeIntervalSince1970))
    switch secs {
    case ..<3600: return "\(secs / 60)m"
    case ..<86400: return "\(secs / 3600)h\(String(format: "%02d", secs % 3600 / 60))m"
    default: return "\(secs / 86400)d\(secs % 86400 / 3600)h"
    }
}
