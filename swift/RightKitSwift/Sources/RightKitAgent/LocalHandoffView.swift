import Foundation
import Observation
import SwiftUI

/// Execution target a session can be handed off to/from. String-compatible
/// with `coderight_tools::cloud_endpoint::ExecutionTarget::as_str()`
/// (`"local" | "cloud" | "computer"`) so a value round-trips through a
/// `TargetMetadata.target` without a translation table.
public enum HandoffTarget: String, CaseIterable, Codable, Sendable {
    case local
    case cloud
    case computer

    public var displayName: String {
        switch self {
        case .local: return "This iPhone"
        case .cloud: return "Cloud endpoint"
        case .computer: return "Paired computer"
        }
    }
}

/// Phase of an in-flight handoff, mirroring `HandoffController`'s
/// `HandoffPhase` (ENG-66): the destination is never allowed to resume until
/// the source has confirmed it quiesced, so at most one target ever owns the
/// session's dispatch loop.
public enum HandoffPhase: Equatable, Sendable {
    case idle
    case quiescingSource
    case sourceQuiesced
    case resumingDestination
    case completed
    case failed(String)
}

/// Drives one explicit session handoff from the app side. Never changes a
/// session's active target on its own initiative — `beginHandoff` requires
/// the caller (the view) to name both source and destination explicitly,
/// matching the "explicit, never implicit" contract in the Rust
/// `HandoffController` this ports.
@MainActor
@Observable
public final class LocalHandoffStore {
    public private(set) var phase: HandoffPhase = .idle
    public private(set) var source: HandoffTarget?
    public private(set) var destination: HandoffTarget?

    /// Injected seam so this store can be driven against the real FFI bridge
    /// in the app and against a fake in tests, mirroring the transport-seam
    /// pattern used by `CloudEndpointConnector`.
    public struct Bridge: Sendable {
        /// Asks the source target's runtime to stop dispatching tool calls
        /// for `sessionID` and confirms once it has. Corresponds to
        /// `HandoffController::confirm_quiesced` being called from the
        /// source's own runtime after it has actually stopped.
        public var quiesceSource: @Sendable (_ sessionID: String, _ source: HandoffTarget) async throws -> Void
        /// Fetches the exported `SessionPackage` bytes for `sessionID` from
        /// the source, to be handed to the destination.
        public var exportPackage: @Sendable (_ sessionID: String) async throws -> Data
        /// Resumes the session at `destination` using the decoded package.
        /// Corresponds to `HandoffController::resume_at_destination`.
        public var resumeAtDestination: @Sendable (_ sessionID: String, _ destination: HandoffTarget, _ package: SessionPackage) async throws -> Void

        public init(
            quiesceSource: @escaping @Sendable (String, HandoffTarget) async throws -> Void,
            exportPackage: @escaping @Sendable (String) async throws -> Data,
            resumeAtDestination: @escaping @Sendable (String, HandoffTarget, SessionPackage) async throws -> Void
        ) {
            self.quiesceSource = quiesceSource
            self.exportPackage = exportPackage
            self.resumeAtDestination = resumeAtDestination
        }
    }

    private let bridge: Bridge

    public init(bridge: Bridge) {
        self.bridge = bridge
    }

    /// Runs one full handoff: quiesce source → export package → decode +
    /// validate (refusing an unrecognized schema version) → resume
    /// destination. Stops at the first failure and reports it in `phase`
    /// rather than partially advancing — a failed quiesce must never let the
    /// destination proceed.
    public func beginHandoff(
        sessionID: String,
        from source: HandoffTarget,
        to destination: HandoffTarget
    ) async {
        guard source != destination else {
            phase = .failed("Source and destination must differ.")
            return
        }
        self.source = source
        self.destination = destination
        phase = .quiescingSource

        do {
            try await bridge.quiesceSource(sessionID, source)
        } catch {
            phase = .failed("Could not quiesce \(source.displayName): \(error.localizedDescription)")
            return
        }
        phase = .sourceQuiesced

        let packageData: Data
        do {
            packageData = try await bridge.exportPackage(sessionID)
        } catch {
            phase = .failed("Could not export session package: \(error.localizedDescription)")
            return
        }

        let package: SessionPackage
        do {
            package = try SessionPackageImporter.decode(packageData)
        } catch {
            phase = .failed(error.localizedDescription)
            return
        }

        phase = .resumingDestination
        do {
            try await bridge.resumeAtDestination(sessionID, destination, package)
        } catch {
            phase = .failed("Could not resume on \(destination.displayName): \(error.localizedDescription)")
            return
        }

        phase = .completed
    }

    public func reset() {
        phase = .idle
        source = nil
        destination = nil
    }
}

/// Explicit handoff picker + progress view. The user must pick a
/// destination distinct from the current target and tap "Move Session" —
/// there is no automatic/implicit path that starts a handoff on its own.
public struct LocalHandoffView: View {
    @Bindable private var store: LocalHandoffStore
    private let sessionID: String
    private let currentTarget: HandoffTarget

    @State private var selectedDestination: HandoffTarget?

    public init(store: LocalHandoffStore, sessionID: String, currentTarget: HandoffTarget) {
        self.store = store
        self.sessionID = sessionID
        self.currentTarget = currentTarget
    }

    public var body: some View {
        List {
            Section("Current target") {
                Text(currentTarget.displayName)
                    .foregroundStyle(Theme.text)
            }

            Section("Move to") {
                ForEach(HandoffTarget.allCases.filter { $0 != currentTarget }, id: \.self) { target in
                    Button {
                        selectedDestination = target
                    } label: {
                        HStack {
                            Text(target.displayName)
                                .foregroundStyle(Theme.text)
                            Spacer()
                            if selectedDestination == target {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(Theme.accent)
                            }
                        }
                    }
                }
            }

            Section {
                statusRow
                Button("Move Session") {
                    guard let destination = selectedDestination else { return }
                    Task {
                        await store.beginHandoff(sessionID: sessionID, from: currentTarget, to: destination)
                    }
                }
                .disabled(selectedDestination == nil || isInFlight)
            } footer: {
                Text("The current target stops handling this session before the new one takes over, so the session is never driven from two places at once.")
                    .foregroundStyle(Theme.muted)
            }
        }
        .navigationTitle("Handoff")
    }

    private var isInFlight: Bool {
        switch store.phase {
        case .quiescingSource, .sourceQuiesced, .resumingDestination:
            return true
        case .idle, .completed, .failed:
            return false
        }
    }

    @ViewBuilder
    private var statusRow: some View {
        switch store.phase {
        case .idle:
            EmptyView()
        case .quiescingSource:
            Label("Stopping current target…", systemImage: "hourglass")
                .foregroundStyle(Theme.muted)
        case .sourceQuiesced:
            Label("Preparing session package…", systemImage: "hourglass")
                .foregroundStyle(Theme.muted)
        case .resumingDestination:
            Label("Resuming on new target…", systemImage: "hourglass")
                .foregroundStyle(Theme.muted)
        case .completed:
            Label("Session moved.", systemImage: "checkmark.circle.fill")
                .foregroundStyle(Theme.accent2)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(Theme.danger)
        }
    }
}
