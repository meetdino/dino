import SwiftUI

// The Welcome card's switches, drawn from what they're given: Notifier's NeedsYouNotifyToggle and
// the card's computer use row read the state and pass it in.

/// "Notify me when an agent needs me", and why it can't when macOS says no.
struct NeedsYouNotifyRow: View {
    @Binding var isOn: Bool
    /// What stops it notifying (macOS turned dino's notifications off); nil when nothing does.
    var note: String?
    var openSettings: () -> Void
    /// In a Form (Settings) rather than on the card.
    var form = false

    static let title = "Notify me when an agent needs me"
    static let summary = "Names the session and what it asks; click to go there."

    var body: some View {
        if form {
            Toggle(isOn: $isOn) {
                Text(Self.title)
                Text(note ?? Self.summary)
            }
            if note != nil {
                HStack {
                    Spacer()
                    Button("Open System Settings", action: openSettings)
                }
            }
        } else {
            VStack(alignment: .leading, spacing: 2) {
                Toggle(Self.title, isOn: $isOn)
                    .toggleStyle(.checkbox)
                    .font(.callout)
                    .help(Self.summary)
                if let note {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(note).foregroundStyle(.secondary).lineLimit(1)
                        Button("Open System Settings", action: openSettings).buttonStyle(.link)
                    }
                    .font(.caption)
                    .padding(.leading, 20)
                }
            }
        }
    }
}

enum ComputerUseCopy {
    static let title = "Let agents use your Mac's apps"
    static let summary = "Adds open-computer-use to your agents. It needs Accessibility and Screen Recording permission once."
}

/// Where computer use is on this Mac, as the card says it.
enum ComputerUseStatus: Equatable {
    case off
    case installing
    /// Installed; macOS's permissions for it not checked yet (nil), or these still missing.
    case permissions(missing: String?)
    case ready
    case failed(String)
}

/// The card's computer use switch, on unless turned off, with one line on where it is.
struct ComputerUseRow: View {
    @Binding var isOn: Bool
    var status: ComputerUseStatus
    var checking = false
    var check: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Toggle(ComputerUseCopy.title, isOn: $isOn)
                .toggleStyle(.checkbox)
                .font(.callout)
                .help("Agents can see your screen and click and type in your apps, with open-computer-use (open source). Be careful: anything on screen could tell an agent to do something you didn't ask for.")
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                line
            }
            .font(.caption)
            .lineLimit(1)
            .padding(.leading, 20)
        }
    }

    @ViewBuilder private var line: some View {
        switch status {
        case .off:
            Text("Off. Agents' own computer use isn't affected.").foregroundStyle(.secondary)
        case .installing:
            Text("Installing open-computer-use…").foregroundStyle(.secondary)
        case .permissions(nil):
            Text("Needs Accessibility and Screen Recording permission once.").foregroundStyle(.secondary)
            checkButton("Check…")
        case .permissions(let missing?):
            Text("Still needs \(missing) permission.").foregroundStyle(.secondary)
            checkButton("Check Again")
        case .ready:
            Label("Has Accessibility and Screen Recording permission.", systemImage: "checkmark.circle.fill")
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.secondary)
        case .failed(let why):
            Text(why).foregroundStyle(.orange).help(why)
        }
    }

    @ViewBuilder private func checkButton(_ title: String) -> some View {
        if checking {
            ProgressView().controlSize(.mini)
        } else {
            Button(title, action: check)
                .buttonStyle(.link)
                .help("Asks open-computer-use what macOS allows it, and opens its setup for anything missing")
        }
    }
}
