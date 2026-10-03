import SwiftUI

/// A long list of models, as Pi lists them (hundreds, across providers): searchable, grouped by
/// provider, and never taller than a popover can show. Used where a menu of them would run off
/// the screen.
struct ModelSearchList: View {
    let options: [ControlOption]
    /// The one chosen (nil: Default).
    let current: String?
    let defaultLabel: String
    var defaultHelp: String?
    /// A name typed that matches nothing is used as is, as "Other model" did.
    var allowsOther = true
    /// A Default row first (the agent's own model); not for a provider's catalog.
    var showsDefault = true
    let choose: (String?) -> Void

    @State private var search = ""
    @FocusState private var searching: Bool

    /// Its section: the group dino gave it, else the provider in a "provider/model" id.
    static func section(_ o: ControlOption) -> String? {
        if let g = o.group { return g }
        guard let slash = o.value.firstIndex(of: "/") else { return nil }
        return String(o.value[..<slash])
    }

    private var matches: [ControlOption] {
        let q = search.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return options }
        return options.filter { $0.label.localizedCaseInsensitiveContains(q) || $0.value.localizedCaseInsensitiveContains(q) }
    }

    /// Sections in the order they first appear, ungrouped first.
    private var sections: [(String?, [ControlOption])] {
        var order: [String?] = []
        var rows: [String?: [ControlOption]] = [:]
        for o in matches {
            let s = Self.section(o)
            if rows[s] == nil { order.append(s) }
            rows[s, default: []].append(o)
        }
        order.sort { a, b in a == nil && b != nil }
        return order.map { ($0, rows[$0] ?? []) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField("Search", text: $search, prompt: Text("Search \(options.count) models"))
                .textFieldStyle(.roundedBorder)
                .focused($searching)
                .onSubmit(submit)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if showsDefault, search.trimmingCharacters(in: .whitespaces).isEmpty {
                        row(nil, defaultLabel, defaultHelp)
                    }
                    ForEach(sections, id: \.0) { section, rows in
                        if let section {
                            Text(section).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                                .padding(.top, 8).padding(.bottom, 2).padding(.horizontal, 4)
                        }
                        ForEach(rows, id: \.value) { o in row(o.value, o.label, o.help == o.value ? nil : o.help) }
                    }
                    if matches.isEmpty {
                        Text(allowsOther ? "No match: Return uses “\(search)” as the model name" : "No models match")
                            .font(.callout).foregroundStyle(.secondary).padding(4)
                    }
                }
            }
            // A popover sizes a scroll view to nothing: tall enough to browse, never past the screen.
            .frame(height: 320)
        }
        .onAppear { DispatchQueue.main.async { searching = true } }
    }

    private func submit() {
        let q = search.trimmingCharacters(in: .whitespaces)
        if let first = matches.first, !q.isEmpty {
            choose(first.value)
        } else if !q.isEmpty, allowsOther {
            choose(q)
        }
    }

    private func row(_ value: String?, _ label: String, _ help: String?) -> some View {
        Button { choose(value) } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "checkmark")
                    .font(.caption.weight(.semibold))
                    .opacity(value == current ? 1 : 0)
                VStack(alignment: .leading, spacing: 1) {
                    Text(label).lineLimit(1)
                    if let help { Text(help).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 3)
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

extension ControlKind {
    /// More models than a menu or a popover column shows comfortably: searched instead.
    static let longList = 15
}
