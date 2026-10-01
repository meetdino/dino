import AppKit
import CryptoKit
import SwiftUI
import WebKit

/// A session's browser: its dev server, a page an agent printed, or an HTML file.
@MainActor
final class WebPage: NSObject, ObservableObject, WKNavigationDelegate, WKUIDelegate {
    /// The session it belongs to; "" for none.
    let session: String
    let view: WKWebView
    /// What the address field shows while the user isn't typing in it.
    @Published var address = ""
    @Published private(set) var url: URL?
    @Published private(set) var title = ""
    @Published private(set) var canGoBack = false
    @Published private(set) var canGoForward = false
    @Published private(set) var loading = false
    @Published private(set) var progress = 0.0
    @Published var failure: String?
    /// A dev server that was just started: its page loads once dinod knows the port.
    @Published var waitingFor: String?
    /// dinod has answered the start: states from before it may still show the last run's exit.
    var started = false
    /// The dev server whose output the log shows.
    @Published var server: String?
    @Published var showLog = false

    /// A server that just said its address may not accept connections yet: retry until then.
    private var retryUntil = Date.distantPast
    /// What dino itself last loaded (a file you opened may be any kind; one a page goes to may not).
    private var requested: URL?
    private var observers: [NSKeyValueObservation] = []

    init(session: String) {
        self.session = session
        let config = WKWebViewConfiguration()
        config.preferences.isElementFullscreenEnabled = true
        view = WKWebView(frame: .zero, configuration: config)
        super.init()
        view.navigationDelegate = self
        view.uiDelegate = self
        view.allowsBackForwardNavigationGestures = true
        view.allowsMagnification = true
        view.isInspectable = true
        observers = [
            view.observe(\.url) { [weak self] v, _ in MainActor.assumeIsolated { self?.sync(v) } },
            view.observe(\.title) { [weak self] v, _ in MainActor.assumeIsolated { self?.title = v.title ?? "" } },
            view.observe(\.canGoBack) { [weak self] v, _ in MainActor.assumeIsolated { self?.canGoBack = v.canGoBack } },
            view.observe(\.canGoForward) { [weak self] v, _ in MainActor.assumeIsolated { self?.canGoForward = v.canGoForward } },
            view.observe(\.isLoading) { [weak self] v, _ in MainActor.assumeIsolated { self?.loading = v.isLoading } },
            view.observe(\.estimatedProgress) { [weak self] v, _ in MainActor.assumeIsolated { self?.progress = v.estimatedProgress } },
        ]
    }

    private func sync(_ v: WKWebView) {
        guard let u = v.url else { return }
        url = u
        address = Self.display(u)
    }

    /// `localhost:5173/app` rather than the full URL; a file by its path.
    static func display(_ u: URL) -> String {
        if u.isFileURL { return shortPath(u.path) }
        let s = u.absoluteString
        if u.scheme == "http", LinkTarget.isLocal(u) { return String(s.dropFirst("http://".count)) }
        return s
    }

    /// `patience`: how long to keep retrying a refused connection (a server still starting).
    func load(_ u: URL, patience: TimeInterval = 3) {
        failure = nil
        retryUntil = Date().addingTimeInterval(patience)
        requested = u
        url = u
        address = Self.display(u)
        if u.isFileURL {
            view.loadFileURL(u, allowingReadAccessTo: u.deletingLastPathComponent())
        } else {
            view.load(URLRequest(url: u))
        }
    }

    /// What the user typed: a URL, `localhost:3000`, `:3000`, `3000`, or a file path.
    func go(_ typed: String) {
        let t = typed.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return }
        if let port = UInt16(t.hasPrefix(":") ? String(t.dropFirst()) : t) {
            if let u = URL(string: "http://localhost:\(port)/") { load(u) }
            return
        }
        if t.hasPrefix("/") || t.hasPrefix("~") {
            let path = (t as NSString).expandingTildeInPath
            if FileManager.default.fileExists(atPath: path) { load(URL(fileURLWithPath: path)) }
            else { failure = "No file at \(t)" }
            return
        }
        let full = t.contains("://") ? t : (t.hasPrefix("localhost") || t.hasPrefix("127.") || t.hasPrefix("0.0.0.0") ? "http://" : "https://") + t
        if let u = URL(string: full) { load(u) } else { failure = "Not an address: \(t)" }
    }

    func reload() {
        failure = nil
        if view.url == nil, let url { load(url) } else { view.reload() }
    }

    /// dinod's news about the session's servers: load the one we're waiting on once it has a port.
    func follow(_ previews: [PreviewInfo]) {
        guard let name = waitingFor, let p = previews.first(where: { $0.name == name }) else { return }
        if p.running, let s = p.url, let u = URL(string: s) {
            waitingFor = nil
            load(u, patience: 20)
        } else if started, !p.running, p.exit != nil {
            waitingFor = nil
            failure = "\(name) stopped: \(p.exit ?? "")"
            showLog = true
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ v: WKWebView, didFailProvisionalNavigation _: WKNavigation!, withError error: Error) {
        fail(v, error)
    }

    func webView(_ v: WKWebView, didFail _: WKNavigation!, withError error: Error) {
        fail(v, error)
    }

    private func fail(_: WKWebView, _ error: Error) {
        let e = error as NSError
        if e.domain == NSURLErrorDomain, e.code == NSURLErrorCancelled { return }
        // WebKit's "frame load interrupted" when a download or a non-web link replaces the page.
        if e.domain == "WebKitErrorDomain", e.code == 102 { return }
        if e.domain == NSURLErrorDomain, e.code == NSURLErrorCannotConnectToHost, Date() < retryUntil, let url {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
                guard let self, self.url == url, self.failure == nil else { return }
                self.view.load(URLRequest(url: url))
            }
            return
        }
        failure = e.code == NSURLErrorCannotConnectToHost
            ? "Nothing is answering at \(url.map(Self.display) ?? "this address"). Is the server running?"
            : e.localizedDescription
    }

    func webView(_: WKWebView, didFinish _: WKNavigation!) {
        failure = nil
    }

    func webView(_: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
        guard let u = action.request.url, let scheme = u.scheme?.lowercased() else {
            decisionHandler(.allow)
            return
        }
        // mailto:, app links and the like belong to other apps: only a click on a link in the
        // page itself, not a script or an iframe, and never without asking (see `LinkPolicy`).
        if !Self.webSchemes.contains(scheme) {
            decisionHandler(.cancel)
            if action.navigationType == .linkActivated, action.targetFrame?.isMainFrame == true { LinkPolicy.openElsewhere(u) }
            return
        }
        // A page may go to another page beside it, not to any file (a script there could later be
        // handed to the browser): what was loaded here, back, forward and reload stay allowed.
        if scheme == "file", action.targetFrame?.isMainFrame == true, action.navigationType != .backForward,
           action.navigationType != .reload, u.standardizedFileURL.path != requested?.standardizedFileURL.path,
           !Self.pageExtensions.contains(u.pathExtension.lowercased())
        {
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }

    private static let webSchemes: Set<String> = ["http", "https", "file", "about", "data", "blob"]
    /// Local files a page may navigate to.
    private static let pageExtensions: Set<String> = ["html", "htm", "xhtml"]

    /// "Open in your browser": web pages, and local HTML with the browser itself (the default app
    /// for a file could run it). False for anything else.
    @discardableResult
    func openInBrowser() -> Bool {
        guard let u = url else { return false }
        if u.scheme == "http" || u.scheme == "https" {
            return NSWorkspace.shared.open(u)
        }
        guard u.isFileURL, ["html", "htm"].contains(u.pathExtension.lowercased()),
              let probe = URL(string: "https://example.com"), let browser = NSWorkspace.shared.urlForApplication(toOpen: probe)
        else { return false }
        NSWorkspace.shared.open([u], withApplicationAt: browser, configuration: NSWorkspace.OpenConfiguration())
        return true
    }

    // MARK: WKUIDelegate

    /// `target="_blank"` and `window.open`: one pane, so they open here.
    func webView(_ v: WKWebView, createWebViewWith _: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures _: WKWindowFeatures) -> WKWebView? {
        guard action.targetFrame == nil else { return nil }
        if let u = action.request.url, let scheme = u.scheme?.lowercased(), !Self.webSchemes.contains(scheme) {
            // Another app's link: as in `decidePolicyFor`, only for a click, and after asking.
            if action.navigationType == .linkActivated { LinkPolicy.openElsewhere(u) }
            return nil
        }
        v.load(action.request)
        return nil
    }

    /// Who is asking, as the alert's title: a page can't pose as dino.
    private static func says(_ frame: WKFrameInfo) -> String {
        let o = frame.securityOrigin
        if o.protocol == "file" { return "A local file says" }
        guard !o.host.isEmpty else { return "This page says" }
        return "\(o.protocol)://\(o.host)\(o.port == 0 ? "" : ":\(o.port)") says"
    }

    func webView(_: WKWebView, runJavaScriptAlertPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor () -> Void) {
        let alert = NSAlert()
        alert.messageText = Self.says(frame)
        alert.informativeText = message
        alert.runModal()
        completionHandler()
    }

    func webView(_: WKWebView, runJavaScriptConfirmPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor (Bool) -> Void) {
        let alert = NSAlert()
        alert.messageText = Self.says(frame)
        alert.informativeText = message
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        completionHandler(alert.runModal() == .alertFirstButtonReturn)
    }

    func webView(_: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String, defaultText: String?, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor (String?) -> Void) {
        let alert = NSAlert()
        alert.messageText = Self.says(frame)
        alert.informativeText = prompt
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.stringValue = defaultText ?? ""
        alert.accessoryView = field
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        completionHandler(alert.runModal() == .alertFirstButtonReturn ? field.stringValue : nil)
    }

    func webView(_: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters, initiatedByFrame _: WKFrameInfo, completionHandler: @escaping @MainActor ([URL]?) -> Void) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.canChooseDirectories = parameters.allowsDirectories
        completionHandler(panel.runModal() == .OK ? panel.urls : nil)
    }
}

extension DinoModel {
    /// The browser for `session` (one that isn't a live session shares the "" page).
    func webPage(for session: String?) -> WebPage {
        let key = session.flatMap { id in sessions.contains { $0.id == id } ? id : nil } ?? ""
        if let p = webPages[key] { return p }
        let p = WebPage(session: key)
        webPages[key] = p
        return p
    }

    /// The preview pane, for `session` (it's selected), at `url`; without one, at the session's
    /// running server or the address it last printed.
    func openPreview(session: String?, url: URL? = nil) {
        if let f = openFile {
            guard f.confirmClose() else { return }
            f.stop()
        }
        if let session, session != selected, sessions.contains(where: { $0.id == session }) { select(session) }
        let page = webPage(for: session)
        let s = sessions.first { $0.id == page.session }
        if let url {
            page.load(url)
        } else if page.url == nil, page.waitingFor == nil, let s {
            let running = s.previews?.first { $0.running && $0.url != nil }
            if let u = (running?.url ?? s.local_url).flatMap(URL.init(string:)) {
                page.load(u)
                if let running { page.server = running.name }
            }
        }
        // Its printed address is on show now: don't offer it again.
        if let s, let u = s.local_url { offered[s.id] = u }
        sidePane = .preview
    }

    func togglePreview() {
        if sidePane == .preview { closeSidePane() } else { openPreview(session: selected) }
    }

    func previewConfigs(_ session: String) async -> ([PreviewConfig], String?) {
        await Task.detached {
            (try? DinoConnection(path: DinoEnvironment.socketPath).previewConfigs(session: session)) ?? ([], nil)
        }.value
    }

    /// Start the server; the page loads once dinod has seen its port. The first time, and again
    /// whenever it changes, only once you've seen the command it runs (see `ServerApproval`).
    func startServer(_ name: String, page: WebPage) {
        let session = page.session
        let repo = sessions.first { $0.id == session }?.cwd
        Task {
            // The launch file as it is now, not as the menu last polled it.
            let (configs, _) = await previewConfigs(session)
            guard let c = configs.first(where: { $0.name == name }) else {
                page.failure = "\(name) isn't in the launch files any more."
                return
            }
            guard ServerApproval.confirm(c, repo: repo ?? c.cwd) else { return }
            page.failure = nil
            page.server = name
            page.waitingFor = name
            page.started = false
            do {
                try await Task.detached { try DinoConnection(path: DinoEnvironment.socketPath).previewStart(session: session, name: name, approved: c) }.value
                page.started = true
            } catch {
                page.waitingFor = nil
                page.failure = error.localizedDescription
            }
        }
    }

    func stopServer(_ name: String, page: WebPage) {
        let session = page.session
        if page.waitingFor == name { page.waitingFor = nil }
        Task.detached { try? DinoConnection(path: DinoEnvironment.socketPath).previewStop(session: session, name: name) }
    }

    /// A starter `.dino/launch.json` for the session's folder, open for editing; with `script`, one
    /// that runs that package.json script, left closed: the pane offers to start it.
    func addLaunchFile(in cwd: String, script: PackageScript? = nil) {
        let path = (cwd as NSString).appendingPathComponent(".dino/launch.json")
        if !FileManager.default.fileExists(atPath: path) {
            let fm = FileManager.default
            let node = fm.fileExists(atPath: (cwd as NSString).appendingPathComponent("package.json"))
            let pick = script ?? (node ? PackageScript(runner: PackageScript.runner(in: cwd), name: "dev") : nil)
            let run = pick.map { #""runtimeExecutable": "\#($0.runner)",\#n      "runtimeArgs": ["run", "\#($0.name)"]"# }
                ?? #""runtimeExecutable": "python3",\#n      "runtimeArgs": ["-m", "http.server", "8000"],\#n      "port": 8000"#
            let text = """
            {
              // Dev servers dino's preview can start (the same shape as .claude/launch.json).
              // "port" is optional: dino reads it from the server's output otherwise.
              "version": "0.0.1",
              "configurations": [
                {
                  "name": "\(pick?.name ?? "web")",
                  \(run)
                }
              ]
            }

            """
            do {
                try fm.createDirectory(atPath: (path as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
                try text.write(toFile: path, atomically: true, encoding: .utf8)
            } catch {
                self.error = error.localizedDescription
                return
            }
        }
        if script == nil { openFile(path, line: 7) }
    }
}

/// A package.json script that serves the app, as `npm run dev` (or the lockfile's own runner).
struct PackageScript: Equatable, Identifiable {
    var runner: String
    var name: String
    var id: String { name }
    var command: String { "\(runner) run \(name)" }

    /// The ones a preview would want, most likely first.
    private static let serving = ["dev", "start", "serve", "preview", "develop", "storybook"]

    static func find(in cwd: String) -> [PackageScript] {
        let url = URL(fileURLWithPath: cwd).appendingPathComponent("package.json")
        guard let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let scripts = json["scripts"] as? [String: Any] else { return [] }
        let runner = runner(in: cwd)
        return serving.filter { scripts[$0] != nil }.map { PackageScript(runner: runner, name: $0) }
    }

    static func runner(in cwd: String) -> String {
        let has = { FileManager.default.fileExists(atPath: (cwd as NSString).appendingPathComponent($0)) }
        if has("pnpm-lock.yaml") { return "pnpm" }
        if has("yarn.lock") { return "yarn" }
        if has("bun.lockb") || has("bun.lock") { return "bun" }
        return "npm"
    }
}

/// Launch-file servers you've let run, per repo. A cloned repo's launch.json names any program
/// and environment, and the menu shows only a name: its command is shown before it first runs,
/// and again after it changes.
@MainActor
enum ServerApproval {
    /// UserDefaults key: the repo's approved configurations, as hashes of all they contain.
    static func key(_ repo: String) -> String { "preview.approved.\(repo)" }

    static func hash(_ c: PreviewConfig) -> String? {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        guard let data = try? encoder.encode(c) else { return nil }
        return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    /// True if `c` may run: approved before as it is, or now.
    static func confirm(_ c: PreviewConfig, repo: String) -> Bool {
        guard let hash = hash(c) else { return false }
        var approved = UserDefaults.standard.stringArray(forKey: key(repo)) ?? []
        if approved.contains(hash) { return true }
        let plain = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_./:=@%+,"))
        let command = c.argv.map { $0.unicodeScalars.allSatisfy(plain.contains) && !$0.isEmpty ? $0 : Opening.quoted($0) }.joined(separator: " ")
        var info = "\(c.source) runs:\n\n\(command)\n\nin \(shortPath(c.cwd))"
        if let env = c.env, !env.isEmpty {
            info += ", setting \(env.keys.sorted().joined(separator: ", "))"
        }
        info += ".\n\nIt runs with the same access to your files as dino. Only start servers you trust; dino asks again if this one changes."
        let alert = NSAlert()
        alert.messageText = "Start “\(c.name)”?"
        alert.informativeText = info
        alert.addButton(withTitle: "Start")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return false }
        approved.append(hash)
        UserDefaults.standard.set(Array(approved.suffix(50)), forKey: key(repo))
        return true
    }
}

// MARK: - Views

struct PreviewPane: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo?

    var body: some View {
        let page = model.webPage(for: session?.id)
        PreviewBody(page: page, session: session)
            .id(ObjectIdentifier(page))
    }
}

private struct PreviewBody: View {
    @EnvironmentObject var model: DinoModel
    @ObservedObject var page: WebPage
    let session: SessionInfo?
    @State private var configs: [PreviewConfig] = []
    @State private var configError: String?
    /// package.json scripts to offer when nothing's set up; read with the launch files.
    @State private var scripts: [PackageScript] = []
    @State private var editing = ""
    @FocusState private var addressFocused: Bool

    private var running: [PreviewInfo] { (session?.previews ?? []).filter(\.running) }

    var body: some View {
        VStack(spacing: 0) {
            SidePaneHeader(
                title: "Preview",
                subtitle: page.title.isEmpty ? session?.display : page.title,
                closeHelp: "Close the preview (⌘W or Esc)",
                close: { model.closeSidePane() }
            ) {
                Image(systemName: "globe.americas")
            } trailing: {
                Group {
                    serverMenu
                    if let u = page.url, u.isFileURL {
                        Button { model.openFile(u.path, session: session?.id, source: true) } label: {
                            Image(systemName: "chevron.left.forwardslash.chevron.right")
                        }
                        .help("Edit the HTML")
                        .accessibilityLabel("Edit the HTML")
                    }
                    Button {
                        if !page.openInBrowser() { NSSound.beep() }
                    } label: { Image(systemName: "safari") }
                        .disabled(page.url == nil)
                        .help("Open in your browser")
                        .accessibilityLabel("Open in your browser")
                }
                .buttonStyle(.borderless)
            }
            Divider()
            bar
            ZStack(alignment: .top) {
                Divider()
                if page.loading, page.progress < 1 {
                    ProgressView(value: page.progress).progressViewStyle(.linear).controlSize(.mini).tint(Brand.green)
                }
            }
            .frame(height: 2)
            ZStack {
                WebViewHost(view: page.view).opacity(page.url == nil ? 0 : 1)
                if let name = page.waitingFor {
                    starting(name)
                } else if let failure = page.failure {
                    problem(failure)
                } else if page.url == nil {
                    start
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            if page.showLog, let session, let name = page.server ?? running.first?.name {
                Divider()
                ServerLog(session: session.id, name: name) { page.showLog = false }
                    .frame(height: 170)
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
        .task(id: session?.id) {
            // The launch files change as you (or the agent) edit them.
            guard let id = session?.id else { return }
            while !Task.isCancelled {
                let (c, e) = await model.previewConfigs(id)
                if c != configs { configs = c }
                if e != configError { configError = e }
                let found = c.isEmpty && session?.host == nil ? (session?.cwd).map(PackageScript.find) ?? [] : []
                if found != scripts { scripts = found }
                try? await Task.sleep(for: .seconds(3))
            }
        }
        .onAppear { if page.url == nil, page.waitingFor == nil { addressFocused = session == nil } }
    }

    private var bar: some View {
        HStack(spacing: 6) {
            Button { page.view.goBack() } label: { Image(systemName: "chevron.left") }
                .disabled(!page.canGoBack).help("Back").accessibilityLabel("Back")
            Button { page.view.goForward() } label: { Image(systemName: "chevron.right") }
                .disabled(!page.canGoForward).help("Forward").accessibilityLabel("Forward")
            Group {
                if page.loading {
                    Button { page.view.stopLoading() } label: { Image(systemName: "xmark") }.help("Stop loading")
                        .accessibilityLabel("Stop loading")
                } else {
                    Button { page.reload() } label: { Image(systemName: "arrow.clockwise") }.help("Reload (⌘R)")
                        .accessibilityLabel("Reload")
                        .keyboardShortcut("r")
                        .disabled(page.url == nil)
                }
            }
            .frame(width: 18)
            addressField
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private var addressField: some View {
        HStack(spacing: 5) {
            Image(systemName: page.url?.isFileURL == true ? "doc" : page.url?.scheme == "https" ? "lock.fill" : "globe")
                .font(.caption).foregroundStyle(.tertiary)
            TextField("Address", text: Binding(
                get: { addressFocused ? editing : page.address },
                set: { editing = $0 }
            ))
            .textFieldStyle(.plain)
            .font(.callout)
            .focused($addressFocused)
            .onSubmit {
                page.go(editing)
                addressFocused = false
            }
            .onChange(of: addressFocused) { _, f in if f { editing = page.address } }
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.primary.opacity(0.06)))
        .help(page.url == nil ? "A port like localhost:3000, a URL or a file's path" : page.title.isEmpty ? (page.url?.absoluteString ?? "") : "\(page.title) — \(page.url?.absoluteString ?? "")")
    }

    private var serverMenu: some View {
        Menu {
            if let session {
                ForEach(configs) { c in
                    let on = running.contains { $0.name == c.name }
                    Button(on ? "Stop \(c.name)" : "Start \(c.name)") {
                        if on { model.stopServer(c.name, page: page) } else { model.startServer(c.name, page: page) }
                    }
                }
                if !configs.isEmpty { Divider() }
                Button(page.showLog ? "Hide Output" : "Show Output") { page.showLog.toggle() }
                    .disabled(page.server == nil && running.isEmpty)
                if let c = configs.first, let cwd = session.cwd {
                    Button("Edit \(c.source)") { model.openFile((cwd as NSString).appendingPathComponent(c.source), session: session.id) }
                } else if let cwd = session.cwd {
                    Button("Add .dino/launch.json…") { model.addLaunchFile(in: cwd) }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Circle().fill(running.isEmpty ? Color.secondary.opacity(0.4) : Brand.green).frame(width: 7, height: 7)
                Image(systemName: "server.rack")
            }
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(session == nil)
        .accessibilityLabel("Dev servers")
        .help(running.isEmpty ? "Dev servers from launch.json" : "Running: \(running.map(\.name).joined(separator: ", "))")
    }

    /// Nothing loaded yet: what could be.
    private var start: some View {
        VStack(spacing: 14) {
            Image(systemName: "globe").font(.system(size: 34, weight: .light)).foregroundStyle(.tertiary)
            Text("Preview").font(.title3.weight(.semibold))
            if let session {
                if let u = session.local_url, let url = URL(string: u) {
                    Button { page.load(url) } label: {
                        Label("Open \(WebPage.display(url))", systemImage: "arrow.up.forward.app").frame(minWidth: 220)
                    }
                    .controlSize(.large)
                    .buttonStyle(.borderedProminent)
                    .tint(Brand.green)
                    .help("\(session.display) printed this address")
                }
                ForEach(configs) { c in
                    Button { model.startServer(c.name, page: page) } label: {
                        VStack(spacing: 1) {
                            Text("Start \(c.name)")
                            Text(c.argv.joined(separator: " ")).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                        }
                        .frame(minWidth: 220)
                    }
                    .controlSize(.large)
                    .help("Runs in \(shortPath(c.cwd)), from \(c.source); stops with the session")
                }
                if let configError {
                    ErrorLine(message: configError)
                } else if configs.isEmpty, let cwd = session.cwd {
                    if scripts.isEmpty {
                        Text("No dev server set up for \((cwd as NSString).lastPathComponent).")
                            .font(.callout).foregroundStyle(.secondary)
                    } else {
                        Text("Serve \((cwd as NSString).lastPathComponent) with a script from its package.json:")
                            .font(.callout).foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                        ForEach(scripts.prefix(3)) { p in
                            Button { model.addLaunchFile(in: cwd, script: p) } label: {
                                Text(p.command).font(.body.monospaced()).frame(minWidth: 220)
                            }
                            .controlSize(.large)
                            .help("Saves it to .dino/launch.json; you see the command once more before it first runs")
                        }
                    }
                    Button(scripts.isEmpty ? "Add .dino/launch.json…" : "Write .dino/launch.json by Hand…") { model.addLaunchFile(in: cwd) }
                        .buttonStyle(.link)
                        .help("dino also reads .claude/launch.json")
                }
            }
            Text("Or type an address above, or ⌘-click a link in the terminal.")
                .font(.caption).foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(30)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
    }

    private func starting(_ name: String) -> some View {
        VStack(spacing: 12) {
            ProgressView().controlSize(.regular)
            Text("Starting \(name)…").foregroundStyle(.secondary)
            HStack {
                Button(page.showLog ? "Hide Output" : "Show Output") { page.showLog.toggle() }
                Button("Stop") { model.stopServer(name, page: page) }
            }
            .controlSize(.small)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
    }

    private func problem(_ text: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle").font(.system(size: 28, weight: .light)).foregroundStyle(SessionStatus.needsYou.color)
            Text(text).multilineTextAlignment(.center).foregroundStyle(.secondary).frame(maxWidth: 360)
            HStack {
                Button("Try Again") { page.reload() }.disabled(page.url == nil)
                if page.server != nil { Button(page.showLog ? "Hide Output" : "Show Output") { page.showLog.toggle() } }
            }
            .controlSize(.small)
        }
        .padding(30)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
    }
}

/// The page's own WKWebView, kept across session switches.
private struct WebViewHost: NSViewRepresentable {
    let view: WKWebView

    func makeNSView(context _: Context) -> NSView {
        let box = NSView()
        view.frame = box.bounds
        view.autoresizingMask = [.width, .height]
        box.addSubview(view)
        return box
    }

    func updateNSView(_ box: NSView, context _: Context) {
        if view.superview !== box {
            view.removeFromSuperview()
            view.frame = box.bounds
            box.addSubview(view)
        }
    }
}

/// A dev server's output, following along.
private struct ServerLog: View {
    let session: String
    let name: String
    let close: () -> Void
    @State private var text = ""

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("\(name) output").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                Spacer()
                Button("Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(text, forType: .string)
                }
                .disabled(text.isEmpty)
                Button { close() } label: { Image(systemName: "xmark") }.help("Hide output")
            }
            .buttonStyle(.borderless)
            .controlSize(.small)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            ScrollViewReader { proxy in
                ScrollView {
                    Text(text.isEmpty ? "No output yet" : String(text.suffix(60000)))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(text.isEmpty ? .tertiary : .primary)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 10)
                        .padding(.bottom, 6)
                    Color.clear.frame(height: 1).id("end")
                }
                .onChange(of: text) { proxy.scrollTo("end", anchor: .bottom) }
            }
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .task(id: "\(session)/\(name)") {
            text = ""
            while !Task.isCancelled {
                let session = session, name = name
                let t = await Task.detached {
                    try? DinoConnection(path: DinoEnvironment.socketPath).previewLog(session: session, name: name)
                }.value
                if let t, t != text { text = t }
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }
}

/// "localhost:5173 · Open Preview", over a terminal that just printed a local address.
struct PreviewOffer: View {
    @EnvironmentObject var model: DinoModel
    let session: SessionInfo
    let url: String

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "globe").foregroundStyle(Brand.green)
            Text(URL(string: url).map(WebPage.display) ?? url).font(.callout.monospaced()).lineLimit(1)
            Button("Open Preview") { model.openPreview(session: session.id, url: URL(string: url)) }
                .buttonStyle(.borderedProminent)
                .tint(Brand.green)
                .controlSize(.small)
            Button { model.offered[session.id] = url } label: { Image(systemName: "xmark").font(.caption) }
                .buttonStyle(.borderless)
                .help("Dismiss")
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
