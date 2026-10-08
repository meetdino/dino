import AppKit
import SwiftUI

/// Settings → Power: keep agents running with the lid closed. dinod does the work; this sets
/// when, and installs (once, with an administrator's password) the permission it needs.
struct LidSection: View {
    @EnvironmentObject var store: SettingsStore
    @ObservedObject private var powerState = PowerState.shared
    /// The permission is installed; nil until dinod has said.
    @State private var ready: Bool?
    @State private var busy = false
    @State private var error: String?

    private var lid: DinoSettings.Lid { store.settings?.machine.lid ?? .standard }

    private func set(_ change: @escaping (inout DinoSettings.Lid) -> Void) {
        store.update { s in
            var l = s.machine.lid ?? .standard
            change(&l)
            s.machine.lid = l
        }
    }

    /// Asks dinod off the main thread: `setup` waits on the password prompt.
    private func power(_ action: String, then: ((Bool) -> Void)? = nil) {
        busy = action != "status"
        Task.detached {
            let result = Result { try DinoConnection(path: DinoEnvironment.socketPath).power(action) }
            await MainActor.run {
                busy = false
                switch result {
                case let .success(p):
                    ready = p.ready
                    error = nil
                    then?(p.ready == true)
                case let .failure(e):
                    let message = e.localizedDescription
                    error = message == "Cancelled" ? nil : message
                    then?(false)
                }
            }
        }
    }

    var body: some View {
        Section {
            Toggle("Keep agents running with the lid closed", isOn: Binding(
                get: { lid.enabled },
                set: { on in
                    guard on else { return set { $0.enabled = false } }
                    // The first time, macOS asks for an administrator's password; cancelled, it stays off.
                    if ready == true { return set { $0.enabled = true } }
                    power("setup") { ok in if ok { set { $0.enabled = true } } }
                }
            ))
            .disabled(store.settings == nil || busy)
            .orgLocked("machine.lid")
            .settingAnchor("lid")
            if lid.enabled {
                Picker("While", selection: Binding(get: { lid.when }, set: { w in set { $0.when = w } })) {
                    Text("An agent is working").tag("working")
                    Text("Any agent session is open").tag("open")
                }
                Toggle("Also on battery", isOn: Binding(get: { lid.on_battery }, set: { on in set { $0.on_battery = on } }))
                if lid.on_battery {
                    Stepper("Until battery is at \(lid.min_battery)%", value: Binding(get: { lid.min_battery }, set: { p in set { $0.min_battery = p } }), in: 10 ... 90, step: 5)
                }
                Picker("For up to", selection: Binding(get: { lid.max_hours }, set: { h in set { $0.max_hours = h } })) {
                    ForEach([2.0, 4, 8, 12, 24], id: \.self) { h in Text("\(Int(h)) hours").tag(h) }
                    Text("No limit").tag(0.0)
                }
            }
            if let power = powerState.info, power.holding || power.external {
                Label(power.holding ? "Your Mac is staying awake now, even with the lid closed" : "Sleep was turned off outside dino, so dino leaves it as it is",
                      systemImage: power.holding ? "laptopcomputer" : "info.circle")
                    .foregroundStyle(.secondary)
            }
            if let power = powerState.info, !power.holding, let note = power.note {
                Text("Last time: \(note).").foregroundStyle(.secondary)
            }
            if let error = error ?? powerState.info?.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            if ready == true {
                LabeledContent("Permission to turn off sleep") {
                    Button("Remove…") {
                        set { $0.enabled = false }
                        power("remove")
                    }
                    .disabled(busy)
                    .help("Removes dino's permission to run “pmset -a disablesleep 0” and “1”")
                }
            }
        } header: {
            Text("Lid Closed")
        } footer: {
            Footnote("Your Mac sleeps normally again when the work is done, when it's unplugged (unless “Also on battery” is on), when the battery runs low, when it gets too hot, or when the time limit is reached. Sleep also turns back on if dino's background service stops, and whenever your Mac restarts. The display still turns off.\n\nThe first time, macOS asks for your password. It lets dino turn sleep off and on, and nothing else.")
        }
        .onAppear { power("status") }
    }
}
