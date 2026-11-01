import Foundation
import Observation
import SwiftUI

/// What `LocalSessionForkView` needs to spin up an independent second
/// session (product atom 18's "fork") and to roll a session back to an
/// earlier checkpoint by reversing recorded effects. A protocol for the
/// same reason as `LocalConversationBranchProviding` in
/// `LocalConversationBranchView.swift`: the FFI entry points this would
/// call — `branching::fork_session`, `plan_rollback`, `execute_rollback` in
/// `engine/crates/mobile_runtime/src/branching.rs` — are not exposed
/// through `ffi.rs` yet. See "Integration gaps" below.
@MainActor
public protocol LocalSessionForkProviding: AnyObject {
    /// Mints a new, independent session id sharing `atSequence`'s
    /// checkpoint. The caller is responsible for actually starting a second
    /// `LocalSessionViewModel` with the returned id.
    func forkSession(label: String, atSequence: UInt64) async throws -> LocalConversationBranch

    /// Describes what a rollback to `sinceSequence` would have to reverse,
    /// without reversing anything. Surfacing this before the user confirms
    /// is what lets the UI show exactly what will be undone, and — per the
    /// rollback safety rule — whether the operation is even possible.
    func previewRollback(sinceSequence: UInt64) async throws -> LocalRollbackPlan

    /// Actually reverses every effect in the plan, most-recent-first.
    /// Throws `LocalRollbackError.irreversible` (mapping
    /// `branching::RollbackError::Irreversible`) without reversing anything
    /// if any step is unsafe — never a partial, silently-skipped undo.
    func performRollback(sinceSequence: UInt64) async throws
}

/// Mirrors `coderight_mobile_runtime::branching::RollbackStep`.
public struct LocalRollbackStep: Identifiable, Equatable, Sendable {
    public var id: UInt64 { sequence }
    public let sequence: UInt64
    public let effectId: String

    public init(sequence: UInt64, effectId: String) {
        self.sequence = sequence
        self.effectId = effectId
    }
}

/// Mirrors the `Ok` side of `branching::plan_rollback` — the steps that
/// would be reversed, most-recent-first, if the user confirms.
public struct LocalRollbackPlan: Equatable, Sendable {
    public let steps: [LocalRollbackStep]

    public init(steps: [LocalRollbackStep]) {
        self.steps = steps
    }

    public var isNoOp: Bool { steps.isEmpty }
}

/// Mirrors `branching::RollbackError`. The distinction the UI must never
/// blur: `irreversible` means nothing was touched (refused outright);
/// `reversalFailed` means some steps already reversed before this one
/// failed.
public enum LocalRollbackError: Error, Equatable {
    case irreversible(effectId: String, sequence: UInt64)
    case reversalFailed(effectId: String, sequence: UInt64, reason: String)
}

@MainActor
@Observable
public final class LocalSessionForkStore {
    public enum RollbackPhase: Equatable {
        case idle
        case previewing
        case previewed(LocalRollbackPlan)
        case refused(effectId: String, sequence: UInt64)
        case rollingBack
        case failed(String)
        case done
    }

    public private(set) var isForking = false
    public private(set) var forkedBranch: LocalConversationBranch?
    public private(set) var rollbackPhase: RollbackPhase = .idle
    public private(set) var errorMessage: String?

    private let provider: LocalSessionForkProviding

    public init(provider: LocalSessionForkProviding) {
        self.provider = provider
    }

    public func fork(label: String, atSequence: UInt64) async {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            errorMessage = "Label cannot be empty."
            return
        }
        isForking = true
        errorMessage = nil
        defer { isForking = false }
        do {
            forkedBranch = try await provider.forkSession(label: trimmed, atSequence: atSequence)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Step 1 of rollback: show the user what would be undone, or that it
    /// is refused, before anything happens.
    public func previewRollback(sinceSequence: UInt64) async {
        rollbackPhase = .previewing
        do {
            let plan = try await provider.previewRollback(sinceSequence: sinceSequence)
            rollbackPhase = .previewed(plan)
        } catch let LocalRollbackError.irreversible(effectId, sequence) {
            rollbackPhase = .refused(effectId: effectId, sequence: sequence)
        } catch {
            rollbackPhase = .failed(error.localizedDescription)
        }
    }

    /// Step 2: only reachable from `.previewed`, matching the safety rule
    /// that rollback never runs without the caller having first seen (and
    /// this store having validated) a plan with no irreversible step.
    public func confirmRollback(sinceSequence: UInt64) async {
        guard case .previewed = rollbackPhase else { return }
        rollbackPhase = .rollingBack
        do {
            try await provider.performRollback(sinceSequence: sinceSequence)
            rollbackPhase = .done
        } catch let LocalRollbackError.irreversible(effectId, sequence) {
            // Plan and execute can race against a concurrently-running turn
            // recording a new, non-reversible effect between preview and
            // confirm; re-refuse rather than assume the earlier preview
            // still holds.
            rollbackPhase = .refused(effectId: effectId, sequence: sequence)
        } catch let LocalRollbackError.reversalFailed(effectId, sequence, reason) {
            rollbackPhase = .failed("Rollback stopped at effect '\(effectId)' (sequence \(sequence)): \(reason)")
        } catch {
            rollbackPhase = .failed(error.localizedDescription)
        }
    }

    public func resetRollback() {
        rollbackPhase = .idle
    }
}

/// Lets the user fork the current conversation into a brand-new
/// independent session, or roll the current session back to an earlier
/// journal sequence with an explicit preview-then-confirm flow so a refused
/// (irreversible) rollback is shown, never silently attempted.
///
/// # Integration gaps
/// - `LocalSessionForkProviding` has no production conformance: `ffi.rs`
///   does not yet expose `branching::fork_session`, `plan_rollback`, or
///   `execute_rollback` (`engine/crates/mobile_runtime/src/branching.rs`),
///   and is outside this lane's file allowlist.
/// - `mobile_runtime/src/lib.rs` needs `pub mod branching;` before that FFI
///   work can reference the module; outside this lane's allowlist.
/// - The reversibility classifier `branching::plan_rollback` requires
///   (`is_reversible: FnMut(&str) -> bool`) has no production
///   implementation; see the "Integration gaps" note in `branching.rs`.
/// - Not yet added to `project.yml` or wired into any navigation host.
@MainActor
public struct LocalSessionForkView: View {
    @Bindable private var store: LocalSessionForkStore
    private let currentSequence: UInt64
    @State private var forkLabel = ""

    public init(store: LocalSessionForkStore, currentSequence: UInt64) {
        self.store = store
        self.currentSequence = currentSequence
    }

    public var body: some View {
        Form {
            Section {
                TextField("New session label", text: $forkLabel)
                Button("Fork into new session") {
                    let label = forkLabel
                    Task { await store.fork(label: label, atSequence: currentSequence) }
                }
                .disabled(forkLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.isForking)
            } footer: {
                if let branch = store.forkedBranch {
                    Text("Forked as \(branch.id). Start a new session with that id to continue it independently.")
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                } else {
                    Text("Creates a second, independent conversation from this point. The current one is untouched.")
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                }
            }

            Section("Roll back") {
                rollbackControl
            }

            if let errorMessage = store.errorMessage {
                Section {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Fork & rollback")
    }

    @ViewBuilder
    private var rollbackControl: some View {
        switch store.rollbackPhase {
        case .idle:
            Button("Preview rollback to here") {
                Task { await store.previewRollback(sinceSequence: currentSequence) }
            }
        case .previewing:
            ProgressView("Checking recorded effects…")
        case let .previewed(plan):
            if plan.isNoOp {
                Text("Nothing to roll back — no effects since this point.")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Will reverse \(plan.steps.count) effect(s), most recent first:")
                        .font(.footnote)
                    ForEach(plan.steps) { step in
                        Text("• \(step.effectId) (sequence \(step.sequence))")
                            .font(.caption)
                            .foregroundStyle(Theme.muted)
                    }
                    Button("Confirm rollback", role: .destructive) {
                        Task { await store.confirmRollback(sinceSequence: currentSequence) }
                    }
                }
            }
        case let .refused(effectId, sequence):
            VStack(alignment: .leading, spacing: 4) {
                Label("Rollback refused", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text("Effect '\(effectId)' at sequence \(sequence) cannot be safely reversed. Nothing was undone.")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
                Button("Dismiss") { store.resetRollback() }
            }
        case .rollingBack:
            ProgressView("Rolling back…")
        case .failed(let message):
            VStack(alignment: .leading, spacing: 4) {
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(.red)
                Button("Dismiss") { store.resetRollback() }
            }
        case .done:
            Label("Rolled back", systemImage: "checkmark.circle.fill")
                .foregroundStyle(Theme.accent2)
        }
    }
}
