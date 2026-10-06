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
    init() {}
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0); errors = c.v("errors", 0)
        limit_hits = c.v("limit_hits", 0); sessions = c.v("sessions", 0); active_days = c.v("active_days", 0); days = c.v("days", 0)
        favorite_model = c.v("favorite_model", nil); longest_session = c.v("longest_session", nil)
        peak_hour = c.v("peak_hour", nil); most_active_day = c.v("most_active_day", nil)
        cost = c.v("cost", nil); priced_requests = c.v("priced_requests", 0)
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
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        agent = c.v("agent", ""); tokens = c.v("tokens", StatsTokens()); requests = c.v("requests", 0)
        sessions = c.v("sessions", 0); active_days = c.v("active_days", 0); proxied = c.v("proxied", 0)
        recorded = c.v("recorded", 0); last_ms = c.v("last_ms", 0); top_model = c.v("top_model", nil)
        undated = c.v("undated", 0)
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
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: StatsKey.self)
        schema = c.v("schema", 0); range = c.v("range", "30d"); from_ms = c.v("from_ms", 0); to_ms = c.v("to_ms", 0)
        utc_offset = c.v("utc_offset", 0); totals = c.v("totals", StatsTotals()); streaks = c.v("streaks", StatsStreaks())
        periods = c.v("periods", StatsPeriods()); heatmap = c.v("heatmap", []); days = c.v("days", []); hours = c.v("hours", [])
        models = c.v("models", []); daily_models = c.v("daily_models", []); agents = c.v("agents", [])
        projects = c.v("projects", []); routes = c.v("routes", []); speed = c.v("speed", []); sources = c.v("sources", StatsSources())
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
    case overview, models, agents, projects, routes, speed
    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
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

    @StateObject private var store = StatsStore()
    @AppStorage("stats.range") private var range = "30d"
    @AppStorage("stats.pane") private var pane: StatsPane = .overview
    @State private var confirmClear = false

    var body: some View {
        NavigationSplitView(columnVisibility: .constant(.all)) {
            List(selection: Binding(get: { pane }, set: { if let p = $0 { pane = p } })) {
                ForEach(StatsPane.allCases) { p in
                    Label(p.title, systemImage: p.symbol).tag(p)
                }
            }
            .navigationSplitViewColumnWidth(min: 170, ideal: 170, max: 170)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(minWidth: 760, minHeight: 560)
                .navigationTitle(pane.title)
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
        .onAppear { store.load(range) }
        .onChange(of: range) { _, r in store.load(r) }
        .confirmationDialog("Clear usage stats?", isPresented: $confirmClear) {
            Button("Clear Stats", role: .destructive) { store.clear(then: range) }
        } message: {
            Text("The route, speed and limit data dino recorded is deleted permanently. Usage from the agents' own history is read again the next time you open Usage Stats.")
        }
    }

    @ViewBuilder private var detail: some View {
        if let r = store.report {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if let e = store.error { errorLine(e) }
                    if r.isEmpty && pane != .routes {
                        empty
                    } else {
                        switch pane {
                        case .overview: OverviewPane(r: r)
                        case .models: ModelsPane(r: r)
                        case .agents: AgentsPane(r: r)
                        case .projects: ProjectsPane(r: r)
                        case .routes: RoutesPane(r: r)
                        case .speed: SpeedPane(r: r)
                        }
                    }
                    SourcesNote(s: r.sources)
                }
                .padding(24)
                .frame(maxWidth: .infinity, alignment: .leading)
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
        }
        .frame(maxWidth: .infinity, minHeight: 320)
    }

    private func errorLine(_ e: String) -> some View {
        Label(e, systemImage: "exclamationmark.triangle").font(.caption).foregroundStyle(.secondary)
    }
}

// MARK: - Overview

private struct OverviewPane: View {
    let r: StatsReport

    var body: some View {
        let t = r.totals
        VStack(alignment: .leading, spacing: 24) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 170), spacing: 12)], spacing: 12) {
                Tile(title: "Tokens", value: tokens(t.tokens.total),
                     detail: "\(tokens(t.tokens.input)) in · \(tokens(t.tokens.output)) out · \(tokens(t.tokens.cache_read + t.tokens.cache_write)) cache")
                Tile(title: "Requests", value: count(t.requests),
                     detail: t.errors + t.limit_hits > 0 ? "\(count(t.errors)) errors · \(count(t.limit_hits)) limit hits" : "no errors")
                Tile(title: "Sessions", value: count(t.sessions), detail: "across every agent")
                Tile(title: "Active days", value: "\(t.active_days)", detail: "of \(t.days)")
                Tile(title: "Current streak", value: days(r.streaks.current), detail: "days in a row")
                Tile(title: "Longest streak", value: days(r.streaks.longest),
                     detail: r.streaks.longest_from.map { "from \(StatsFormat.day($0, style: .abbreviated))" } ?? "")
                Tile(title: "Favorite model", value: t.favorite_model.map { shortModel($0) } ?? "–", detail: "most tokens")
                Tile(title: "Longest session", value: t.longest_session.map { StatsFormat.duration($0.ms) } ?? "–",
                     detail: t.longest_session.map { AgentNames.of($0.agent) + ($0.cwd.map { " · " + StatsFormat.name($0) } ?? "") } ?? "")
                Tile(title: "Peak hour", value: t.peak_hour.map(StatsFormat.hour) ?? "–", detail: "most requests")
                Tile(title: "Most active day", value: t.most_active_day.map { StatsFormat.day($0.date, style: .abbreviated) } ?? "–",
                     detail: t.most_active_day.map { "\(tokens($0.tokens)) tokens" } ?? "")
                if let cost = t.cost {
                    Tile(title: "Reported cost", value: StatsFormat.money(cost), detail: "\(count(t.priced_requests)) priced requests")
                }
            }
            PeriodStrip(p: r.periods)
            StatsSection(title: "Activity", note: "the last 12 months, whatever the range") { Heatmap(days: r.heatmap) }
            StatsSection(title: "Requests by hour", note: "local time") { HoursChart(hours: r.hours) }
        }
    }

    private func days(_ n: UInt64) -> String { n == 1 ? "1 day" : "\(n) days" }
}

private struct Tile: View {
    let title: String
    let value: String
    var detail = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.title2.weight(.semibold).monospacedDigit()).lineLimit(1).minimumScaleFactor(0.6)
            Text(detail).font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.5)))
        .accessibilityElement(children: .combine)
    }
}

private struct PeriodStrip: View {
    let p: StatsPeriods

    var body: some View {
        HStack(spacing: 0) {
            cell("Today", p.today)
            Divider().frame(height: 34)
            cell("Last 7 days", p.week)
            Divider().frame(height: 34)
            cell("Last 30 days", p.month)
        }
        .padding(.vertical, 10)
        .background(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
    }

    private func cell(_ title: String, _ p: StatsPeriod) -> some View {
        VStack(spacing: 2) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            Text("\(tokens(p.tokens)) tokens").font(.callout.weight(.medium).monospacedDigit())
            Text("\(count(p.requests)) requests · \(count(p.sessions)) sessions").font(.caption2).foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }
}

/// A titled block of the page.
private struct StatsSection<Content: View>: View {
    let title: String
    var note: String?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title).font(.headline)
                if let note { Text(note).font(.caption).foregroundStyle(.tertiary) }
            }
            content
        }
    }
}

/// Weeks as columns, weekdays as rows, one green from faint to full by how many tokens a day had.
private struct Heatmap: View {
    let days: [StatsDay]
    private let cell: CGFloat = 11
    private let gap: CGFloat = 3

    var body: some View {
        let weeks = Self.weeks(days)
        let cuts = Self.cuts(days)
        VStack(alignment: .leading, spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: gap) {
                        ForEach(weeks.indices, id: \.self) { i in
                            Text(Self.monthLabel(weeks, i)).font(.caption2).foregroundStyle(.secondary)
                                .fixedSize().frame(width: cell, alignment: .leading)
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
                .padding(.trailing, 2)
            }
            .defaultScrollAnchor(.trailing)
            HStack(spacing: 4) {
                Spacer()
                Text("Less").font(.caption2).foregroundStyle(.tertiary)
                ForEach(0..<5, id: \.self) { l in
                    RoundedRectangle(cornerRadius: 2).fill(Self.color(level: l)).frame(width: cell, height: cell)
                }
                Text("More").font(.caption2).foregroundStyle(.tertiary)
            }
            .accessibilityHidden(true)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Activity heatmap")
        .accessibilityValue("\(days.filter { $0.requests > 0 }.count) active days in the last year")
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
        .frame(height: 160)
        .accessibilityLabel("Requests by hour of day")
    }
}

// MARK: - Models

/// Each model's colour: the report's order (most tokens first), the same in every chart here.
/// Eight hues in a fixed order, checked for colour vision deficiency; past that, grey.
private enum ModelColors {
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
    @State private var picked: Date?

    var body: some View {
        let scale = ModelColors.scale(r)
        let top = Array(r.models.prefix(8))
        let rest = r.models.dropFirst(8).reduce(UInt64(0)) { $0 + $1.tokens.total }
        VStack(alignment: .leading, spacing: 24) {
            StatsSection(title: "Tokens per day", note: "by model") {
                Chart {
                    ForEach(r.daily_models, id: \.self.key) { d in
                        if let date = StatsFormat.date(d.date) {
                            LineMark(x: .value("Day", date, unit: .day), y: .value("Tokens", Double(d.tokens)))
                                .foregroundStyle(by: .value("Model", shortModel(d.model)))
                                .lineStyle(StrokeStyle(lineWidth: 2))
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
                .frame(height: 240)
                .accessibilityLabel("Tokens per day by model")
            }
            StatsSection(title: "Share of tokens") {
                HStack(alignment: .center, spacing: 28) {
                    Chart {
                        ForEach(Array(top.enumerated()), id: \.offset) { i, m in
                            SectorMark(angle: .value("Tokens", Double(m.tokens.total)), innerRadius: .ratio(0.62), angularInset: 1.5)
                                .foregroundStyle(ModelColors.slot(i))
                                .cornerRadius(2)
                        }
                        if rest > 0 {
                            SectorMark(angle: .value("Tokens", Double(rest)), innerRadius: .ratio(0.62), angularInset: 1.5)
                                .foregroundStyle(.gray)
                        }
                    }
                    .frame(width: 180, height: 180)
                    .overlay {
                        VStack(spacing: 0) {
                            Text(tokens(r.totals.tokens.total)).font(.title3.weight(.semibold).monospacedDigit())
                            Text("tokens").font(.caption2).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityLabel("Share of tokens by model")
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(top.enumerated()), id: \.offset) { i, m in
                            HStack(spacing: 8) {
                                Circle().fill(ModelColors.slot(i)).frame(width: 8, height: 8)
                                Text(shortModel(m.model)).lineLimit(1)
                                Spacer(minLength: 12)
                                Text(StatsFormat.percent(m.share)).foregroundStyle(.secondary).monospacedDigit()
                            }
                            .font(.callout)
                        }
                        if rest > 0 {
                            HStack(spacing: 8) {
                                Circle().fill(.gray).frame(width: 8, height: 8)
                                Text("\(r.models.count - 8) more")
                                Spacer(minLength: 12)
                                Text(StatsFormat.percent(Double(rest) / Double(max(r.totals.tokens.total, 1)))).foregroundStyle(.secondary).monospacedDigit()
                            }
                            .font(.callout)
                        }
                    }
                    .frame(maxWidth: 320)
                }
            }
            StatsSection(title: "Every model") {
                StatsTable(headers: ["Model", "Tokens", "In", "Out", "Cache", "Requests", "Share", "Agents"]) {
                    ForEach(Array(r.models.enumerated()), id: \.offset) { i, m in
                        GridRow {
                            HStack(spacing: 6) {
                                Circle().fill(ModelColors.slot(i)).frame(width: 7, height: 7)
                                Text(shortModel(m.model)).lineLimit(1).help(m.model)
                            }
                            num(tokens(m.tokens.total))
                            num(tokens(m.tokens.input))
                            num(tokens(m.tokens.output))
                            num(tokens(m.tokens.cache_read + m.tokens.cache_write))
                            num(count(m.requests))
                            num(StatsFormat.percent(m.share))
                            Text(m.agents.map(AgentNames.of).joined(separator: ", ")).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }
                }
            }
        }
    }
}

private extension StatsDailyModel {
    var key: String { date + "\u{0}" + model }
}

private struct DayTip: View {
    let date: Date
    let rows: [StatsDailyModel]

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(date.formatted(date: .abbreviated, time: .omitted)).font(.caption.weight(.semibold))
            if rows.isEmpty { Text("Nothing").font(.caption).foregroundStyle(.secondary) }
            ForEach(rows.sorted { $0.tokens > $1.tokens }, id: \.model) { d in
                HStack {
                    Text(shortModel(d.model))
                    Spacer(minLength: 12)
                    Text(tokens(d.tokens)).monospacedDigit()
                }
                .font(.caption)
            }
        }
        .padding(8)
        .frame(minWidth: 160)
        .background(RoundedRectangle(cornerRadius: 6).fill(.background).shadow(color: .black.opacity(0.15), radius: 4, y: 1))
    }
}

// MARK: - Agents

private struct AgentsPane: View {
    let r: StatsReport

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            StatsSection(title: "Tokens by agent") {
                Chart(r.agents, id: \.agent) { a in
                    BarMark(x: .value("Tokens", Double(a.tokens.total)), y: .value("Agent", AgentNames.of(a.agent)), height: .ratio(0.6))
                        .foregroundStyle(StatsFormat.agentColor(a.agent))
                        .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 3, topTrailingRadius: 3))
                        .annotation(position: .trailing) { Text(tokens(a.tokens.total)).font(.caption).foregroundStyle(.secondary) }
                }
                .chartXAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(tokens(UInt64(n))) } } } }
                .frame(height: CGFloat(max(r.agents.count, 1)) * 30 + 30)
                .accessibilityLabel("Tokens by agent")
            }
            StatsSection(title: "Every agent", note: "routed through dino, and from each agent's own history") {
                StatsTable(headers: ["Agent", "Tokens", "Requests", "Sessions", "Active days", "Via dino", "From history", "Top model", "Last used"]) {
                    ForEach(r.agents, id: \.agent) { a in
                        GridRow {
                            HStack(spacing: 6) {
                                Circle().fill(StatsFormat.agentColor(a.agent)).frame(width: 7, height: 7)
                                Text(AgentNames.of(a.agent))
                            }
                            num(tokens(a.tokens.total))
                            num(count(a.requests))
                            num(count(a.sessions))
                            num("\(a.active_days)")
                            num(count(a.proxied))
                            num(count(a.recorded))
                            Text(shortModel(a.top_model)).foregroundStyle(.secondary).lineLimit(1)
                            Text(StatsFormat.ago(a.last_ms)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            let guessed = r.agents.filter { $0.undated > 0 }.map { AgentNames.of($0.agent) }
            if !guessed.isEmpty {
                Text("\(ListFormatter.localizedString(byJoining: guessed)) \(guessed.count == 1 ? "doesn't" : "don't") record when each answer happened, so that usage counts here but not in the daily or hourly charts.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
    }
}

// MARK: - Projects

private struct ProjectsPane: View {
    let r: StatsReport

    var body: some View {
        if r.projects.isEmpty {
            Text("No projects in this range. The agents' history doesn't say which folders they ran in.")
                .foregroundStyle(.secondary)
        } else {
            StatsSection(title: "Projects", note: "worktrees count toward their repository") {
                StatsTable(headers: ["Project", "Tokens", "Requests", "Sessions", "Agents", "Last used"]) {
                    ForEach(r.projects, id: \.name) { p in
                        GridRow {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(p.name).lineLimit(1)
                                Text(NSString(string: p.path).abbreviatingWithTildeInPath).font(.caption2).foregroundStyle(.tertiary)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                            .help(p.path)
                            num(tokens(p.tokens.total))
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

// MARK: - Routes

private struct RoutesPane: View {
    let r: StatsReport

    var body: some View {
        if r.routes.isEmpty {
            Text("No traffic was routed through dino in this range. Route, limit and speed data comes only from traffic routed through dino.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(r.routes, id: \.route) { RouteCard(route: $0) }
            }
        }
    }
}

private struct RouteCard: View {
    let route: StatsRoute

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(route.label.isEmpty ? route.route : route.label).font(.headline)
                Text(Self.kind(route.kind)).font(.caption).foregroundStyle(.secondary)
                Spacer()
                if let cost = route.cost {
                    Text(StatsFormat.money(cost)).font(.callout.weight(.medium).monospacedDigit())
                        .help("The cost the provider reported for these calls")
                }
            }
            HStack(spacing: 24) {
                fact("Tokens", tokens(route.tokens.total))
                fact("Requests", count(route.requests))
                fact("Errors", count(route.errors))
                fact("Limit hits", count(route.limit_hits))
                if route.fallbacks > 0 { fact("As a fallback", count(route.fallbacks)) }
                if let last = route.last_limit_ms { fact("Last limit", StatsFormat.ago(last)) }
                // What the route's own account says, never worked out by dino.
                if let spend = route.account_spend {
                    fact("Key spend", route.account_limit.map { "\(StatsFormat.money(spend)) of \(StatsFormat.money($0))" } ?? StatsFormat.money(spend))
                        .help("What OpenRouter says this key has spent, all time")
                }
            }
            if !route.windows.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(route.windows, id: \.name) { w in
                        QuotaBar(label: "\(w.name) window", window: WindowInfo(name: w.name, utilization: Float(w.used), resets_at: w.resets_at))
                    }
                }
                .frame(maxWidth: 420)
            }
            if !route.models.isEmpty {
                Text(route.models.map { shortModel($0) }.joined(separator: " · ")).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.5)))
        .accessibilityElement(children: .combine)
    }

    private func fact(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption2).foregroundStyle(.secondary)
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

    var body: some View {
        if r.speed.isEmpty {
            Text("Speed is measured only on traffic routed through dino, and there was none in this range.")
                .foregroundStyle(.secondary)
        } else {
            let ttft = r.speed.filter { $0.ttft_p50_ms != nil }
            let tps = r.speed.filter { $0.tps_p50 != nil }
            VStack(alignment: .leading, spacing: 24) {
                if !ttft.isEmpty {
                    StatsSection(title: "Time to first token", note: "median, by route and model") {
                        Chart(ttft, id: \.self.key) { s in
                            BarMark(x: .value("Seconds", Double(s.ttft_p50_ms ?? 0) / 1000), y: .value("Model", s.label), height: .ratio(0.6))
                                .foregroundStyle(Brand.green)
                                .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 3, topTrailingRadius: 3))
                                .annotation(position: .trailing) { Text(StatsFormat.ms(s.ttft_p50_ms)).font(.caption).foregroundStyle(.secondary) }
                        }
                        .chartXAxis { AxisMarks { v in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel { if let n = v.as(Double.self) { Text(String(format: "%gs", n)) } } } }
                        .frame(height: CGFloat(ttft.count) * 30 + 30)
                        .accessibilityLabel("Median time to first token by model")
                    }
                }
                if !tps.isEmpty {
                    StatsSection(title: "Output speed", note: "median tokens per second after the first") {
                        Chart(tps, id: \.self.key) { s in
                            BarMark(x: .value("Tokens per second", s.tps_p50 ?? 0), y: .value("Model", s.label), height: .ratio(0.6))
                                .foregroundStyle(Brand.green)
                                .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 3, topTrailingRadius: 3))
                                .annotation(position: .trailing) { Text(StatsFormat.rate(s.tps_p50)).font(.caption).foregroundStyle(.secondary) }
                        }
                        .chartXAxis { AxisMarks { _ in AxisGridLine().foregroundStyle(.quaternary); AxisValueLabel() } }
                        .frame(height: CGFloat(tps.count) * 30 + 30)
                        .accessibilityLabel("Median output tokens per second by model")
                    }
                }
                StatsSection(title: "Every route and model") {
                    StatsTable(headers: ["Model", "Route", "Calls", "TTFT p50", "TTFT p90", "tok/s p50", "tok/s p90"]) {
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

/// A plain table: a header row, then rows, columns sized to fit.
private struct StatsTable<Rows: View>: View {
    let headers: [String]
    @ViewBuilder let rows: Rows

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 7) {
            GridRow {
                ForEach(Array(headers.enumerated()), id: \.offset) { i, h in
                    Text(h).font(.caption.weight(.medium)).foregroundStyle(.secondary)
                        .gridColumnAlignment(i > 0 && Self.numeric(h) ? .trailing : .leading)
                }
            }
            Divider().gridCellUnsizedAxes(.horizontal)
            rows
        }
        .font(.callout)
    }

    static func numeric(_ h: String) -> Bool {
        !["Model", "Agent", "Project", "Agents", "Top model", "Last used", "Route"].contains(h)
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
            .font(.caption2)
            .foregroundStyle(.tertiary)
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
