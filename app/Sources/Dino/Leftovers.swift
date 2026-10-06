import SwiftUI

/// A build dinod found running for no session as it started: an agent's, left behind when its
/// session or an older dinod went (`leftovers` in dinod's state).
struct Leftover: Codable, Equatable, Identifiable {
    var pid: UInt32
    var name: String?
    var cwd: String?
    var started: UInt64?

    var id: UInt32 { pid }

    /// "cargo test in dino-app-poc"
    var line: String {
        let name = name.flatMap { $0.isEmpty ? nil : $0 } ?? "a build"
        guard let cwd, !cwd.isEmpty else { return name }
        return "\(name) in \((cwd as NSString).lastPathComponent)"
    }
}

/// Over the terminals, bottom right, while there are any: the builds left running for no
/// session, nothing waiting on what they build, and Stop for all of them. Closed, it stays away
/// until others are found.
struct LeftoversOffer: View {
    let leftovers: [Leftover]
    @State private var dismissed: Set<UInt32> = []
    @State private var stopping = false

    var body: some View {
        let shown = leftovers.filter { !dismissed.contains($0.pid) }
        if !shown.isEmpty {
            HStack(spacing: 8) {
                Image(systemName: "hammer").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 1) {
                    Text(shown.count == 1 ? "A build is running for no session" : "\(shown.count) builds are running for no session")
                        .font(.callout)
                    Text(shown.map(\.line).joined(separator: " · "))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                }
                .frame(maxWidth: 320, alignment: .leading)
                .help(shown.map { "\($0.line) (pid \($0.pid))" }.joined(separator: "\n"))
                Button("Stop") {
                    stopping = true
                    let pids = shown.map(\.pid)
                    Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).stopLeftovers(pids: pids) }
                }
                .buttonStyle(.borderedProminent)
                .tint(.orange)
                .controlSize(.small)
                .disabled(stopping)
                .help("Stops each of them and everything it runs: their sessions are gone, so nothing waits on what they build")
                Button { dismissed.formUnion(shown.map(\.pid)) } label: { Image(systemName: "xmark").font(.caption) }
                    .buttonStyle(.borderless)
                    .help("Leave them running")
            }
            .padding(.leading, 12)
            .padding(.trailing, 8)
            .padding(.vertical, 6)
            .background(Capsule().fill(.regularMaterial))
            .overlay(Capsule().strokeBorder(Color.primary.opacity(0.1)))
            .shadow(color: .black.opacity(0.2), radius: 6, y: 2)
            .padding(14)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .onChange(of: leftovers) { _, _ in stopping = false }
        }
    }
}
