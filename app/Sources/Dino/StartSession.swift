import SwiftUI

/// What opened the picker: where it starts, and whether the session goes in a new worktree.
struct StartRequest: Identifiable, Equatable {
    let id = UUID()
    /// A folder already chosen (⌘O, a folder opened with dino): straight to the agent.
    var folder: String?
    var worktree = false
}

extension DinoModel {
    /// ⌘N, the toolbar's +, the empty window: one way to start work, where first, then which agent.
    func startSession(in folder: String? = nil, worktree: Bool = false) {
        startRequest = StartRequest(folder: folder, worktree: worktree)
    }

    /// ⌘O: a folder from the Finder's panel, then the agent to run there.
    func openFolderToStart() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.directoryURL = folder
        panel.prompt = "Open"
        if panel.runModal() == .OK, let url = panel.url { startSession(in: url.path) }
    }

    /// Folders worth offering first: where you started sessions lately, the repos and folders in the
    /// sidebar, and where agents found on this Mac work. The current folder is offered on its own.
    var recentPlaces: [String] {
        // One folder by any spelling (/tmp and /private/tmp) is offered once.
        func key(_ path: String) -> String { URL(fileURLWithPath: path).resolvingSymlinksInPath().path }
        var seen = Set<String>([key(folder.path)])
        var out: [String] = []
        let candidates = recentFolders(on: "local")
            + sessions.filter { $0.host == nil }.compactMap(\.here)
            + repos.map(\.path)
            + found.sorted { $0.updated_at > $1.updated_at }.compactMap(\.cwd)
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let gitRepos = repos.filter { !$0.worktrees.isEmpty }.map(\.path)
        for path in candidates {
            // A session's folder deep in a git repo is offered as the repo it's in (a plain folder
            // in the sidebar, like your home, doesn't swallow the folders under it).
            let place = gitRepos.filter { SessionTree.contains($0, path) }.max { $0.count < $1.count } ?? path
            guard place != home, place != "/", !place.hasPrefix(NSTemporaryDirectory()), seen.insert(key(place)).inserted else { continue }
            var dir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: place, isDirectory: &dir), dir.boolValue else { continue }
            out.append(place)
            if out.count == 12 { break }
        }
        return out
    }
}

/// A repository of yours on GitHub, from `gh repo list`.
struct GitHubRepo: Decodable, Hashable {
    let nameWithOwner: String
    let isPrivate: Bool
    let description: String?
    let url: String
    var name: String { nameWithOwner.split(separator: "/").last.map(String.init) ?? nameWithOwner }
}

/// GitHub's CLI, if it's installed and signed in: dino asks it, and never for a token of its own.
enum GitHubCLI {
    static let path: String? = DinoEnvironment.loginPath.split(separator: ":").map { "\($0)/gh" }
        .first { FileManager.default.isExecutableFile(atPath: $0) }

    /// Your repositories, most recently pushed first; nil when `gh` is missing or not signed in.
    static func repos() async -> [GitHubRepo]? {
        guard let gh = path else { return nil }
        return await Task.detached {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: gh)
            p.arguments = ["repo", "list", "--limit", "200", "--json", "nameWithOwner,isPrivate,description,url"]
            let out = Pipe()
            p.standardOutput = out
            p.standardError = FileHandle.nullDevice
            guard (try? p.run()) != nil else { return nil }
            let data = out.fileHandleForReading.readDataToEndOfFile()
            p.waitUntilExit()
            guard p.terminationStatus == 0 else { return nil }
            return try? JSONDecoder().decode([GitHubRepo].self, from: data)
        }.value
    }
}

/// One row in the picker's first step.
private enum Place: Hashable, Identifiable {
    case folder(String, current: Bool)
    case github(GitHubRepo)
    case clone(String)
    case openFolder
    case newProject

    var id: String {
        switch self {
        case .folder(let p, _): "f:\(p)"
        case .github(let r): "g:\(r.nameWithOwner)"
        case .clone(let s): "c:\(s)"
        case .openFolder: "open"
        case .newProject: "new"
        }
    }

    /// What typing matches against.
    var text: String {
        switch self {
        case .folder(let p, _): p
        case .github(let r): "\(r.nameWithOwner) \(r.description ?? "")"
        case .clone(let s): s
        case .openFolder: "open folder choose"
        case .newProject: "new project create folder"
        }
    }
}

/// The picker behind ⌘N: where first (here, recent folders, your GitHub repositories, a folder or a
/// URL to clone, a new project), then which agent. Typing narrows both; Return takes the first.
struct StartSessionSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    let request: StartRequest

    @State private var query = ""
    @State private var highlighted: String?
    @State private var folder: String?
    @State private var worktree = false
    @State private var agent: String?
    @State private var github: [GitHubRepo]?
    @State private var githubLoaded = false
    @State private var cloning: Clone?
    @State private var problem: String?
    /// Repositories already cloned on this Mac, by normalized origin URL: read once when it opens.
    @State private var clones: [String: String] = [:]
    @FocusState private var searching: Bool

    private var places: [Place] {
        let words = query.lowercased().split(separator: " ").map(String.init)
        func matches(_ p: Place) -> Bool { words.allSatisfy { p.text.lowercased().contains($0) } }
        var out: [Place] = [.folder(model.folder.path, current: true)] + model.recentPlaces.map { .folder($0, current: false) }
        out += (github ?? []).map(Place.github)
        out = out.filter(matches)
        let q = query.trimmingCharacters(in: .whitespaces)
        if Clone.source(q) != nil, !out.contains(where: { if case .github(let r) = $0 { return r.nameWithOwner.lowercased() == q.lowercased() } else { return false } }) {
            out.insert(.clone(q), at: 0)
        }
        out += [Place.openFolder, .newProject].filter { words.isEmpty || matches($0) }
        return out
    }

    private var agents: [LauncherInfo] {
        let words = query.lowercased().split(separator: " ").map(String.init)
        return model.launchers.filter { l in words.allSatisfy { l.label.lowercased().contains($0) || l.short.contains($0) } }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            TextField(folder == nil ? "Search folders, repositories, or paste a URL to clone" : "Search agents", text: $query)
                .textFieldStyle(.roundedBorder)
                .focused($searching)
                .onSubmit(takeHighlighted)
                .onKeyPress(.downArrow) { move(1); return .handled }
                .onKeyPress(.upArrow) { move(-1); return .handled }
                .disabled(cloning != nil)
            if let cloning {
                CloneProgress(clone: cloning) { cancelClone() }
            } else if let folder {
                agentList(in: folder)
            } else {
                placeList
            }
            if let problem {
                Text(problem).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
            }
            footer
        }
        .padding(16)
        .frame(width: 520)
        .onAppear {
            worktree = request.worktree
            folder = request.folder
            agent = model.launchers.first { $0.agent_id != "shell" }?.short ?? model.launchers.first?.short
            highlighted = folder == nil ? places.first?.id : agent
            searching = true
        }
        .task { await loadGitHub() }
        .task {
            let places = [model.folder.path] + model.recentPlaces
            let parent = NewProjectSheet.lastParent
            clones = await Task.detached { Clone.known(in: places, parent: parent) }.value
        }
        .onChange(of: query) { _, _ in
            highlighted = folder == nil ? places.first?.id : agents.first?.short
        }
        .onExitCommand {
            if folder != nil, request.folder == nil { back() } else { dismiss() }
        }
    }

    // MARK: Parts

    private var header: some View {
        HStack(spacing: 6) {
            if folder != nil, request.folder == nil {
                Button { back() } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.plain)
                    .help("Back to choosing the folder (Esc)")
            }
            Text(folder == nil ? "New Session" : "New Session in \(URL(fileURLWithPath: folder!).lastPathComponent)")
                .font(.title3.weight(.semibold))
                .lineLimit(1)
            Spacer()
        }
    }

    private var placeList: some View {
        ScrollViewReader { proxy in
            List(selection: Binding(get: { highlighted }, set: { highlighted = $0 })) {
                ForEach(places) { p in
                    row(p).tag(p.id).id(p.id)
                        .contentShape(Rectangle())
                        .onTapGesture(count: 2) { pick(p) }
                        .onTapGesture { highlighted = p.id; pick(p) }
                }
                if !githubLoaded {
                    Label("Looking for your GitHub repositories…", systemImage: "hourglass").foregroundStyle(.secondary).font(.callout)
                } else if github == nil {
                    Text(GitHubCLI.path == nil
                         ? "To pick from your GitHub repositories here, install the GitHub CLI (brew install gh) and run gh auth login. Or paste a repository's URL above."
                         : "To pick from your GitHub repositories here, run gh auth login. Or paste a repository's URL above.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .listStyle(.inset)
            .frame(height: 340)
            .onChange(of: highlighted) { _, id in if let id { proxy.scrollTo(id) } }
        }
    }

    @ViewBuilder private func row(_ p: Place) -> some View {
        switch p {
        case .folder(let path, let current):
            Label {
                VStack(alignment: .leading, spacing: 1) {
                    Text(URL(fileURLWithPath: path).lastPathComponent).lineLimit(1)
                    Text(current ? "Here · \(shortPath(path))" : shortPath(path))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }
            } icon: {
                Image(systemName: FolderLook.icon(repo: isRepo(path)))
            }
        case .github(let r):
            Label {
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 4) {
                        Text(r.nameWithOwner).lineLimit(1)
                        if r.isPrivate { Image(systemName: "lock.fill").font(.caption2).foregroundStyle(.secondary) }
                    }
                    Text(existingClone(r.url).map { "Cloned at \(shortPath($0))" } ?? (r.description ?? "GitHub"))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            } icon: {
                Image(systemName: "arrow.down.circle")
            }
        case .clone(let s):
            Label("Clone \(s)", systemImage: "arrow.down.circle")
        case .openFolder:
            Label("Open Folder…", systemImage: "folder.badge.questionmark")
        case .newProject:
            Label("New Project…", systemImage: "folder.badge.plus")
        }
    }

    private func agentList(in folder: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            List(selection: Binding(get: { highlighted }, set: { highlighted = $0 })) {
                ForEach(agents) { l in
                    HStack {
                        Text(l.label)
                        Spacer()
                        if l.short == model.launchers.first?.short {
                            Text("⌘N").font(.caption).foregroundStyle(.tertiary)
                        }
                    }
                    .tag(l.short)
                    .contentShape(Rectangle())
                    .onTapGesture { highlighted = l.short; startAgent(l.short) }
                }
            }
            .listStyle(.inset)
            .frame(height: 220)
            HStack {
                if isRepo(folder) {
                    Toggle("In a new worktree", isOn: $worktree)
                        .help("Its own worktree and branch: its edits stay off your checkout until you apply them")
                }
                Spacer()
                Button("More Options…") {
                    model.folder = URL(fileURLWithPath: folder)
                    dismiss()
                    DispatchQueue.main.async { model.showNewSession = true }
                }
                .help("Mode, model, effort, an SSH host or another provider's model (⌃⌘N)")
            }
            Text(shortPath(folder)).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
        }
    }

    private var footer: some View {
        HStack {
            Text(folder == nil ? "↑↓ to choose · Return to pick · Esc to close" : "Return to start · Esc to go back")
                .font(.caption).foregroundStyle(.tertiary)
            Spacer()
            Button("Cancel", role: .cancel) { cancelClone(); dismiss() }
                .keyboardShortcut(.cancelAction)
        }
    }

    // MARK: Actions

    private func move(_ step: Int) {
        let ids = folder == nil ? places.map(\.id) : agents.map(\.short)
        guard !ids.isEmpty else { return }
        let at = highlighted.flatMap { ids.firstIndex(of: $0) } ?? -1
        highlighted = ids[max(0, min(ids.count - 1, at + step))]
    }

    private func takeHighlighted() {
        if folder != nil {
            if let id = highlighted ?? agents.first?.short { startAgent(id) }
        } else if let p = places.first(where: { $0.id == highlighted }) ?? places.first {
            pick(p)
        }
    }

    private func pick(_ p: Place) {
        problem = nil
        switch p {
        case .folder(let path, _): choose(path)
        case .github(let r):
            if let there = existingClone(r.url) { choose(there) } else { startClone(r.nameWithOwner, url: r.url) }
        case .clone(let s):
            guard let src = Clone.source(s) else { return }
            if let there = existingClone(src.url) { choose(there) } else { startClone(src.name, url: src.url) }
        case .openFolder:
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.canCreateDirectories = true
            panel.directoryURL = model.folder
            panel.prompt = "Open"
            if panel.runModal() == .OK, let url = panel.url { choose(url.path) }
        case .newProject:
            dismiss()
            DispatchQueue.main.async { model.showNewProject = true }
        }
    }

    private func choose(_ path: String) {
        folder = path
        query = ""
        highlighted = agent
        searching = true
    }

    private func back() {
        folder = nil
        query = ""
        highlighted = places.first?.id
    }

    private func startAgent(_ short: String) {
        guard let folder, let l = model.launchers.first(where: { $0.short == short }) else { return }
        model.folder = URL(fileURLWithPath: folder)
        model.rememberLocalFolder(folder)
        model.newSession(l, worktree: worktree && isRepo(folder) && l.agent_id != "shell", in: folder)
        dismiss()
    }

    private func loadGitHub() async {
        let repos = await GitHubCLI.repos()
        github = repos
        githubLoaded = true
        if folder == nil, highlighted == nil { highlighted = places.first?.id }
    }

    // MARK: Cloning

    private func startClone(_ name: String, url: String) {
        let parent = NewProjectSheet.lastParent
        let target = Clone.freeTarget(in: parent, name: Clone.folderName(name))
        let c = Clone(name: name, url: url, target: target)
        cloning = c
        c.run { ok, message in
            cloning = nil
            if ok {
                clones[Clone.normalized(url)] = target
                choose(target)
            } else {
                problem = message
            }
        }
    }

    private func cancelClone() {
        cloning?.cancel()
        cloning = nil
    }

    /// A clone of this repository already on this Mac, among the places dino knows.
    private func existingClone(_ url: String) -> String? {
        clones[Clone.normalized(url)]
    }

    private func isRepo(_ path: String) -> Bool {
        FileManager.default.fileExists(atPath: (path as NSString).appendingPathComponent(".git"))
    }
}

/// A `git clone` (or `gh repo clone`, which knows your private repositories) into a new folder,
/// with its progress line.
final class Clone: ObservableObject, Identifiable {
    let name: String
    let url: String
    let target: String
    @Published var line = ""
    private var process: Process?
    private var stopped = false

    init(name: String, url: String, target: String) {
        self.name = name
        self.url = url
        self.target = target
    }

    /// `owner/repo`, a GitHub URL or any git URL: what to clone, and its name.
    static func source(_ text: String) -> (name: String, url: String)? {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty, !t.contains(" ") else { return nil }
        if t.hasPrefix("https://") || t.hasPrefix("http://") || t.hasPrefix("git@") || t.hasPrefix("ssh://") {
            let name = t.split(separator: "/").suffix(2).joined(separator: "/").replacingOccurrences(of: ".git", with: "")
            return (name.replacingOccurrences(of: "git@github.com:", with: ""), t)
        }
        let parts = t.split(separator: "/")
        if parts.count == 2, parts.allSatisfy({ $0.allSatisfy { $0.isLetter || $0.isNumber || "-_.".contains($0) } }) {
            return (t, "https://github.com/\(t)")
        }
        return nil
    }

    /// A URL as compared: no scheme, user, `.git` or trailing slash, lowercased (`git@host:a/b` too).
    static func normalized(_ url: String) -> String {
        var u = url.lowercased()
        for prefix in ["https://", "http://", "ssh://", "git@"] where u.hasPrefix(prefix) { u.removeFirst(prefix.count) }
        u = u.replacingOccurrences(of: ":", with: "/")
        if u.hasSuffix("/") { u.removeLast() }
        if u.hasSuffix(".git") { u.removeLast(4) }
        return u
    }

    static func folderName(_ nameOrURL: String) -> String {
        let last = nameOrURL.split(separator: "/").last.map(String.init) ?? nameOrURL
        return last.hasSuffix(".git") ? String(last.dropLast(4)) : last
    }

    /// `parent/name`, or `name-2`, `name-3`… when that's taken by something else.
    static func freeTarget(in parent: String, name: String) -> String {
        let fm = FileManager.default
        var candidate = (parent as NSString).appendingPathComponent(name)
        var n = 2
        while fm.fileExists(atPath: candidate) {
            candidate = (parent as NSString).appendingPathComponent("\(name)-\(n)")
            n += 1
        }
        return candidate
    }

    /// The `origin` remote of the repository at `path`, from its `.git/config` (no git to run).
    static func origin(of path: String) -> String? {
        guard let config = try? String(contentsOfFile: (path as NSString).appendingPathComponent(".git/config"), encoding: .utf8) else { return nil }
        var inOrigin = false
        for raw in config.split(separator: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") { inOrigin = line == "[remote \"origin\"]"; continue }
            if inOrigin, line.hasPrefix("url") , let eq = line.firstIndex(of: "=") {
                return line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces)
            }
        }
        return nil
    }

    /// Clones on this Mac by normalized origin: the places given, and the folders in `parent`
    /// (where new clones go).
    static func known(in places: [String], parent: String) -> [String: String] {
        let children = ((try? FileManager.default.contentsOfDirectory(atPath: parent)) ?? []).map { (parent as NSString).appendingPathComponent($0) }
        var out: [String: String] = [:]
        for path in places + children {
            if let origin = origin(of: path), out[normalized(origin)] == nil { out[normalized(origin)] = path }
        }
        return out
    }

    func run(done: @escaping @MainActor (Bool, String) -> Void) {
        let p = Process()
        let isGitHub = Clone.normalized(url).hasPrefix("github.com/")
        if isGitHub, let gh = GitHubCLI.path {
            p.executableURL = URL(fileURLWithPath: gh)
            p.arguments = ["repo", "clone", Clone.normalized(url).replacingOccurrences(of: "github.com/", with: ""), target, "--", "--progress"]
        } else {
            p.executableURL = URL(fileURLWithPath: "/usr/bin/git")
            p.arguments = ["clone", "--progress", url, target]
        }
        let err = Pipe()
        p.standardError = err
        p.standardOutput = FileHandle.nullDevice
        p.environment = ProcessInfo.processInfo.environment.merging(["PATH": DinoEnvironment.loginPath, "GIT_TERMINAL_PROMPT": "0"]) { $1 }
        var tail = ""
        err.fileHandleForReading.readabilityHandler = { [weak self] h in
            let chunk = String(decoding: h.availableData, as: UTF8.self)
            guard !chunk.isEmpty else { return }
            tail = String((tail + chunk).suffix(400))
            let last = tail.split(whereSeparator: { $0 == "\r" || $0 == "\n" }).last.map(String.init) ?? ""
            DispatchQueue.main.async { if self?.line != last { self?.line = last } }
        }
        p.terminationHandler = { proc in
            err.fileHandleForReading.readabilityHandler = nil
            let ok = proc.terminationStatus == 0
            // Stopped halfway: the half-made folder isn't left behind (it's always a new one).
            if !ok, self.stopped { try? FileManager.default.removeItem(atPath: self.target) }
            let reason = proc.terminationReason == .uncaughtSignal ? "Stopped." : "Couldn't clone \(self.name): \(tail.split(separator: "\n").last.map(String.init) ?? "git failed")"
            Task { @MainActor in done(ok, ok ? "" : reason) }
        }
        do {
            try p.run()
            process = p
        } catch {
            Task { @MainActor in done(false, "Couldn't start git: \(error.localizedDescription)") }
        }
    }

    func cancel() {
        stopped = true
        process?.terminate()
    }
}

private struct CloneProgress: View {
    @ObservedObject var clone: Clone
    let cancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Cloning \(clone.name) into \(shortPath(clone.target))…").lineLimit(1).truncationMode(.middle)
            }
            Text(clone.line.isEmpty ? " " : clone.line).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
            Button("Stop", action: cancel)
        }
        .frame(maxWidth: .infinity, minHeight: 220, alignment: .topLeading)
    }
}
