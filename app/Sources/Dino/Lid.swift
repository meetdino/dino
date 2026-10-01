import AppKit
import SwiftUI

/// Settings → General: keep agents running with the lid closed. dinod does the work; this sets
/// when, and installs (once, with an administrator's password) the permission it needs.
struct LidSection: View {
    @EnvironmentObject var store: SettingsStore
    @EnvironmentObject var model: DinoModel
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
            if lid.enabled {
                Picker("While", selection: Binding(get: { lid.when }, set: { w in set { $0.when = w } })) {
                    Text("an agent is working").tag("working")
                    Text("dino has an agent open").tag("open")
                }
                Toggle("Also on battery", isOn: Binding(get: { lid.on_battery }, set: { on in set { $0.on_battery = on } }))
                if lid.on_battery {
                    Stepper("Down to \(lid.min_battery)%", value: Binding(get: { lid.min_battery }, set: { p in set { $0.min_battery = p } }), in: 10 ... 90, step: 5)
                }
                Picker("At most", selection: Binding(get: { lid.max_hours }, set: { h in set { $0.max_hours = h } })) {
                    ForEach([2.0, 4, 8, 12, 24], id: \.self) { h in Text("\(Int(h)) hours").tag(h) }
                    Text("No limit").tag(0.0)
                }
            }
            if let power = model.power, power.holding || power.external {
                Label(power.holding ? "Awake now, with the lid closed or open" : "Sleep is already off, turned off outside dino; dino leaves it alone",
                      systemImage: power.holding ? "laptopcomputer" : "info.circle")
                    .foregroundStyle(.secondary)
            }
            if let power = model.power, !power.holding, let note = power.note {
                Text("Last time: \(note).").foregroundStyle(.secondary)
            }
            if let error = error ?? model.power?.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            if ready == true {
                LabeledContent("Permission") {
                    Button("Remove…") {
                        set { $0.enabled = false }
                        power("remove")
                    }
                    .disabled(busy)
                }
            }
        } header: {
            Text("Lid closed")
        } footer: {
            Footnote("dino turns system sleep off only while this holds, and back on when it stops: work done, unplugged (unless battery is allowed), battery low, the Mac running hot, or the time limit. Sleep also comes back if dino quits or crashes, and at every restart. The display still sleeps. The first time, macOS asks for your password to let dino run exactly “pmset -a disablesleep 0” and “1”, nothing else.")
        }
        .onAppear { power("status") }
    }
}
