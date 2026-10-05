import AppKit
import SwiftUI

extension DinoSettings {
    /// One compiler cache for every session's builds on this Mac; nil from an older dinod.
    struct BuildCache: Codable, Equatable {
        var enabled: Bool
        var size_gb: Int

        static let standard = BuildCache(enabled: true, size_gb: 10)
        /// What Settings offers; one set some other way is offered too.
        static let sizes = [5, 10, 20, 50, 100]
    }
}

/// See crates/dino-core/src/ipc.rs.
struct BuildCacheInfo: Codable, Equatable {
    var enabled: Bool
    var sccache: String?
    var version: String?
    var install: String
    var unused: String?
    var running: Bool
    var dir: String
    var max_bytes: UInt64
    var size_bytes: UInt64?
    var stats: BuildCacheStats?
}

struct BuildCacheStats: Codable, Equatable {
    var requests: UInt64
    var hits: UInt64
    var misses: UInt64
    var not_cacheable: UInt64
    var other: UInt64

    /// Hits among the compiles it could keep, in percent.
    var hitRate: Double? { hits + misses > 0 ? Double(hits) * 100 / Double(hits + misses) : nil }
}

private struct BuildCacheResponse: Decodable { let info: BuildCacheInfo }

extension DinoConnection {
    func buildCache() throws -> BuildCacheInfo {
        try JSONDecoder().decode(BuildCacheResponse.self, from: send(["type": "build_cache"])).info
    }

    /// Runs sccache's install command in a new shell; the session's id.
    func buildCacheInstall() throws -> String? {
        try request(["type": "build_cache_install"]).id
    }
}

extension BuildCacheInfo {
    /// "1,234 of 1,500 compiles (82%)".
    var hitsLine: String? {
        guard let s = stats, let rate = s.hitRate else { return nil }
        return "\(s.hits.formatted()) of \((s.hits + s.misses).formatted()) compiles (\(Int(rate.rounded()))%)"
    }

    var sizeLine: String {
        let max = ByteCountFormatter.string(fromByteCount: Int64(max_bytes), countStyle: .file)
        guard let size = size_bytes else { return "Up to \(max)" }
        return "\(ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file)) of \(max)"
    }
}

/// Settings → Workspaces → Worktrees → Build Cache: the switch, its size, how it's doing, and
/// sccache when it isn't installed.
struct BuildCacheSection: View {
    @EnvironmentObject var store: SettingsStore
    @EnvironmentObject var model: DinoModel
    @State private var info: BuildCacheInfo?
    @State private var error: String?

    private var current: DinoSettings.BuildCache { store.settings?.machine.build_cache ?? .standard }

    var body: some View {
        Section {
            Toggle(isOn: Binding(
                get: { current.enabled },
                set: { on in
                    var b = current
                    b.enabled = on
                    store.update { $0.machine.build_cache = b }
                }
            )) {
                Text("Share one build cache across worktrees")
                Text("Rust, through sccache")
            }
            .orgLocked("machine.build_cache.enabled")
            if current.enabled {
                if let info, info.sccache == nil {
                    LabeledContent {
                        HStack {
                            Text(info.install).font(.callout.monospaced()).textSelection(.enabled)
                            Button("Install…") { install() }
                                .help("Runs \(info.install) in a new dino shell, where you see it run")
                        }
                    } label: {
                        Text("sccache isn't installed")
                        Text("The cache starts working once it is.")
                    }
                } else if let info {
                    if let why = info.unused {
                        Label(why, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange).font(.callout)
                    } else {
                        LabeledContent("Hits") {
                            Text(info.hitsLine ?? (info.running ? "None yet" : "None yet: it starts with the next session"))
                                .monospacedDigit()
                        }
                        LabeledContent("Size") { Text(info.sizeLine).monospacedDigit() }
                    }
                }
                Picker("Keep up to", selection: Binding(
                    get: { current.size_gb },
                    set: { gb in
                        var b = current
                        b.size_gb = gb
                        store.update { $0.machine.build_cache = b }
                    }
                )) {
                    ForEach(Array(Set(DinoSettings.BuildCache.sizes + [current.size_gb])).sorted(), id: \.self) { gb in
                        Text("\(gb) GB").tag(gb)
                    }
                }
                .orgLocked("machine.build_cache.size_gb")
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill").foregroundStyle(.red).font(.callout)
            }
        } header: {
            Text("Build Cache")
        } footer: {
            Footnote("Every agent dino starts, and every dino shell, builds Rust through one cache on this Mac, so a new worktree compiles only what another worktree hasn't compiled already. Each worktree keeps its own target folder. A missing, broken or full cache never fails a build: it compiles as it would without one. A wrapper the repo or you set up yourself is used instead. Turning it on applies to sessions started from then on; off, at once.")
        }
        .task(id: current) {
            // Hits come in as sessions build; asking sccache is a quick local call.
            while !Task.isCancelled {
                if let next = try? await Task.detached(operation: { try DinoConnection(path: DinoEnvironment.socketPath).buildCache() }).value, next != info {
                    info = next
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    private func install() {
        let model = model
        Task.detached {
            do {
                let session = try DinoConnection(path: DinoEnvironment.socketPath).buildCacheInstall()
                await MainActor.run {
                    error = nil
                    guard let session else { return }
                    model.pendingSelect = session
                    NSApp.windows.first { w in w.isVisible && !(w.identifier?.rawValue.hasPrefix(SettingsView.windowID) ?? false) && w.canBecomeMain }?
                        .makeKeyAndOrderFront(nil)
                }
            } catch {
                await MainActor.run { self.error = error.localizedDescription }
            }
        }
    }
}

/// Offered once, over a session in a Rust project while sccache isn't installed: what the build
/// cache would do, and its install command. Install runs it in a new shell; either button, or the
/// close button, and it isn't offered again.
struct BuildCacheOffer: View {
    @EnvironmentObject var model: DinoModel
    @AppStorage(BuildCacheOffer.key) private var answered = false
    let install: String

    static let key = "buildCacheOffered"

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "shippingbox").foregroundStyle(Brand.green)
            VStack(alignment: .leading, spacing: 1) {
                Text("Builds could share one cache across worktrees").font(.callout)
                Text(install).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
            }
            Button("Install sccache") {
                answered = true
                let model = model
                Task.detached {
                    let session = try? DinoConnection(path: DinoEnvironment.socketPath).buildCacheInstall()
                    await MainActor.run { if let session { model.pendingSelect = session } }
                }
            }
            .buttonStyle(.borderedProminent)
            .tint(Brand.green)
            .controlSize(.small)
            .help("Runs \(install) in a new dino shell, where you see it run")
            Button { answered = true } label: { Image(systemName: "xmark").font(.caption) }
                .buttonStyle(.borderless)
                .help("Not now; Settings → Workspaces → Worktrees has it")
        }
        .padding(.leading, 12)
        .padding(.trailing, 8)
        .padding(.vertical, 6)
        .background(Capsule().fill(.regularMaterial))
        .overlay(Capsule().strokeBorder(Color.primary.opacity(0.1)))
        .shadow(color: .black.opacity(0.2), radius: 6, y: 2)
        .padding(14)
        .transition(.move(edge: .bottom).combined(with: .opacity))
    }
}

/// Over the terminals, bottom right: [`BuildCacheOffer`] while a Rust project is shown, until answered.
struct BuildCacheOfferSlot: View {
    @EnvironmentObject var model: DinoModel
    @StateObject private var state = BuildCacheOfferState()
    @AppStorage(BuildCacheOffer.key) private var answered = false

    var body: some View {
        Group {
            if !answered, state.rustShown, let install = state.install {
                BuildCacheOffer(install: install)
            }
        }
        .onAppear { state.consider(model.selectedSession) }
        .onChange(of: model.selected) { _, _ in state.consider(model.selectedSession) }
    }
}

/// Whether to offer sccache: not answered yet, a Rust project shown, the cache on and sccache
/// missing. dinod is asked once per app run, the first time a Rust project is shown.
@MainActor
final class BuildCacheOfferState: ObservableObject {
    @Published private(set) var install: String?
    @Published private(set) var rustShown = false
    private var asked = false

    func consider(_ session: SessionInfo?) {
        guard !UserDefaults.standard.bool(forKey: BuildCacheOffer.key) else { return }
        let rust = session.flatMap { s in s.host == nil ? s.cwd : nil }.map(Self.isRust) ?? false
        if rustShown != rust { rustShown = rust }
        guard rust, !asked else { return }
        asked = true
        Task.detached {
            guard let info = try? DinoConnection(path: DinoEnvironment.socketPath).buildCache() else { return }
            await MainActor.run {
                self.install = info.enabled && info.sccache == nil ? info.install : nil
            }
        }
    }

    /// A Cargo project: its folder or one above it, short of the home folder.
    static func isRust(_ cwd: String) -> Bool {
        let home = FileManager.default.homeDirectoryForCurrentUser.standardizedFileURL.path
        var url = URL(fileURLWithPath: cwd).standardizedFileURL
        for _ in 0..<8 {
            if url.path == home || url.path == "/" { return false }
            if FileManager.default.fileExists(atPath: url.appendingPathComponent("Cargo.toml").path) { return true }
            url.deleteLastPathComponent()
        }
        return false
    }
}
