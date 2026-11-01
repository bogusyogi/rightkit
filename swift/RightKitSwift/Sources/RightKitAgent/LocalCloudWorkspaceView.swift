import Foundation
import Observation
import SwiftUI

/// Mirrors `coderight_tools::cloud_workspace::CloudWorkspaceStatus`.
public enum CloudWorkspaceStatus: Equatable, Sendable {
    case provisioning
    case ready
    case failed(reason: String)
}

/// Mirrors `coderight_tools::cloud_workspace::CloudWorkspaceHandle`.
public struct CloudWorkspaceHandle: Equatable, Sendable {
    public let workspaceId: String
    public let target: LocalExecutionTarget
    public let baseURL: String
    public let status: CloudWorkspaceStatus
}

/// Mirrors `coderight_tools::cloud_workspace::CloudRunStatus`. `disconnected`
/// is not a failure — the run keeps executing server-side; only the client
/// attachment dropped.
public enum CloudRunStatus: String, Equatable, Sendable {
    case running
    case disconnected
    case completed
    case failed
}

/// Mirrors `coderight_tools::cloud_workspace::CloudRunEvent`. `cursor` is
/// monotonically increasing per run; resuming asks for everything after the
/// last cursor the client saw.
public struct CloudRunEvent: Equatable, Sendable, Identifiable {
    public let cursor: UInt64
    public let kind: String
    public let summary: String
    public var id: UInt64 { cursor }
}

/// Mirrors `coderight_tools::cloud_workspace::CloudRunSnapshot`.
public struct CloudRunSnapshot: Equatable, Sendable {
    public let runId: String
    public let workspaceId: String
    public let status: CloudRunStatus
    public let lastCursor: UInt64
}

/// Seam for the actual engine round trip. No FFI entry point exists yet for
/// `cloud_workspace` (see the gap note at the bottom of this file), so the
/// store below is wired against this protocol rather than a concrete
/// runtime call, the same way `CloudEndpointConnector` is wired against
/// `CloudEndpointTransport` — swapping in a real implementation later
/// requires no change to the store or view.
public protocol CloudWorkspaceBackend: Sendable {
    func provisionWorkspace(target: LocalExecutionTarget) async throws -> CloudWorkspaceHandle
    func startRun(workspaceId: String, runId: String) async throws -> CloudRunSnapshot
    /// Resume from `sinceCursor` (exclusive) — the cursor of the last event
    /// this client already applied. Never re-returns events at or before it,
    /// which is what keeps a reconnect from duplicating effects the client
    /// already rendered.
    func resumeEvents(runId: String, sinceCursor: UInt64) async throws -> [CloudRunEvent]
    func runSnapshot(runId: String) async throws -> CloudRunSnapshot?
    func setConnected(runId: String, connected: Bool) async
}

/// Drives one cloud workspace + run: provisions the workspace, starts (or
/// re-attaches to) a run, and on reconnect resumes the event log from the
/// last cursor this device actually applied — never from zero, so a
/// disconnect/reconnect cycle never re-plays (and re-renders) events the
/// device already has.
@MainActor
@Observable
public final class LocalCloudWorkspaceStore {
    public private(set) var workspace: CloudWorkspaceHandle?
    public private(set) var run: CloudRunSnapshot?
    public private(set) var events: [CloudRunEvent] = []
    public private(set) var isBusy = false
    public private(set) var lastError: String?

    /// The cursor of the last event this device has applied. Persisted in
    /// memory only for now — see the gap note; a durable (e.g. UserDefaults)
    /// cursor would let resumption survive an app relaunch, not just a
    /// transient network drop within one app session.
    private var lastSeenCursor: UInt64 = 0

    private let backend: CloudWorkspaceBackend

    public init(backend: CloudWorkspaceBackend) {
        self.backend = backend
    }

    public func provisionAndStart(target: LocalExecutionTarget, runId: String) async {
        isBusy = true
        defer { isBusy = false }
        do {
            let handle = try await backend.provisionWorkspace(target: target)
            workspace = handle
            guard case .ready = handle.status else { return }
            let snapshot = try await backend.startRun(workspaceId: handle.workspaceId, runId: runId)
            run = snapshot
            lastSeenCursor = snapshot.lastCursor
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Called when the app backgrounds/loses connectivity. Does not clear
    /// `run` or `events` — the run is still executing on the endpoint; only
    /// this device's attachment state changes.
    public func noteDisconnected() async {
        guard let run else { return }
        await backend.setConnected(runId: run.runId, connected: false)
    }

    /// Called on reconnect (foreground, network restored, or explicit
    /// retry). Fetches only events after `lastSeenCursor`, appends them, and
    /// advances the cursor — so repeated calls (e.g. a flaky network
    /// retrying this itself) never duplicate what is already in `events`.
    public func reconnect() async {
        guard let run else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            await backend.setConnected(runId: run.runId, connected: true)
            let fresh = try await backend.resumeEvents(runId: run.runId, sinceCursor: lastSeenCursor)
            guard !fresh.isEmpty else {
                if let snapshot = try await backend.runSnapshot(runId: run.runId) {
                    self.run = snapshot
                }
                return
            }
            events.append(contentsOf: fresh)
            lastSeenCursor = fresh.map(\.cursor).max() ?? lastSeenCursor
            if let snapshot = try await backend.runSnapshot(runId: run.runId) {
                self.run = snapshot
            }
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }
}

/// Surfaces workspace provisioning state, run status, and the resumed event
/// log for one cloud (or paired-computer) run.
public struct LocalCloudWorkspaceView: View {
    @State private var store: LocalCloudWorkspaceStore
    private let target: LocalExecutionTarget
    private let runId: String

    public init(store: LocalCloudWorkspaceStore, target: LocalExecutionTarget, runId: String) {
        _store = State(initialValue: store)
        self.target = target
        self.runId = runId
    }

    public var body: some View {
        Form {
            Section {
                LabeledContent("Target", value: target.displayName)
                LabeledContent("Workspace", value: workspaceStatusLabel)
                LabeledContent("Run", value: runStatusLabel)
            } header: {
                Text("Cloud Workspace")
            } footer: {
                Text(
                    "A cloud run keeps executing on the connected endpoint while this device is "
                        + "disconnected. Reconnecting resumes the run's event history instead of "
                        + "starting over."
                )
            }

            if !store.events.isEmpty {
                Section("Events") {
                    ForEach(store.events) { event in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(event.kind).foregroundStyle(Theme.text)
                            Text("cursor \(event.cursor) · \(event.summary)")
                                .font(.footnote)
                                .foregroundStyle(Theme.muted)
                        }
                    }
                }
            }

            if let lastError = store.lastError {
                Section {
                    Text(lastError).foregroundStyle(Theme.danger)
                }
            }

            Section {
                if store.workspace == nil {
                    Button("Provision & start run") {
                        Task { await store.provisionAndStart(target: target, runId: runId) }
                    }
                    .disabled(store.isBusy)
                } else {
                    Button("Reconnect (resume from last cursor)") {
                        Task { await store.reconnect() }
                    }
                    .disabled(store.isBusy)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Cloud Workspace")
        .navigationBarTitleDisplayMode(.inline)
    }

    private var workspaceStatusLabel: String {
        switch store.workspace?.status {
        case .none: return "Not provisioned"
        case .provisioning: return "Provisioning…"
        case .ready: return "Ready"
        case .failed(let reason): return "Failed: \(reason)"
        }
    }

    private var runStatusLabel: String {
        switch store.run?.status {
        case .none: return "Not started"
        case .running: return "Running"
        case .disconnected: return "Running (disconnected — will resume)"
        case .completed: return "Completed"
        case .failed: return "Failed"
        }
    }
}

// ---------------------------------------------------------------------
// Integration gaps (out of this lane's file allowlist):
//
// - No `CloudWorkspaceBackend` implementation backed by the real engine
//   exists yet: `cloud_workspace` (engine/crates/tools/src/cloud_workspace.rs)
//   is not declared in `coderight_tools`'s `lib.rs`, has no tool
//   registration in `registry/registry_impl.rs`, and has no
//   `CodeRightMobileRuntime` FFI entry point
//   (`LocalRuntime/CodeRightMobileRuntime.h` is outside this lane's
//   allowlist). Until that lands, `LocalCloudWorkspaceStore` has no
//   production `CloudWorkspaceBackend` to be constructed with; only a test
//   stub can drive it today.
// - `lastSeenCursor` is in-memory only; surviving an app relaunch (not just
//   a transient disconnect within one session) needs it persisted
//   (UserDefaults or the session store), which is a follow-up once the
//   backend exists to persist against.
// ---------------------------------------------------------------------
