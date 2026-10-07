import SwiftUI

/// ⌥⇧⌘N: a new project in one step: its folder, a git repository if you want one, and a tab in
/// it with a shell or an agent.
struct NewProjectSheet: View {
    @EnvironmentObject var model: DinoModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var parent = NewProjectSheet.lastParent
    @State private var git = true
    @State private var start = "shell"
    @State private var problem: String?
    @FocusState private var naming: Bool

    private static let parentKey = "newProject.parent"
    /// Where the last one went, else the usual folder for projects, else home.
    static var lastParent: String {
        if let p = UserDefaults.standard.string(forKey: parentKey), isFolder(p) { return p }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return ["Developer", "Projects", "projects", "code", "src", "dev"].map { "\(home)/\($0)" }.first(where: isFolder) ?? home
    }

    private static func isFolder(_ path: String) -> Bool {
        var dir: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &dir) && dir.boolValue
    }

    private var trimmed: String { name.trimmingCharacters(in: .whitespaces) }
    private var path: String { (parent as NSString).appendingPathComponent(trimmed) }
    private var valid: Bool { !trimmed.isEmpty && !trimmed.contains("/") && trimmed != "." && trimmed != ".." }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("New Project").font(.title3.weight(.semibold)).padding([.horizontal, .top], 20)
            Form {
                TextField("Name", text: $name, prompt: Text("my-app"))
                    .focused($naming)
                LabeledContent("In") {
                    HStack {
                        Text(shortPath(parent)).lineLimit(1).truncationMode(.middle)
                        Button("Change…") { choose() }
                    }
                }
                Toggle("Make it a git repository", isOn: $git)
                Picker("Open with", selection: $start) {
                    ForEach(model.launchers) { Text($0.label).tag($0.short) }
                }
            }
            .formStyle(.grouped)
            if let problem {
                Text(problem).font(.callout).foregroundStyle(.red).padding(.horizontal, 20)
            }
            HStack {
                Text(valid ? shortPath(path) : " ").font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Create") { create() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!valid)
            }
            .padding(20)
        }
        .frame(width: 460)
        .onAppear { naming = true }
        .onChange(of: name) { problem = nil }
    }

    private func choose() {
        if let url = FolderPanel.choose(in: URL(fileURLWithPath: parent), verb: "Use") { parent = url.path }
    }

    private func create() {
        let fm = FileManager.default
        // An empty folder of that name is fine to use; anything else is someone's work.
        if fm.fileExists(atPath: path), !((try? fm.contentsOfDirectory(atPath: path).isEmpty) ?? false) {
            problem = "\(shortPath(path)) already exists. Choose another name, or open it with Open Folder (⌘O)."
            return
        }
        do {
            try fm.createDirectory(atPath: path, withIntermediateDirectories: true)
        } catch {
            problem = error.localizedDescription
            return
        }
        if git {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/git")
            p.arguments = ["init", "-q"]
            p.currentDirectoryURL = URL(fileURLWithPath: path)
            let failed = (try? p.run()).map { p.waitUntilExit(); return p.terminationStatus != 0 } ?? true
            if failed {
                problem = "Created \(shortPath(path)), but couldn't make it a git repository."
                return
            }
        }
        UserDefaults.standard.set(parent, forKey: Self.parentKey)
        model.folder = URL(fileURLWithPath: path)
        if let l = model.launchers.first(where: { $0.short == start }) ?? model.launchers.first {
            model.newSession(l, in: path)
        }
        dismiss()
    }
}
