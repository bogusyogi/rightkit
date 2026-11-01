import Foundation
import Observation
import SwiftUI

/// One labelled fork point in a local conversation's history, mirroring
/// `coderight_mobile_runtime::branching::Branch` (ENG-69). This type lives
/// on the Swift side only until the FFI plumbing described below exists —
/// see "Integration gaps".
public struct LocalConversationBranch: Identifiable, Equatable, Sendable {
    public enum Status: Equatable, Sendable {
        case active
        /// ENG-69: "summarize abandoned branches before returning".
        case abandoned(summary: String)
    }

    public let id: String
    public let label: String
    public let createdAt: Date
    public let forkSequence: UInt64
    public var status: Status

    public init(id: String, label: String, createdAt: Date, forkSequence: UInt64, status: Status) {
        self.id = id
        self.label = label
        self.createdAt = createdAt
        self.forkSequence = forkSequence
        self.status = status
    }
}

/// What `LocalConversationBranchView` needs from the runtime. A protocol
/// rather than a concrete FFI call because the C ABI entry points this
/// would call (`branching::create_branch`, `list_branches`,
/// `abandon_branch` in `engine/crates/mobile_runtime/src/branching.rs`) are
/// not yet exposed through `ffi.rs` — see "Integration gaps" below. A real
/// adapter conforms to this against the FFI once that lands; a preview or
/// test can conform a stub directly.
@MainActor
public protocol LocalConversationBranchProviding: AnyObject {
    func listBranches() async throws -> [LocalConversationBranch]
    /// `atSequence` is the parent journal sequence to fork from — callers
    /// pass the session's current high-water mark to branch "from here".
    func createBranch(label: String, atSequence: UInt64) async throws -> LocalConversationBranch
    func abandonBranch(id: String, summary: String) async throws -> LocalConversationBranch
    /// Resumes the session on the given branch's frozen checkpoint. Actually
    /// switching means restarting `LocalSessionViewModel` with that
    /// checkpoint, which is the caller's responsibility — this view only
    /// asks for it.
    func switchToBranch(id: String) async throws
}

@MainActor
@Observable
public final class LocalConversationBranchStore {
    public private(set) var branches: [LocalConversationBranch] = []
    public private(set) var isLoading = false
    public private(set) var errorMessage: String?

    private let provider: LocalConversationBranchProviding

    public init(provider: LocalConversationBranchProviding) {
        self.provider = provider
    }

    public func reload() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            branches = try await provider.listBranches()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    public func createBranch(label: String, atSequence: UInt64) async {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            errorMessage = "Label cannot be empty."
            return
        }
        errorMessage = nil
        do {
            let branch = try await provider.createBranch(label: trimmed, atSequence: atSequence)
            branches.append(branch)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    public func abandon(_ branch: LocalConversationBranch, summary: String) async {
        errorMessage = nil
        do {
            let updated = try await provider.abandonBranch(id: branch.id, summary: summary)
            if let index = branches.firstIndex(where: { $0.id == updated.id }) {
                branches[index] = updated
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    public func switchTo(_ branch: LocalConversationBranch) async {
        errorMessage = nil
        do {
            try await provider.switchToBranch(id: branch.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// Lists branch points for the current conversation, lets the user label a
/// new one from the current turn, switch to an existing branch, or abandon
/// one with a summary (ENG-69).
///
/// # Integration gaps
/// - No FFI entry points exist yet for `branching::create_branch`,
///   `list_branches`, or `abandon_branch`
///   (`engine/crates/mobile_runtime/src/branching.rs`); `ffi.rs` is outside
///   this lane's file allowlist, so `LocalConversationBranchProviding` has
///   no production conformance yet.
/// - `mobile_runtime/src/lib.rs` needs `pub mod branching;` before that FFI
///   work can even reference the module; also outside this lane's
///   allowlist.
/// - Not yet added to `project.yml` or wired into `LocalSessionView`'s
///   navigation; both are out of this lane's scope.
@MainActor
public struct LocalConversationBranchView: View {
    @Bindable private var store: LocalConversationBranchStore
    /// Current journal high-water mark, supplied by the caller (typically
    /// `LocalSessionViewModel`), used as the fork point for a new branch.
    private let currentSequence: UInt64
    @State private var newLabel = ""
    @State private var abandoningBranch: LocalConversationBranch?
    @State private var abandonSummary = ""

    public init(store: LocalConversationBranchStore, currentSequence: UInt64) {
        self.store = store
        self.currentSequence = currentSequence
    }

    public var body: some View {
        List {
            Section {
                HStack {
                    TextField("Label this point in the conversation", text: $newLabel)
                        .textFieldStyle(.roundedBorder)
                    Button("Branch") {
                        let label = newLabel
                        newLabel = ""
                        Task { await store.createBranch(label: label, atSequence: currentSequence) }
                    }
                    .disabled(newLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            } footer: {
                Text("Creates a labelled fork point without changing the current conversation.")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
            }

            Section("Branches") {
                if store.branches.isEmpty && !store.isLoading {
                    Text("No branches yet.")
                        .font(.footnote)
                        .foregroundStyle(Theme.muted)
                } else {
                    ForEach(store.branches) { branch in
                        branchRow(branch)
                    }
                }
            }

            if let errorMessage = store.errorMessage {
                Section {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                }
            }
        }
        .navigationTitle("Conversation branches")
        .task { await store.reload() }
        .sheet(item: $abandoningBranch) { branch in
            abandonSheet(for: branch)
        }
    }

    @ViewBuilder
    private func branchRow(_ branch: LocalConversationBranch) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(branch.label)
                    .font(.subheadline.weight(.semibold))
                Spacer()
                if case .abandoned = branch.status {
                    Text("Abandoned")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(Theme.muted)
                }
            }
            Text("Fork point: sequence \(branch.forkSequence)")
                .font(.caption)
                .foregroundStyle(Theme.muted)
            if case let .abandoned(summary) = branch.status, !summary.isEmpty {
                Text(summary)
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
            }
            HStack {
                Button("Switch") { Task { await store.switchTo(branch) } }
                    .font(.caption)
                if case .active = branch.status {
                    Button("Abandon") {
                        abandonSummary = ""
                        abandoningBranch = branch
                    }
                    .font(.caption)
                    .foregroundStyle(.red)
                }
            }
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private func abandonSheet(for branch: LocalConversationBranch) -> some View {
        NavigationStack {
            Form {
                Section("Summarize before returning") {
                    TextField("What happened on this branch?", text: $abandonSummary, axis: .vertical)
                        .lineLimit(3...6)
                }
            }
            .navigationTitle(branch.label)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { abandoningBranch = nil }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Abandon") {
                        let summary = abandonSummary
                        abandoningBranch = nil
                        Task { await store.abandon(branch, summary: summary) }
                    }
                }
            }
        }
    }
}
