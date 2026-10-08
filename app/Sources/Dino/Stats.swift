import Charts
import SwiftUI

// MARK: - The report, as dinod sends it (`dino stats --json`; see crates/dino-core/src/usage/report.rs)

/// A field's name as dinod spells it.
private struct StatsKey: CodingKey, ExpressibleByStringLiteral {
    var stringValue: String
    var intValue: Int? { nil }
    init(stringValue: String) { self.stringValue = stringValue }
    init?(intValue _: Int) { nil }
    init(stringLiteral value: String) { stringValue = value }
}

/// Every field has a default: a dinod older or newer than the app still shows what it can.
private extension KeyedDecodingContainer where K == StatsKey {
    func v<T: Decodable>(_ key: Key, _ fallback: T) -> T {
        ((try? decodeIfPresent(T.self, forKey: key)) ?? nil) ?? fallback
    }
}

struct StatsTokens: Decodable, Equatable {
    var input: UInt64 = 0, cache_read: UInt64 = 0, cache_write: UInt64 = 0, output: UInt64 = 0, total: UInt64 = 0
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        input = c.v("input", 0); cache_read = c.v("cache_read", 0); cache_write = c.v("cache_write", 0)
        output = c.v("output", 0); total = c.v("total", 0)
    }
}

/// One part of the tokens: what it used, and in how many calls.
struct StatsShare: Decodable, Equatable {
    var tokens = StatsTokens(), requests: UInt64 = 0
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
    }
}

/// Where the tokens went: the conversation itself, its subagents (Claude's Agent tool), and side
/// requests (calls an agent makes that its transcript doesn't keep). Nil from an older dinod.
struct StatsParts: Decodable, Equatable {
    var main = StatsShare(), subagents = StatsShare(), side = StatsShare()
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        main = c.v("main", StatsShare()); subagents = c.v("subagents", StatsShare()); side = c.v("side", StatsShare())
    }

    var total: UInt64 { main.tokens.total + subagents.tokens.total + side.tokens.total }
    var isEmpty: Bool { total == 0 }
}

struct StatsDay: Decodable, Equatable {
    var date = "", tokens: UInt64 = 0, requests: UInt64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        date = c.v("date", ""); tokens = c.v("tokens", 0); requests = c.v("requests", 0)
    }
}

struct StatsLongestSession: Decodable, Equatable {
    var agent = "", conversation: String?, cwd: String?, ms: Int64 = 0, started_ms: Int64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        agent = c.v("agent", ""); conversation = c.v("conversation", nil); cwd = c.v("cwd", nil)
        ms = c.v("ms", 0); started_ms = c.v("started_ms", 0)
    }
}

struct StatsTotals: Decodable, Equatable {
    var tokens = StatsTokens(), requests: UInt64 = 0, errors: UInt64 = 0, limit_hits: UInt64 = 0, sessions: UInt64 = 0
    var active_days: UInt64 = 0, days: UInt64 = 0
    var favorite_model: String?, longest_session: StatsLongestSession?, peak_hour: Int?, most_active_day: StatsDay?
    var cost: Double?, priced_requests: UInt64 = 0
    var parts: StatsParts?
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0); errors = c.v("errors", 0)
        limit_hits = c.v("limit_hits", 0); sessions = c.v("sessions", 0); active_days = c.v("active_days", 0); days = c.v("days", 0)
        favorite_model = c.v("favorite_model", nil); longest_session = c.v("longest_session", nil)
        peak_hour = c.v("peak_hour", nil); most_active_day = c.v("most_active_day", nil)
        cost = c.v("cost", nil); priced_requests = c.v("priced_requests", 0)
        parts = c.v("parts", nil)
    }
}

struct StatsStreaks: Decodable, Equatable {
    var current: UInt64 = 0, longest: UInt64 = 0, longest_from: String?
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        current = c.v("current", 0); longest = c.v("longest", 0); longest_from = c.v("longest_from", nil)
    }
}

struct StatsPeriod: Decodable, Equatable {
    var tokens: UInt64 = 0, requests: UInt64 = 0, sessions: UInt64 = 0
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        tokens = c.v("tokens", 0); requests = c.v("requests", 0); sessions = c.v("sessions", 0)
    }
}

struct StatsPeriods: Decodable, Equatable {
    var today = StatsPeriod(), week = StatsPeriod(), month = StatsPeriod()
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        today = c.v("today", StatsPeriod()); week = c.v("week", StatsPeriod()); month = c.v("month", StatsPeriod())
    }
}

struct StatsHour: Decodable, Equatable {
    var hour = 0, requests: UInt64 = 0, tokens: UInt64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        hour = c.v("hour", 0); requests = c.v("requests", 0); tokens = c.v("tokens", 0)
    }
}

struct StatsModel: Decodable, Equatable {
    var model = "", tokens = StatsTokens(), requests: UInt64 = 0, share = 0.0, agents: [String] = []
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        model = c.v("model", ""); tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
        share = c.v("share", 0); agents = c.v("agents", [])
    }
}

struct StatsDailyModel: Decodable, Equatable {
    var date = "", model = "", tokens: UInt64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        date = c.v("date", ""); model = c.v("model", ""); tokens = c.v("tokens", 0)
    }
}

struct StatsAgent: Decodable, Equatable {
    var agent = "", tokens = StatsTokens(), requests: UInt64 = 0, sessions: UInt64 = 0, active_days: UInt64 = 0
    var proxied: UInt64 = 0, recorded: UInt64 = 0, last_ms: Int64 = 0, top_model: String?, undated: UInt64 = 0
    var parts: StatsParts?
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        agent = c.v("agent", ""); tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
        sessions = c.v("sessions", 0); active_days = c.v("active_days", 0); proxied = c.v("proxied", 0)
        recorded = c.v("recorded", 0); last_ms = c.v("last_ms", 0); top_model = c.v("top_model", nil)
        undated = c.v("undated", 0); parts = c.v("parts", nil)
    }
}

struct StatsProject: Decodable, Equatable {
    var name = "", path = "", tokens = StatsTokens(), requests: UInt64 = 0, sessions: UInt64 = 0, agents: [String] = [], last_ms: Int64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        name = c.v("name", ""); path = c.v("path", ""); tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
        sessions = c.v("sessions", 0); agents = c.v("agents", []); last_ms = c.v("last_ms", 0)
    }
}

/// One of the sessions that used the most: a conversation, or a dino session whose conversation
/// isn't known. Its tokens split as the totals are.
struct StatsSession: Decodable, Equatable {
    var agent = "", conversation: String?, session: String?, cwd: String?
    var first_ms: Int64 = 0, last_ms: Int64 = 0, tokens = StatsTokens(), requests: UInt64 = 0, parts: StatsParts?
    /// The model that answered most of its tokens, and how many subagents it ran; nil and 0 from
    /// an older dinod.
    var model: String?, subagents: UInt64 = 0
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        agent = c.v("agent", ""); conversation = c.v("conversation", nil); session = c.v("session", nil); cwd = c.v("cwd", nil)
        first_ms = c.v("first_ms", 0); last_ms = c.v("last_ms", 0); tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
        parts = c.v("parts", nil); model = c.v("model", nil); subagents = c.v("subagents", 0)
    }

    var id: String { (session ?? "") + "\u{0}" + (conversation ?? "") + "\u{0}" + agent }
    var project: String { cwd.map(StatsFormat.name) ?? "" }
}

struct StatsRouteWindow: Decodable, Equatable {
    var name = "", used = 0.0, resets_at: UInt64?
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        name = c.v("name", ""); used = c.v("used", 0); resets_at = c.v("resets_at", nil)
    }
}

struct StatsRoute: Decodable, Equatable {
    var route = "", label = "", kind = "", tokens = StatsTokens(), requests: UInt64 = 0, errors: UInt64 = 0
    var limit_hits: UInt64 = 0, last_limit_ms: Int64?, cost: Double?, models: [String] = [], windows: [StatsRouteWindow] = []
    var fallbacks: UInt64 = 0, account_spend: Double?, account_limit: Double?
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        route = c.v("route", ""); label = c.v("label", ""); kind = c.v("kind", ""); tokens = c.v("tokens", StatsTokens())
        requests = c.v("requests", 0); errors = c.v("errors", 0); limit_hits = c.v("limit_hits", 0)
        last_limit_ms = c.v("last_limit_ms", nil); cost = c.v("cost", nil); models = c.v("models", [])
        windows = c.v("windows", []); fallbacks = c.v("fallbacks", 0)
        account_spend = c.v("account_spend", nil); account_limit = c.v("account_limit", nil)
    }
}

struct StatsSpeed: Decodable, Equatable {
    var route = "", routeLabel = "", model = "", requests: UInt64 = 0
    var ttft_p50_ms: Int?, ttft_p90_ms: Int?, tps_p50: Double?, tps_p90: Double?
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        route = c.v("route", ""); routeLabel = c.v("label", ""); model = c.v("model", ""); requests = c.v("requests", 0)
        ttft_p50_ms = c.v("ttft_p50_ms", nil); ttft_p90_ms = c.v("ttft_p90_ms", nil)
        tps_p50 = c.v("tps_p50", nil); tps_p90 = c.v("tps_p90", nil)
    }
}

struct StatsSources: Decodable, Equatable {
    var proxied: UInt64 = 0, recorded: UInt64 = 0, deduplicated: UInt64 = 0
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        proxied = c.v("proxied", 0); recorded = c.v("recorded", 0); deduplicated = c.v("deduplicated", 0)
    }
}

struct StatsReport: Decodable, Equatable {
    var schema = 0, range = "30d", from_ms: Int64 = 0, to_ms: Int64 = 0, utc_offset: Int64 = 0
    var totals = StatsTotals(), streaks = StatsStreaks(), periods = StatsPeriods()
    var heatmap: [StatsDay] = [], days: [StatsDay] = [], hours: [StatsHour] = []
    var models: [StatsModel] = [], daily_models: [StatsDailyModel] = [], agents: [StatsAgent] = []
    var projects: [StatsProject] = [], routes: [StatsRoute] = [], speed: [StatsSpeed] = [], sources = StatsSources()
    var sessions: [StatsSession] = []
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        schema = c.v("schema", 0); range = c.v("range", "30d"); from_ms = c.v("from_ms", 0); to_ms = c.v("to_ms", 0)
        utc_offset = c.v("utc_offset", 0); totals = c.v("totals", StatsTotals()); streaks = c.v("streaks", StatsStreaks())
        periods = c.v("periods", StatsPeriods()); heatmap = c.v("heatmap", []); days = c.v("days", []); hours = c.v("hours", [])
        models = c.v("models", []); daily_models = c.v("daily_models", []); agents = c.v("agents", [])
        projects = c.v("projects", []); routes = c.v("routes", []); speed = c.v("speed", []); sources = c.v("sources", StatsSources())
        sessions = c.v("sessions", [])
    }

    var isEmpty: Bool { totals.requests == 0 }
}

// MARK: - Loading

/// The Stats window's report: asked for when the window opens, the range changes, or on refresh;
/// nothing runs while it's closed or left alone.
@MainActor
final class StatsStore: ObservableObject {
    @Published private(set) var report: StatsReport?
    @Published private(set) var loading = false
    @Published private(set) var error: String?
    private var asked = 0

    func load(_ range: String) {
        asked += 1
        let ticket = asked
        if !loading { loading = true }
        Task {
            let result: Result<StatsReport, Error> = await Task.detached {
                Result { try DinoConnection(path: DinoEnvironment.socketPath).stats(range: range) }
            }.value
            // A newer ask (another range) answers instead.
            guard ticket == asked else { return }
            if loading { loading = false }
            switch result {
            case .success(let r):
                if r != report { report = r }
                if error != nil { error = nil }
            case .failure(let e):
                let msg = e.localizedDescription
                if msg != error { error = msg }
            }
        }
    }

    /// Shows `r` as if dinod had sent it (renders of the window).
    func preview(_ r: StatsReport) {
        report = r
        loading = false
    }

    func clear(then range: String) {
        Task {
            let result: Result<Void, Error> = await Task.detached {
                Result { try DinoConnection(path: DinoEnvironment.socketPath).clearStats() }
            }.value
            if case .failure(let e) = result, e.localizedDescription != error { error = e.localizedDescription }
            load(range)
        }
    }
}

// MARK: - The window

enum StatsPane: String, CaseIterable, Identifiable {
    case overview, sessions, models, agents, projects, routes, speed
    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .sessions: "Sessions"
        case .models: "Models"
        case .agents: "Agents"
        case .projects: "Projects"
        case .routes: "Routes"
        case .speed: "Speed"
        }
    }

    var symbol: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .sessions: "text.bubble"
        case .models: "cpu"
        case .agents: "person.2"
        case .projects: "folder"
        case .routes: "arrow.triangle.branch"
        case .speed: "speedometer"
        }
    }
}

struct StatsView: View {
    static let windowID = "stats"

    @StateObject private var store: StatsStore
    @AppStorage("stats.range") private var range = "30d"
    @AppStorage("stats.pane") private var pane: StatsPane = .overview
    @State private var confirmClear = false

    /// `store`: one already holding a report (renders of the window); else the window asks dinod.
    @MainActor init(store: StatsStore? = nil) {
        _store = StateObject(wrappedValue: store ?? StatsStore())
    }

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            List(selection: Binding(get: { pane }, set: { if let p = $0 { pane = p } })) {
                ForEach(StatsPane.allCases) { p in
                    Label(p.title, systemImage: p.symbol).tag(p)
                }
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 180, max: 180)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(minWidth: 760, minHeight: 560)
                .navigationTitle(pane.title)
                .navigationSubtitle(subtitle)
                .toolbar {
                    ToolbarItemGroup(placement: .primaryAction) {
                        Picker("Range", selection: $range) {
                            Text("7 days").tag("7d")
                            Text("30 days").tag("30d")
                            Text("All").tag("all")
                        }
                        .pickerStyle(.segmented)
                        .help("Which days the numbers cover")
                        Button { store.load(range) } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                            .help("Load the latest usage")
                            .disabled(store.loading)
                        Menu {
                            Button("Clear Stats…", role: .destructive) { confirmClear = true }
                        } label: { Label("More", systemImage: "ellipsis.circle") }
                            .menuIndicator(.hidden)
                            .help("More")
                    }
                }
        }
        .onAppear { if store.report == nil { store.load(range) } }
        .onChange(of: range) { _, r in store.load(r) }
        .confirmationDialog("Clear usage stats?", isPresented: $confirmClear) {
            Button("Clear Stats", role: .destructive) { store.clear(then: range) }
        } message: {
            Text("The route, speed and limit data dino recorded is deleted permanently. Usage from the agents' own history is read again the next time you open Usage Stats.")
        }
    }

    /// The days the numbers cover, under the title.
    private var subtitle: String {
        guard let r = store.report, r.from_ms > 0 else { return "" }
        let from = Date(timeIntervalSince1970: Double(r.from_ms) / 1000)
        let to = Date(timeIntervalSince1970: Double(r.to_ms) / 1000)
        return "\(from.formatted(.dateTime.month(.abbreviated).day())) – \(to.formatted(.dateTime.month(.abbreviated).day().year()))"
    }

    @ViewBuilder private var detail: some View {
        if let r = store.report {
            GeometryReader { geo in
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        if let e = store.error { errorLine(e) }
                        if r.isEmpty && pane != .routes {
                            empty.frame(minHeight: geo.size.height - 120)
                        } else {
                            let size = StatsSize(width: min(geo.size.width, 1440) - 56, height: geo.size.height)
                            switch pane {
                            case .overview: OverviewPane(r: r, size: size)
                            case .sessions: SessionsPane(r: r)
                            case .models: ModelsPane(r: r, size: size)
                            case .agents: AgentsPane(r: r, size: size)
                            case .projects: ProjectsPane(r: r, size: size)
                            case .routes: RoutesPane(r: r, size: size)
                            case .speed: SpeedPane(r: r, size: size)
                            }
                        }
                        SourcesNote(s: r.sources)
                    }
                    .padding(.horizontal, 28)
                    .padding(.vertical, 22)
                    .frame(maxWidth: 1440, alignment: .leading)
                    .frame(maxWidth: .infinity)
                }
            }
            .overlay(alignment: .topTrailing) {
                if store.loading { ProgressView().controlSize(.small).padding(12) }
            }
        } else if let e = store.error {
            ContentUnavailableView {
                Label("Stats aren't available", systemImage: "exclamationmark.triangle")
            } description: {
                Text(e)
            } actions: {
                Button("Try Again") { store.load(range) }
            }
        } else {
            VStack(spacing: 10) {
                ProgressView()
                Text("Reading usage…").font(.callout).foregroundStyle(.secondary)
                Text("The first time takes longer: dino reads each agent's full history. After that, it reads only what's new.")
                    .font(.caption).foregroundStyle(.tertiary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var empty: some View {
        ContentUnavailableView {
            Label("No usage in this range", systemImage: "chart.bar")
        } description: {
            Text(range == "all"
                ? "Start an agent in dino, or keep using the ones you have. dino reads usage from the history Claude Code, Codex, OpenCode and other agents keep."
                : "No usage in the last \(range == "7d" ? "7" : "30") days. Choose All to see earlier usage.")
        } actions: {
            if range != "all" { Button("Show All") { range = "all" } }
        }
        .frame(maxWidth: .infinity)
    }

    private func errorLine(_ e: String) -> some View {
        Label(e, systemImage: "exclamationmark.triangle").font(.caption).foregroundStyle(.secondary)
    }
}

/// The room a page has: charts take their size from it, and wide windows get two columns.
struct StatsSize {
    let width: CGFloat
    let height: CGFloat
    var wide: Bool { width >= 880 }
    /// A main chart's height: about a third of the window, within reason.
    var chart: CGFloat { min(max(height * 0.3, 200), 340) }
    /// Two columns side by side, with the gap between them.
    var half: CGFloat { wide ? (width - 16) / 2 : width }
}

// MARK: - Overview

/// The three places tokens go, with their colors: identity, so the same in every chart.
enum StatsPart: String, CaseIterable, Identifiable {
    case main, subagents, side
    var id: String { rawValue }

    var title: String {
        switch self {
        case .main: "Conversations"
        case .subagents: "Subagents"
        case .side: "Side requests"
        }
    }

    var color: Color { ModelColors.slot(self == .main ? 0 : self == .subagents ? 1 : 6) }

    func of(_ p: StatsParts) -> UInt64 { part(p).tokens.total }

    func part(_ p: StatsParts) -> StatsShare {
        switch self {
        case .main: p.main
        case .subagents: p.subagents
        case .side: p.side
        }
    }

    static let sideNote = "Side requests are calls Claude Code makes that its transcript doesn't keep (titles, compaction, auto-mode checks), only visible for sessions dino carried."
}

private struct OverviewPane: View {
    let r: StatsReport
    let size: StatsSize

    var body: some View {
        let t = r.totals
        VStack(alignment: .leading, spacing: 16) {
            // The headline: tokens, and where they went.
            Columns(size: size, ratio: 0.5) {
                TokensCard(r: r)
            } trailing: {
                LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 12) {
                    Tile(title: "Requests", value: count(t.requests),
                         detail: t.errors + t.limit_hits > 0 ? "\(count(t.errors)) errors · \(count(t.limit_hits)) limit hits" : "no errors")
                    Tile(title: "Sessions", value: count(t.sessions), detail: "across every agent")
                    Tile(title: "Active days", value: "\(t.active_days)", detail: "of \(t.days) · streak \(days(r.streaks.current))")
                    if let cost = t.cost {
                        Tile(title: "Reported cost", value: StatsFormat.money(cost), detail: "\(count(t.priced_requests)) priced requests")
                    } else {
                        Tile(title: "Longest streak", value: days(r.streaks.longest),
                             detail: r.streaks.longest_from.map { "from \(StatsFormat.day($0, style: .abbreviated))" } ?? "")
                    }
                }
            }
            PeriodStrip(p: r.periods)
            StatsCard(title: "Tokens per day", note: r.totals.parts == nil ? nil : "conversations, subagents and side requests together") {
                DailyChart(days: r.days, height: size.chart)
            }
            StatsCard(title: "Activity", note: "the last 12 months, whatever the range") {
                Heatmap(days: r.heatmap, width: size.width - 32)
            }
            Columns(size: size, ratio: 0.58) {
                StatsCard(title: "Requests by hour", note: "local time") { HoursChart(hours: r.hours) }
            } trailing: {
                StatsCard(title: "Highlights") { Highlights(r: r) }
            }
        }
    }

    private func days(_ n: UInt64) -> String { n == 1 ? "1 day" : "\(n) days" }
}

/// Two blocks side by side in a wide window, one above the other in a narrow one.
private struct Columns<Leading: View, Trailing: View>: View {
    let size: StatsSize
    /// The leading block's share of the width.
    var ratio: CGFloat = 0.5
    @ViewBuilder let leading: Leading
    @ViewBuilder let trailing: Trailing

    var body: some View {
        if size.wide {
            HStack(alignment: .top, spacing: 16) {
                leading.frame(width: (size.width - 16) * ratio)
                trailing.frame(maxWidth: .infinity)
            }
        } else {
            VStack(alignment: .leading, spacing: 16) {
                leading
                trailing
            }
        }
    }
}

/// The hero: every token in the range, and the split between the conversation, its subagents and
/// side requests, which is where most tokens go.
private struct TokensCard: View {
    let r: StatsReport

    var body: some View {
        let t = r.totals.tokens
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Tokens").font(.callout).foregroundStyle(.secondary)
                Text(tokens(t.total)).font(.system(size: 44, weight: .semibold))
                Text("\(tokens(t.input)) in · \(tokens(t.output)) out · \(tokens(t.cache_read + t.cache_write)) cache")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let parts = r.totals.parts, !parts.isEmpty {
                PartsBar(parts: parts, height: 12)
                VStack(spacing: 6) {
                    ForEach(StatsPart.allCases) { p in
                        HStack(spacing: 8) {
                            RoundedRectangle(cornerRadius: 2).fill(p.color).frame(width: 10, height: 10)
                            Text(p.title)
                            if p == .side {
                                Image(systemName: "info.circle").font(.caption).foregroundStyle(.tertiary).help(StatsPart.sideNote)
                            }
                            Spacer(minLength: 8)
                            Text(tokens(p.of(parts))).monospacedDigit()
                            Text(StatsFormat.percent(Double(p.of(parts)) / Double(max(parts.total, 1))))
                                .foregroundStyle(.secondary).monospacedDigit().frame(width: 40, alignment: .trailing)
                        }
                        .font(.callout)
                        .fontWeight(p == .subagents ? .semibold : .regular)
                        .help("\(count(p.part(parts).requests)) requests")
                    }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .modifier(CardBackground())
        .accessibilityElement(children: .combine)
    }
}

/// A part split as one bar: each part its own segment, a 2 pt gap between.
struct PartsBar: View {
    let parts: StatsParts
    var height: CGFloat = 8

    var body: some View {
        GeometryReader { geo in
            let shown = StatsPart.allCases.filter { $0.of(parts) > 0 }
            let gaps = CGFloat(max(shown.count - 1, 0)) * 2
            HStack(spacing: 2) {
                ForEach(shown) { p in
                    RoundedRectangle(cornerRadius: min(4, height / 2), style: .continuous)
                        .fill(p.color)
                        .frame(width: max(2, (geo.size.width - gaps) * CGFloat(p.of(parts)) / CGFloat(max(parts.total, 1))))
                }
            }
        }
        .frame(height: height)
        .accessibilityElement()
        .accessibilityLabel(StatsPart.allCases.map { "\($0.title) \(tokens($0.of(parts)))" }.joined(separator: ", "))
    }
}

private struct Tile: View {
    let title: String
    let value: String
    var detail = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.callout).foregroundStyle(.secondary)
            Text(value).font(.title.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.6)
            Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, minHeight: 76, alignment: .leading)
        .padding(14)
        .modifier(CardBackground())
        .accessibilityElement(children: .combine)
    }
}

/// A card's surface: one step off the window's, with a hairline edge.
struct CardBackground: ViewModifier {
    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.primary.opacity(0.035)))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Color.primary.opacity(0.08), lineWidth: 1))
    }
}

private struct PeriodStrip: View {
    let p: StatsPeriods

    var body: some View {
        HStack(spacing: 0) {
            cell("Today", p.today)
            Divider().frame(height: 38)
            cell("Last 7 days", p.week)
            Divider().frame(height: 38)
            cell("Last 30 days", p.month)
        }
        .padding(.vertical, 12)
        .modifier(CardBackground())
    }

    private func cell(_ title: String, _ p: StatsPeriod) -> some View {
        VStack(spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text("\(tokens(p.tokens)) tokens").font(.title3.weight(.medium))
            Text("\(count(p.requests)) requests · \(count(p.sessions)) sessions").font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }
}

/// A titled card on the page.
struct StatsCard<Content: View>: View {
    let title: String
    var note: String?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title).font(.headline)
                if let note { Text(note).font(.caption).foregroundStyle(.secondary) }
            }
            content
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .modifier(CardBackground())
    }
}

/// Tokens each day, one series: the title names it, so no legend. Hover shows the day.
private struct DailyChart: View {
    let days: [StatsDay]
    let height: CGFloat
    @State private var picked: Date?

    var body: some View {
        let points = days.compactMap { d in StatsFormat.date(d.date).map { ($0, d) } }
        Chart {
            ForEach(points, id: \.1.date) { date, d in
                BarMark(x: .value("Day", date, unit: .day), y: .value("Tokens", Double(d.tokens)), width: .ratio(0.7))
                    .foregroundStyle(Brand.green.opacity(picked == nil || Calendar.current.isDate(date, inSameDayAs: picked!) ? 1 : 0.45))
                    .clipShape(UnevenRoundedRectangle(topLeadingRadius: 3, topTrailingRadius: 3))
            }
            if let picked, let d = points.first(where: { Calendar.current.isDate($0.0, inSameDayAs: picked) })?.1 {
                RuleMark(x: .value("Day", picked, unit: .day))
                    .foregroundStyle(.clear)
                    .annotation(position: .top, overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                        Tip(title: StatsFormat.day(d.date, style: .abbreviated), lines: [("Tokens", tokens(d.tokens)), ("Requests", count(d.requests))])
                    }
            }
        }
        .chartXSelection(value: $picked)
        .chartYAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(tokens(UInt64(n))) } } } }
        .chartXAxis { AxisMarks(values: .stride(by: .day, count: max(1, points.count / 8))) { _ in AxisValueLabel(format: .dateTime.month(.abbreviated).day()) } }
        .frame(height: height)
        .accessibilityLabel("Tokens per day")
    }
}

/// A chart's hover card.
private struct Tip: View {
    let title: String
    let lines: [(String, String)]

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.caption.weight(.semibold))
            ForEach(lines, id: \.0) { l in
                HStack {
                    Text(l.0).foregroundStyle(.secondary)
                    Spacer(minLength: 12)
                    Text(l.1).monospacedDigit()
                }
                .font(.caption)
            }
        }
        .padding(8)
        .frame(minWidth: 150)
        .background(RoundedRectangle(cornerRadius: 6).fill(.background).shadow(color: .black.opacity(0.18), radius: 4, y: 1))
    }
}

/// The facts that are one value each.
private struct Highlights: View {
    let r: StatsReport

    var body: some View {
        let t = r.totals
        VStack(spacing: 0) {
            row("Favorite model", t.favorite_model.map { shortModel($0) } ?? "–", "most tokens")
            Divider()
            row("Longest session", t.longest_session.map { StatsFormat.duration($0.ms) } ?? "–",
                t.longest_session.map { AgentNames.of($0.agent) + ($0.cwd.map { " · " + StatsFormat.name($0) } ?? "") } ?? "")
            Divider()
            row("Peak hour", t.peak_hour.map(StatsFormat.hour) ?? "–", "most requests")
            Divider()
            row("Most active day", t.most_active_day.map { StatsFormat.day($0.date, style: .abbreviated) } ?? "–",
                t.most_active_day.map { "\(tokens($0.tokens)) tokens" } ?? "")
            Divider()
            row("Longest streak", r.streaks.longest == 1 ? "1 day" : "\(r.streaks.longest) days",
                r.streaks.longest_from.map { "from \(StatsFormat.day($0, style: .abbreviated))" } ?? "")
        }
    }

    private func row(_ title: String, _ value: String, _ detail: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title).foregroundStyle(.secondary)
            Spacer(minLength: 12)
            VStack(alignment: .trailing, spacing: 1) {
                Text(value).fontWeight(.medium).lineLimit(1)
                if !detail.isEmpty { Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
            }
        }
        .font(.callout)
        .padding(.vertical, 7)
        .accessibilityElement(children: .combine)
    }
}

/// Weeks as columns, weekdays as rows, one green from faint to full by how many tokens a day had.
/// The cells grow to fill the card's width.
private struct Heatmap: View {
    let days: [StatsDay]
    let width: CGFloat
    private let gap: CGFloat = 3

    var body: some View {
        let weeks = Self.weeks(days)
        let cuts = Self.cuts(days)
        let cell = max(9, min(18, (width - 28 - gap * CGFloat(max(weeks.count - 1, 0))) / CGFloat(max(weeks.count, 1))))
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 6) {
                // The weekday rows, labelled every other one.
                VStack(alignment: .trailing, spacing: gap) {
                    Color.clear.frame(height: 12)
                    ForEach(0..<7, id: \.self) { row in
                        Text(row % 2 == 1 ? Self.weekday(row) : "").font(.caption2).foregroundStyle(.secondary)
                            .frame(height: cell)
                    }
                }
                .frame(width: 28)
                VStack(alignment: .leading, spacing: gap) {
                    HStack(spacing: gap) {
                        ForEach(weeks.indices, id: \.self) { i in
                            Text(Self.monthLabel(weeks, i)).font(.caption2).foregroundStyle(.secondary)
                                .fixedSize().frame(width: cell, height: 12, alignment: .leading)
                        }
                    }
                    HStack(alignment: .top, spacing: gap) {
                        ForEach(weeks.indices, id: \.self) { i in
                            VStack(spacing: gap) {
                                ForEach(0..<7, id: \.self) { row in
                                    if let d = weeks[i][row] {
                                        RoundedRectangle(cornerRadius: 2)
                                            .fill(Self.color(level: Self.level(d.tokens, cuts)))
                                            .frame(width: cell, height: cell)
                                            .help("\(StatsFormat.day(d.date, style: .complete)): \(tokens(d.tokens)) tokens, \(count(d.requests)) requests")
                                    } else {
                                        Color.clear.frame(width: cell, height: cell)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            HStack(spacing: 4) {
                Text("\(days.filter { $0.requests > 0 }.count) active days").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Text("Less").font(.caption2).foregroundStyle(.secondary)
                ForEach(0..<5, id: \.self) { l in
                    RoundedRectangle(cornerRadius: 2).fill(Self.color(level: l)).frame(width: 10, height: 10)
                }
                Text("More").font(.caption2).foregroundStyle(.secondary)
            }
            .accessibilityHidden(true)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Activity heatmap")
        .accessibilityValue("\(days.filter { $0.requests > 0 }.count) active days in the last year")
    }

    static func weekday(_ row: Int) -> String {
        let cal = Calendar.current
        return cal.shortWeekdaySymbols[(cal.firstWeekday - 1 + row) % 7]
    }

    /// Columns of 7 (the locale's first weekday on top); nil where the year starts or ends mid-week.
    static func weeks(_ days: [StatsDay]) -> [[StatsDay?]] {
        guard let first = days.first, let start = StatsFormat.date(first.date) else { return [] }
        let cal = Calendar.current
        let lead = (cal.component(.weekday, from: start) - cal.firstWeekday + 7) % 7
        var cells: [StatsDay?] = Array(repeating: nil, count: lead) + days.map { Optional($0) }
        while cells.count % 7 != 0 { cells.append(nil) }
        return stride(from: 0, to: cells.count, by: 7).map { Array(cells[$0..<$0 + 7]) }
    }

    /// The month's name over the first week that starts it.
    static func monthLabel(_ weeks: [[StatsDay?]], _ i: Int) -> String {
        guard let d = weeks[i].compactMap({ $0 }).first, let date = StatsFormat.date(d.date) else { return "" }
        let month = Calendar.current.component(.month, from: date)
        if i > 0, let p = weeks[i - 1].compactMap({ $0 }).first, let pd = StatsFormat.date(p.date),
           Calendar.current.component(.month, from: pd) == month { return "" }
        // Too close to the edge for a whole label.
        if i >= weeks.count - 2 { return "" }
        // A month's stub before the next one starts: its label would run into the next.
        for j in (i + 1)..<min(i + 3, weeks.count) {
            if let n = weeks[j].compactMap({ $0 }).first, let nd = StatsFormat.date(n.date),
               Calendar.current.component(.month, from: nd) != month { return "" }
        }
        return date.formatted(.dateTime.month(.abbreviated))
    }

    /// Token counts splitting active days into four even groups.
    static func cuts(_ days: [StatsDay]) -> [UInt64] {
        let v = days.map(\.tokens).filter { $0 > 0 }.sorted()
        guard !v.isEmpty else { return [1, 1, 1] }
        return [0.25, 0.5, 0.75].map { v[min(v.count - 1, Int(Double(v.count - 1) * $0))] }
    }

    static func level(_ n: UInt64, _ cuts: [UInt64]) -> Int {
        guard n > 0 else { return 0 }
        return 1 + cuts.filter { n > $0 }.count
    }

    static func color(level: Int) -> Color {
        switch level {
        case 0: Color.primary.opacity(0.07)
        case 1: Brand.green.opacity(0.3)
        case 2: Brand.green.opacity(0.5)
        case 3: Brand.green.opacity(0.75)
        default: Brand.green
        }
    }
}

private struct HoursChart: View {
    let hours: [StatsHour]

    var body: some View {
        // Hours as categories, so each gets its own band (a width ratio means nothing on a
        // continuous scale, where the bars came out zero wide).
        Chart(hours, id: \.hour) { h in
            BarMark(x: .value("Hour", StatsFormat.hour(Int(h.hour))), y: .value("Requests", Double(h.requests)), width: .ratio(0.7))
                .foregroundStyle(Brand.green)
                .clipShape(UnevenRoundedRectangle(topLeadingRadius: 3, topTrailingRadius: 3))
        }
        .chartXScale(domain: (0..<24).map { StatsFormat.hour($0) })
        .chartXAxis {
            AxisMarks(values: stride(from: 0, to: 24, by: 3).map { StatsFormat.hour($0) }) { _ in
                AxisValueLabel()
            }
        }
        .chartYAxis { AxisMarks { _ in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel() } }
        .frame(height: 190)
        .accessibilityLabel("Requests by hour of day")
    }
}

// MARK: - Sessions

/// The sessions that used the most, each with its subagents' and side requests' share.
private struct SessionsPane: View {
    let r: StatsReport

    var body: some View {
        if r.sessions.isEmpty {
            EmptyCard(symbol: "text.bubble", title: "No sessions to show",
                      text: "No session used any tokens in this range, or dino's background service is older than this app: restart it from Settings → Updates.")
        } else {
            VStack(alignment: .leading, spacing: 16) {
                if let parts = r.totals.parts, !parts.isEmpty {
                    PartsLegend(parts: parts)
                }
                let names = StatsSessionNames()
                StatsCard(title: "Top sessions", note: "by tokens, at most \(r.sessions.count)") {
                    StatsTable(columns: [
                        .init("Session", flexible: true), .init("Model", numeric: false), .init("Tokens"), .init("Split", width: 150, numeric: false),
                        .init("Subagents"), .init("Side"), .init("Requests"), .init("Last used", numeric: false),
                    ]) {
                        ForEach(r.sessions, id: \.id) { s in
                            GridRow {
                                VStack(alignment: .leading, spacing: 1) {
                                    let title = names.title(s)
                                    Text(title).lineLimit(1)
                                    Text([AgentNames.of(s.agent), title == s.project ? "" : s.project, StatsFormat.day(s.first_ms)].filter { !$0.isEmpty }.joined(separator: " · "))
                                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                                .help(s.cwd ?? "")
                                Text(s.model.map { shortModel($0) } ?? "–").foregroundStyle(.secondary).lineLimit(1).help(s.model ?? "")
                                num(tokens(s.tokens.total))
                                if let p = s.parts, !p.isEmpty { PartsBar(parts: p, height: 8).frame(width: 150) } else { Text("–").foregroundStyle(.secondary) }
                                subagents(s)
                                num(s.parts.map { $0.side.tokens.total > 0 ? tokens($0.side.tokens.total) : "–" } ?? "–")
                                num(count(s.requests))
                                Text(StatsFormat.ago(s.last_ms)).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                Text("Subagents: how many ran, and their tokens. \(StatsPart.sideNote)")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// "6 · 37.8M": how many subagents a session ran, and their tokens.
private func subagents(_ s: StatsSession) -> some View {
    let t = s.parts?.subagents.tokens.total ?? 0
    let text = switch (s.subagents, t) {
    case (0, 0): "–"
    case (0, _): tokens(t)
    case (let n, 0): "\(n)"
    case (let n, _): "\(n) · \(tokens(t))"
    }
    return num(text)
}

/// A session's name as the sidebar had it, from the app's sessions and archive, read once when the
/// page draws (not watched). One dino doesn't know: its project.
@MainActor
struct StatsSessionNames {
    private var names: [String: String] = [:]

    init() {
        guard let model = (NSApp.delegate as? AppDelegate)?.model else { return }
        for a in model.archived { names[a.id] = a.name }
        for s in model.sessions { names[s.id] = s.name }
    }

    func title(_ s: StatsSession) -> String {
        if let id = s.session, let n = names[id], !n.isEmpty { return n }
        return s.project.isEmpty ? "Untitled" : s.project
    }
}

/// The three parts, as a key for the bars on the page.
private struct PartsLegend: View {
    let parts: StatsParts

    var body: some View {
        HStack(spacing: 18) {
            ForEach(StatsPart.allCases) { p in
                HStack(spacing: 6) {
                    RoundedRectangle(cornerRadius: 2).fill(p.color).frame(width: 10, height: 10)
                    Text(p.title)
                    Text(StatsFormat.percent(Double(p.of(parts)) / Double(max(parts.total, 1)))).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .font(.callout)
    }
}

/// A page with nothing to show: why, in a card where its content would be.
private struct EmptyCard: View {
    let symbol: String
    let title: String
    let text: String

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(text)
        }
        .frame(maxWidth: .infinity, minHeight: 280)
        .modifier(CardBackground())
    }
}

// MARK: - Models

/// Each model's colour: the report's order (most tokens first), the same in every chart here.
/// Eight hues in a fixed order, checked for colour vision deficiency; past that, grey.
enum ModelColors {
    private static let light: [UInt32] = [0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x4a3aa7, 0xe34948]
    private static let dark: [UInt32] = [0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767]

    static func slot(_ i: Int) -> Color {
        guard i >= 0, i < light.count else { return .gray }
        return Color(nsColor: NSColor(name: nil) { a in
            let hex = a.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark[i] : light[i]
            return NSColor(srgbRed: CGFloat(hex >> 16 & 0xff) / 255, green: CGFloat(hex >> 8 & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: 1)
        })
    }

    /// Names in the daily chart's order, and their colours ("Other" grey).
    static func scale(_ r: StatsReport) -> (domain: [String], range: [Color]) {
        var names: [String] = []
        for d in r.daily_models where !names.contains(d.model) { names.append(d.model) }
        let order = r.models.map(\.model)
        return (names, names.map { n in n == "Other" ? .gray : slot(order.firstIndex(of: n) ?? 99) })
    }
}

private struct ModelsPane: View {
    let r: StatsReport
    let size: StatsSize
    @State private var picked: Date?

    var body: some View {
        let scale = ModelColors.scale(r)
        let top = Array(r.models.prefix(8))
        let rest = r.models.dropFirst(8).reduce(UInt64(0)) { $0 + $1.tokens.total }
        VStack(alignment: .leading, spacing: 16) {
            StatsCard(title: "Tokens per day", note: "by model") {
                Chart {
                    ForEach(r.daily_models, id: \.self.key) { d in
                        if let date = StatsFormat.date(d.date) {
                            LineMark(x: .value("Day", date, unit: .day), y: .value("Tokens", Double(d.tokens)))
                                .foregroundStyle(by: .value("Model", shortModel(d.model)))
                                .lineStyle(StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
                                .interpolationMethod(.monotone)
                        }
                    }
                    if let picked {
                        RuleMark(x: .value("Day", picked, unit: .day))
                            .foregroundStyle(.secondary.opacity(0.5))
                            .annotation(position: .top, overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                                DayTip(date: picked, rows: r.daily_models.filter { $0.date == StatsFormat.key(picked) && $0.tokens > 0 })
                            }
                    }
                }
                .chartForegroundStyleScale(domain: scale.domain.map { shortModel($0) }, range: scale.range)
                .chartXSelection(value: $picked)
                .chartYAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(tokens(UInt64(n))) } } } }
                .chartLegend(position: .bottom, alignment: .leading)
                .frame(height: size.chart + 30)
                .accessibilityLabel("Tokens per day by model")
            }
            Columns(size: size, ratio: 0.4) {
                StatsCard(title: "Share of tokens") {
                    HStack(alignment: .center, spacing: 22) {
                        Chart {
                            ForEach(Array(top.enumerated()), id: \.offset) { i, m in
                                SectorMark(angle: .value("Tokens", Double(m.tokens.total)), innerRadius: .ratio(0.64), angularInset: 1.5)
                                    .foregroundStyle(ModelColors.slot(i))
                                    .cornerRadius(2)
                            }
                            if rest > 0 {
                                SectorMark(angle: .value("Tokens", Double(rest)), innerRadius: .ratio(0.64), angularInset: 1.5)
                                    .foregroundStyle(.gray)
                            }
                        }
                        .frame(width: 150, height: 150)
                        .overlay {
                            VStack(spacing: 0) {
                                Text(tokens(r.totals.tokens.total)).font(.title3.weight(.semibold))
                                Text("tokens").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .accessibilityLabel("Share of tokens by model")
                        VStack(alignment: .leading, spacing: 7) {
                            ForEach(Array(top.enumerated()), id: \.offset) { i, m in
                                legendRow(ModelColors.slot(i), shortModel(m.model), m.share)
                            }
                            if rest > 0 {
                                legendRow(.gray, "\(r.models.count - 8) more", Double(rest) / Double(max(r.totals.tokens.total, 1)))
                            }
                        }
                    }
                }
            } trailing: {
                StatsCard(title: "Every model") {
                    StatsTable(columns: [.init("Model", flexible: true), .init("Tokens"), .init("In"), .init("Out"), .init("Cache"), .init("Requests"), .init("Share")]) {
                        ForEach(Array(r.models.enumerated()), id: \.offset) { i, m in
                            GridRow {
                                HStack(spacing: 6) {
                                    Circle().fill(ModelColors.slot(i)).frame(width: 8, height: 8)
                                    VStack(alignment: .leading, spacing: 0) {
                                        Text(shortModel(m.model)).lineLimit(1).help(m.model)
                                        Text(m.agents.map(AgentNames.of).joined(separator: ", ")).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                    }
                                }
                                num(tokens(m.tokens.total))
                                num(tokens(m.tokens.input))
                                num(tokens(m.tokens.output))
                                num(tokens(m.tokens.cache_read + m.tokens.cache_write))
                                num(count(m.requests))
                                num(StatsFormat.percent(m.share))
                            }
                        }
                    }
                }
            }
        }
    }

    private func legendRow(_ color: Color, _ name: String, _ share: Double) -> some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(name).lineLimit(1)
            Spacer(minLength: 12)
            Text(StatsFormat.percent(share)).foregroundStyle(.secondary).monospacedDigit()
        }
        .font(.callout)
    }
}

private extension StatsDailyModel {
    var key: String { date + "\u{0}" + model }
}

private struct DayTip: View {
    let date: Date
    let rows: [StatsDailyModel]

    var body: some View {
        Tip(title: date.formatted(date: .abbreviated, time: .omitted),
            lines: rows.isEmpty ? [("Nothing", "")] : rows.sorted { $0.tokens > $1.tokens }.map { (shortModel($0.model), tokens($0.tokens)) })
    }
}

// MARK: - Agents

private struct AgentsPane: View {
    let r: StatsReport
    let size: StatsSize

    /// Each agent's tokens by part; one plain bar where dinod doesn't split them.
    private var bars: [(agent: String, part: StatsPart?, tokens: UInt64)] {
        r.agents.flatMap { a -> [(String, StatsPart?, UInt64)] in
            guard let p = a.parts, !p.isEmpty else { return [(a.agent, nil, a.tokens.total)] }
            return StatsPart.allCases.map { (a.agent, $0, $0.of(p)) }.filter { $0.2 > 0 }
        }
    }

    var body: some View {
        let split = r.agents.contains { $0.parts.map { !$0.isEmpty } ?? false }
        VStack(alignment: .leading, spacing: 16) {
            StatsCard(title: "Tokens by agent", note: split ? "conversations, subagents and side requests" : nil) {
                Chart(Array(bars.enumerated()), id: \.offset) { _, b in
                    BarMark(x: .value("Tokens", Double(b.tokens)), y: .value("Agent", AgentNames.of(b.agent)), height: .fixed(18))
                        .foregroundStyle(by: .value("Part", b.part?.title ?? "Tokens"))
                }
                .chartForegroundStyleScale(domain: split ? StatsPart.allCases.map(\.title) : ["Tokens"],
                                           range: split ? StatsPart.allCases.map(\.color) : [Brand.green])
                .chartLegend(split ? .visible : .hidden)
                .chartLegend(position: .top, alignment: .leading)
                .chartXAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(tokens(UInt64(n))) } } } }
                .frame(height: CGFloat(max(r.agents.count, 1)) * 34 + (split ? 56 : 30))
                .accessibilityLabel("Tokens by agent")
            }
            StatsCard(title: "Every agent", note: "routed through dino, and from each agent's own history") {
                StatsTable(columns: [
                    .init("Agent", flexible: true), .init("Tokens"), .init("Conversations"), .init("Subagents"), .init("Side"),
                    .init("Requests"), .init("Sessions"), .init("Active days"), .init("Via dino"), .init("From history"), .init("Last used", numeric: false),
                ]) {
                    ForEach(r.agents, id: \.agent) { a in
                        GridRow {
                            HStack(spacing: 6) {
                                Circle().fill(StatsFormat.agentColor(a.agent)).frame(width: 8, height: 8)
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(AgentNames.of(a.agent))
                                    Text(shortModel(a.top_model)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }
                            num(tokens(a.tokens.total))
                            num(a.parts.map { tokens($0.main.tokens.total) } ?? "–")
                            num(a.parts.map { $0.subagents.tokens.total > 0 ? tokens($0.subagents.tokens.total) : "–" } ?? "–")
                            num(a.parts.map { $0.side.tokens.total > 0 ? tokens($0.side.tokens.total) : "–" } ?? "–")
                            num(count(a.requests))
                            num(count(a.sessions))
                            num("\(a.active_days)")
                            num(count(a.proxied))
                            num(count(a.recorded))
                            Text(StatsFormat.ago(a.last_ms)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                if split { Text(StatsPart.sideNote) }
                let guessed = r.agents.filter { $0.undated > 0 }.map { AgentNames.of($0.agent) }
                if !guessed.isEmpty {
                    Text("\(ListFormatter.localizedString(byJoining: guessed)) \(guessed.count == 1 ? "doesn't" : "don't") record when each answer happened, so that usage counts here but not in the daily or hourly charts.")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

// MARK: - Projects

private struct ProjectsPane: View {
    let r: StatsReport
    let size: StatsSize

    var body: some View {
        if r.projects.isEmpty {
            EmptyCard(symbol: "folder", title: "No projects in this range", text: "The agents' history doesn't say which folders they ran in.")
        } else {
            let top = r.projects.first?.tokens.total ?? 1
            StatsCard(title: "Projects", note: "worktrees count toward their repository") {
                StatsTable(columns: [.init("Project", flexible: true), .init("Tokens"), .init("", width: 160, numeric: false), .init("Requests"),
                                     .init("Sessions"), .init("Agents", numeric: false), .init("Last used", numeric: false)]) {
                    ForEach(r.projects, id: \.name) { p in
                        GridRow {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(p.name).lineLimit(1)
                                Text(NSString(string: p.path).abbreviatingWithTildeInPath).font(.caption).foregroundStyle(.secondary)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                            .help(p.path)
                            num(tokens(p.tokens.total))
                            Meter(value: Double(p.tokens.total) / Double(max(top, 1))).frame(width: 160)
                            num(count(p.requests))
                            num(count(p.sessions))
                            Text(p.agents.map(AgentNames.of).joined(separator: ", ")).foregroundStyle(.secondary).lineLimit(1)
                            Text(StatsFormat.ago(p.last_ms)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }
}

/// A value against the largest, as a thin bar on a faint track of the same hue.
private struct Meter: View {
    let value: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Brand.green.opacity(0.15))
                Capsule().fill(Brand.green).frame(width: max(3, geo.size.width * value))
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}

// MARK: - Routes

private struct RoutesPane: View {
    let r: StatsReport
    let size: StatsSize

    var body: some View {
        if r.routes.isEmpty {
            EmptyCard(symbol: "arrow.triangle.branch", title: "No traffic routed through dino",
                      text: "Route, limit and speed data comes only from traffic routed through dino (Settings → Providers).")
        } else {
            LazyVGrid(columns: size.wide ? [GridItem(.flexible(), spacing: 16), GridItem(.flexible(), spacing: 16)] : [GridItem(.flexible())],
                      alignment: .leading, spacing: 16) {
                ForEach(r.routes, id: \.route) { RouteCard(route: $0) }
            }
        }
    }
}

private struct RouteCard: View {
    let route: StatsRoute

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(route.label.isEmpty ? route.route : route.label).font(.headline)
                    Text(Self.kind(route.kind)).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if let cost = route.cost {
                    Text(StatsFormat.money(cost)).font(.title3.weight(.medium))
                        .help("The cost the provider reported for these calls")
                }
            }
            Grid(alignment: .leading, horizontalSpacing: 24, verticalSpacing: 8) {
                GridRow {
                    fact("Tokens", tokens(route.tokens.total))
                    fact("Requests", count(route.requests))
                    fact("Errors", count(route.errors))
                    fact("Limit hits", count(route.limit_hits))
                }
                if route.fallbacks > 0 || route.last_limit_ms != nil || route.account_spend != nil {
                    GridRow {
                        if route.fallbacks > 0 { fact("As a fallback", count(route.fallbacks)) }
                        if let last = route.last_limit_ms { fact("Last limit", StatsFormat.ago(last)) }
                        // What the route's own account says, never worked out by dino.
                        if let spend = route.account_spend {
                            fact("Key spend", route.account_limit.map { "\(StatsFormat.money(spend)) of \(StatsFormat.money($0))" } ?? StatsFormat.money(spend))
                                .help("What OpenRouter says this key has spent, all time")
                                .gridCellColumns(2)
                        }
                    }
                }
            }
            if !route.windows.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(route.windows, id: \.name) { w in
                        QuotaBar(label: "\(w.name) window", window: WindowInfo(name: w.name, utilization: Float(w.used), resets_at: w.resets_at))
                    }
                }
            }
            if !route.models.isEmpty {
                Text(route.models.map { shortModel($0) }.joined(separator: " · ")).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .modifier(CardBackground())
        .accessibilityElement(children: .combine)
    }

    private func fact(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout.monospacedDigit())
        }
    }

    static func kind(_ k: String) -> String {
        switch k {
        case "own": "the agent's own sign-in or key"
        case "plan": "coding plan"
        case "openrouter": "OpenRouter"
        case "local": "model server"
        case "free": "free models"
        case "chatgpt_plan": "ChatGPT plan"
        default: k
        }
    }
}

// MARK: - Speed

private struct SpeedPane: View {
    let r: StatsReport
    let size: StatsSize

    var body: some View {
        if r.speed.isEmpty {
            EmptyCard(symbol: "speedometer", title: "No speed data in this range",
                      text: "Speed is measured only on traffic routed through dino (Settings → Providers).")
        } else {
            let ttft = r.speed.filter { $0.ttft_p50_ms != nil }
            let tps = r.speed.filter { $0.tps_p50 != nil }
            VStack(alignment: .leading, spacing: 16) {
                Columns(size: size) {
                    if !ttft.isEmpty {
                        StatsCard(title: "Time to first token", note: "median") {
                            Chart(ttft, id: \.self.key) { s in
                                BarMark(x: .value("Seconds", Double(s.ttft_p50_ms ?? 0) / 1000), y: .value("Model", s.label), height: .fixed(16))
                                    .foregroundStyle(Brand.green)
                                    .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 3, topTrailingRadius: 3))
                                    .annotation(position: .trailing) { Text(StatsFormat.ms(s.ttft_p50_ms)).font(.caption).foregroundStyle(.secondary) }
                            }
                            .chartXAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(String(format: "%gs", n)) } } } }
                            .frame(height: CGFloat(ttft.count) * 32 + 30)
                            .accessibilityLabel("Median time to first token by model")
                        }
                    }
                } trailing: {
                    if !tps.isEmpty {
                        StatsCard(title: "Output speed", note: "median tokens per second after the first") {
                            Chart(tps, id: \.self.key) { s in
                                BarMark(x: .value("Tokens per second", s.tps_p50 ?? 0), y: .value("Model", s.label), height: .fixed(16))
                                    .foregroundStyle(Brand.green)
                                    .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 3, topTrailingRadius: 3))
                                    .annotation(position: .trailing) { Text(StatsFormat.rate(s.tps_p50)).font(.caption).foregroundStyle(.secondary) }
                            }
                            .chartXAxis { AxisMarks { _ in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel() } }
                            .frame(height: CGFloat(tps.count) * 32 + 30)
                            .accessibilityLabel("Median output tokens per second by model")
                        }
                    }
                }
                StatsCard(title: "Every route and model") {
                    StatsTable(columns: [.init("Model", flexible: true), .init("Route", numeric: false), .init("Calls"),
                                         .init("TTFT p50"), .init("TTFT p90"), .init("tok/s p50"), .init("tok/s p90")]) {
                        ForEach(r.speed, id: \.self.key) { s in
                            GridRow {
                                Text(shortModel(s.model)).lineLimit(1).help(s.model)
                                Text(r.routes.first { $0.route == s.route }?.label ?? s.route).foregroundStyle(.secondary).lineLimit(1)
                                num(count(s.requests))
                                num(StatsFormat.ms(s.ttft_p50_ms))
                                num(StatsFormat.ms(s.ttft_p90_ms))
                                num(StatsFormat.rate(s.tps_p50))
                                num(StatsFormat.rate(s.tps_p90))
                            }
                        }
                    }
                }
            }
        }
    }
}

private extension StatsSpeed {
    var key: String { route + "\u{0}" + model }
    var label: String { "\(shortModel(model)) · \(routeLabel.isEmpty ? route : routeLabel)" }
}

// MARK: - Pieces

/// A table filling its card: a header row, then rows with hairlines between, the first column taking
/// the room that's left, numbers right-aligned.
private struct StatsTable<Rows: View>: View {
    struct Column {
        let title: String
        var flexible = false
        var width: CGFloat?
        var numeric = true
        init(_ title: String, flexible: Bool = false, width: CGFloat? = nil, numeric: Bool = true) {
            self.title = title
            self.flexible = flexible
            self.width = width
            self.numeric = numeric && !flexible
        }
    }

    let columns: [Column]
    @ViewBuilder let rows: Rows

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 20, verticalSpacing: 9) {
            GridRow {
                ForEach(Array(columns.enumerated()), id: \.offset) { _, c in
                    Text(c.title).font(.caption.weight(.medium)).foregroundStyle(.secondary)
                        .frame(maxWidth: c.flexible ? .infinity : nil, alignment: c.numeric ? .trailing : .leading)
                        .frame(width: c.width, alignment: .leading)
                        .gridColumnAlignment(c.numeric ? .trailing : .leading)
                }
            }
            Divider().gridCellUnsizedAxes(.horizontal)
            rows
        }
        .font(.callout)
    }
}

private func num(_ s: String) -> some View {
    Text(s).monospacedDigit()
}

private func count(_ n: UInt64) -> String {
    n.formatted(.number)
}

private struct SourcesNote: View {
    let s: StatsSources

    var body: some View {
        Text("\(count(s.proxied)) calls routed through dino · \(count(s.recorded)) from agents' own history · \(count(s.deduplicated)) duplicates skipped. Stats stay on this Mac and never sync.")
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

enum StatsFormat {
    /// An agent's colour as the app marks it; a plain shell (curl, scripts) isn't an agent: grey.
    static func agentColor(_ agent: String) -> Color {
        ["shell", "unknown"].contains(agent) ? Color.secondary.opacity(0.6) : AgentBadge.color(agent)
    }

    private static let parser: DateFormatter = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = .current
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    /// A local date as the report gives it ("2026-10-04"), at its midnight.
    static func date(_ s: String) -> Date? { parser.date(from: s) }

    static func key(_ d: Date) -> String { parser.string(from: d) }

    /// "8 Oct", from a time in ms.
    static func day(_ ms: Int64) -> String {
        ms > 0 ? Date(timeIntervalSince1970: Double(ms) / 1000).formatted(.dateTime.month(.abbreviated).day()) : ""
    }

    static func day(_ s: String, style: Date.FormatStyle.DateStyle) -> String {
        date(s)?.formatted(date: style, time: .omitted) ?? s
    }

    static func hour(_ h: Int) -> String {
        let d = Calendar.current.date(bySettingHour: h, minute: 0, second: 0, of: Date()) ?? Date()
        return d.formatted(.dateTime.hour())
    }

    static func duration(_ ms: Int64) -> String {
        let s = ms / 1000
        switch s {
        case ..<60: return "\(s)s"
        case ..<3600: return "\(s / 60)m"
        default: return "\(s / 3600)h \(s % 3600 / 60)m"
        }
    }

    static func ms(_ v: Int?) -> String {
        guard let v else { return "–" }
        return v < 1000 ? "\(v) ms" : String(format: "%.1f s", Double(v) / 1000)
    }

    static func rate(_ v: Double?) -> String {
        guard let v else { return "–" }
        return String(format: v < 10 ? "%.1f" : "%.0f", v)
    }

    static func percent(_ v: Double) -> String {
        v < 0.01 && v > 0 ? "<1%" : "\(Int((v * 100).rounded()))%"
    }

    static func money(_ v: Double) -> String {
        v.formatted(.currency(code: "USD").precision(.fractionLength(v < 1 ? 4 : 2)))
    }

    static func ago(_ ms: Int64) -> String {
        guard ms > 0 else { return "–" }
        return Date(timeIntervalSince1970: Double(ms) / 1000).formatted(.relative(presentation: .named))
    }

    static func name(_ path: String) -> String {
        (path as NSString).lastPathComponent
    }
}
