import SwiftUI

/// dinod's power state, observed apart from `DinoModel`: other apps take and drop power assertions
/// all the time (a chat app finishing a task, a video playing), and each would otherwise redraw the
/// whole window. Only the views that show it (here and in Settings → Power) observe this.
@MainActor
final class PowerState: ObservableObject {
    static let shared = PowerState()
    @Published var info: PowerInfo?
}

/// What keeps the Mac awake, at the foot of the sidebar: one line saying what's true now
/// ("Staying awake · 2 agents working"), opening to every process that does. Hidden when nothing
/// you'd care about keeps it awake.
struct AwakeStatus: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject private var state = PowerState.shared
    @State private var showing = false

    var body: some View {
        if let power = state.info, let line = power.awakeLine(session: model.awakeSessionName) {
            Button { showing.toggle() } label: {
                Label {
                    Text(line).lineLimit(1).truncationMode(.tail)
                } icon: {
                    Image(systemName: power.holding ? "laptopcomputer" : "cup.and.heat.waves")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("\(line). Click for everything keeping the Mac awake.")
            .accessibilityLabel(line)
            .accessibilityHint("Shows everything keeping the Mac awake")
            .popover(isPresented: $showing, arrowEdge: .trailing) {
                AwakePopover(close: { showing = false })
                    .environmentObject(model)
            }
        }
    }
}

extension DinoModel {
    /// "Claude: Retry with backoff" for session `id`, as the sidebar names it.
    func awakeSessionName(_ id: String) -> String? {
        sessions.first { $0.id == id }.map { $0.plainShell ? $0.display : "\($0.agentWord): \($0.display)" }
    }
}

/// Everything keeping the Mac awake, with the way to Settings → Power.
private struct AwakePopover: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject private var state = PowerState.shared
    @Environment(\.openWindow) private var openWindow
    @AppStorage("settingsTab") private var settingsPane: SettingsPane = .general
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Keeping the Mac awake").font(.headline)
            if state.info?.holding == true {
                Label("Awake with the lid closed: dino turned system sleep off while agents work", systemImage: "laptopcomputer")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            AwakeList(open: { id in
                close()
                model.select(id)
            })
            Divider()
            Button {
                close()
                settingsPane = .power
                openWindow(id: SettingsView.windowID)
            } label: {
                Text("Power Settings…").font(.callout)
            }
            .buttonStyle(.link)
            .help("Keep the Mac awake while agents work, and with the lid closed")
        }
        .padding(14)
        .frame(width: 340, alignment: .leading)
    }
}

/// Every process holding a power assertion against sleep: dino's own first, macOS's own last and
/// dimmed. `open` goes to a session, when one is named.
struct AwakeList: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject private var state = PowerState.shared
    var open: ((String) -> Void)?

    private var holders: [AwakeHolder] {
        let all = state.info?.awake ?? []
        return all.filter { !$0.system } + all.filter(\.system)
    }

    var body: some View {
        let holders = holders
        if holders.isEmpty {
            Text("Nothing is keeping the Mac awake: it sleeps when idle.")
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(holders) { h in
                    row(h)
                }
            }
        }
    }

    private func row(_ h: AwakeHolder) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: h.ours ? "cup.and.heat.waves.fill" : h.system ? "gearshape" : "cup.and.heat.waves")
                .foregroundStyle(h.ours ? AnyShapeStyle(Brand.green) : AnyShapeStyle(.secondary))
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(h.ours ? "dino" : h.process).font(.callout.weight(.medium))
                    Text(verbatim: "pid \(h.pid)").font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
                }
                if let id = h.session, let name = model.awakeSessionName(id) {
                    if let open {
                        Button { open(id) } label: { Text(name).lineLimit(1) }
                            .buttonStyle(.link)
                            .font(.caption)
                            .help("Go to this session")
                    } else {
                        Text(name).font(.caption).lineLimit(1)
                    }
                }
                Text(detail(h))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .opacity(h.system ? 0.7 : 1)
        .accessibilityElement(children: .combine)
    }

    /// "Idle sleep · since 3m · caffeinate command-line tool"
    private func detail(_ h: AwakeHolder) -> String {
        var parts = [h.kindLabel]
        if let since = h.since {
            parts.append("since \(Self.since(since))")
        }
        let name = h.ours && h.name.hasPrefix("dino: ") ? String(h.name.dropFirst(6)) : h.name
        if !name.isEmpty { parts.append(name) }
        return parts.joined(separator: " · ")
    }

    private static func since(_ t: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(t))
        let secs = Date().timeIntervalSince(date)
        if secs < 60 { return "just now" }
        if secs < 3600 { return "\(Int(secs / 60))m" }
        if Calendar.current.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}

/// Settings → Power: what keeps the Mac awake right now.
struct AwakeNow: View {
    @ObservedObject private var state = PowerState.shared
    @State private var open = false

    var body: some View {
        let all = state.info?.awake ?? []
        let count = all.filter { !$0.system }.count
        DisclosureGroup(isExpanded: $open) {
            AwakeList()
                .padding(.vertical, 4)
        } label: {
            LabeledContent("Keeping it awake now") {
                Text(all.isEmpty ? "Nothing" : count == 0 ? "Only macOS" : count == 1 ? "1 process" : "\(count) processes")
                    .foregroundStyle(.secondary)
            }
        }
    }
}
