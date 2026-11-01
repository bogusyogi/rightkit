import SwiftUI

/// Execution target a run can be dispatched to. Mirrors
/// `coderight_tools::cloud_endpoint::ExecutionTarget` on the engine side —
/// kept as a plain Swift enum here (rather than imported through FFI)
/// because no FFI binding for it exists yet; see the gap note at the
/// bottom of this file.
public enum LocalExecutionTarget: String, CaseIterable, Identifiable, Sendable {
    case local
    case cloud
    case computer

    public var id: String { rawValue }

    public var displayName: String {
        switch self {
        case .local: return "This iPhone"
        case .cloud: return "Cloud endpoint"
        case .computer: return "Paired computer"
        }
    }
}

/// ENG-67: what a target can do, who pays for it, and where data goes.
/// Mirrors `coderight_tools::cloud_workspace::ExecutionTargetDisclosure`.
public struct ExecutionTargetDisclosure: Sendable, Equatable, Identifiable {
    public let target: LocalExecutionTarget
    public let capabilities: [String]
    public let payer: String
    public let dataLocation: String

    public var id: String { target.id }
}

/// Static disclosure text per target. This is policy copy, not per-session
/// state — it needs no engine round trip, so the view can render it even
/// before any cloud endpoint is connected. Kept in exact correspondence
/// with `cloud_workspace::disclosure_for` on the engine side; if that
/// function's wording changes, update this table too.
public enum LocalTargetDisclosures {
    public static let all: [ExecutionTargetDisclosure] = [
        ExecutionTargetDisclosure(
            target: .local,
            capabilities: [
                "Runs on this device only",
                "Stops if the app is closed or the device sleeps",
                "Full filesystem access within the selected workspace",
            ],
            payer: "You, directly, via your configured model provider account",
            dataLocation: "Stays on this device except for model provider API calls you configured"
        ),
        ExecutionTargetDisclosure(
            target: .cloud,
            capabilities: [
                "Keeps running after this device disconnects",
                "Resumable session/event history",
                "Bound by the connected endpoint's own resource limits",
            ],
            payer: "You, via the cloud endpoint you connected (its own billing, outside the app)",
            dataLocation: "Workspace files and run output live on the connected cloud endpoint"
        ),
        ExecutionTargetDisclosure(
            target: .computer,
            capabilities: [
                "Runs on a paired desktop/daemon over the network",
                "Keeps running as long as that paired machine is on",
                "Filesystem access limited to that machine's shared workspace",
            ],
            payer: "You — no separate billing beyond the paired machine you already own/rent",
            dataLocation: "Workspace files and run output live on the paired machine"
        ),
    ]

    public static func disclosure(for target: LocalExecutionTarget) -> ExecutionTargetDisclosure {
        all.first(where: { $0.target == target }) ?? all[0]
    }
}

/// Shows, per execution target, what it can do, who pays, and where data
/// goes — surfaced before or while a user picks where a session runs.
public struct LocalTargetDisclosureView: View {
    private let preselected: LocalExecutionTarget?

    public init(preselected: LocalExecutionTarget? = nil) {
        self.preselected = preselected
    }

    public var body: some View {
        Form {
            ForEach(LocalTargetDisclosures.all) { disclosure in
                Section {
                    ForEach(disclosure.capabilities, id: \.self) { capability in
                        Label(capability, systemImage: "checkmark.circle")
                            .foregroundStyle(Theme.text)
                    }
                    LabeledContent("Who pays") {
                        Text(disclosure.payer)
                    }
                    LabeledContent("Data location") {
                        Text(disclosure.dataLocation)
                    }
                } header: {
                    HStack {
                        Text(disclosure.target.displayName)
                        if disclosure.target == preselected {
                            Spacer()
                            Text("Selected")
                                .font(.caption)
                                .foregroundStyle(Theme.accent)
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Execution Targets")
        .navigationBarTitleDisplayMode(.inline)
    }
}

// ---------------------------------------------------------------------
// Gap: this view's copy is hand-mirrored from
// `coderight_tools::cloud_workspace::disclosure_for` rather than fetched
// through FFI, because `cloud_workspace` is not yet declared in
// `coderight_tools`'s `lib.rs` and has no `CodeRightMobileRuntime` FFI
// entry point. Once that wiring lands, replace `LocalTargetDisclosures`
// with a call through the runtime bridge so the two copies cannot drift.
// ---------------------------------------------------------------------
