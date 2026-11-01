import SwiftUI

/// ENG-94: mid-turn credential request & resume.
///
/// Mirrors the shape of `ApprovalInteractionState` / `ApprovalInteractionCard`
/// (`apps/coderight-ios/CodeRight/Approvals/ApprovalInteractionState.swift`,
/// `apps/coderight-ios/CodeRight/Views/ApprovalInteractionCard.swift`) — a
/// blocking transcript card that locks itself on submit and is replaced by a
/// resolved/failed/stale state from the daemon — but is deliberately its own
/// type rather than a case grafted onto `ApprovalInteractionState`, because
/// this card's submitted value is a credential secret and needs to be kept
/// out of every place that type is stored, logged, or diffed:
///
/// - `ApprovalInteractionState` conforms to `Equatable`/`Codable` and is
///   diffed (`onChange(of: state)`), archived, and potentially journaled
///   alongside ordinary approvals; a secret riding inside it would follow all
///   of that machinery.
/// - `CredentialRequestState` below intentionally does **not** conform to
///   `Codable` and its `Equatable` conformance ignores the secret entirely
///   (see `Equatable` conformance below) — it must never be persisted,
///   logged, or archived verbatim while a secret is present.
///
/// # Where the secret is and is not allowed to travel
///
/// - The typed value the operator enters lives only in the view's local
///   `@State private var secretInput: String` and, once submitted, inside
///   `CredentialRequestAction.supply(SecretString)` — never inside
///   `CredentialRequestState` itself (that struct carries only the request's
///   *identity*: which credential, why, and the request id it is bound to).
/// - `SecretString.debugDescription` and `description` never print the
///   wrapped value, so accidental interpolation into a log line or
///   `print()` cannot leak it.
/// - `onResume` hands the raw value back to the caller (the runtime layer
///   responsible for FFI plumbing into the engine's resume path — see the
///   Gaps section) exactly once; the view clears `secretInput` immediately
///   after calling it so the value does not linger in view state longer than
///   needed for the single submit.
/// - Nothing in this file writes the secret to `UserDefaults`, the
///   filesystem, or any transcript/journal type.
///
/// # Single-use binding
///
/// `CredentialRequestState.requestID` is expected to be the same
/// `tool_call_id` the engine's `PendingCredentialRequests` registry
/// (`engine/crates/engine/src/credential_request.rs`) keyed its one-shot
/// channel by. This view enforces single-use the same way
/// `ApprovalInteractionState.submit` does: `canSubmit` flips to `false` the
/// instant a submission starts (`status = .submitting`), so a second tap
/// cannot fire a second resume for the same request id. The engine-side
/// enforcement (removing the channel on first resolve, so even a duplicate
/// call from here cannot redeliver) is the authoritative guarantee; this
/// view's lock is defense in depth against a double-tap, not the boundary
/// itself.
///
/// # Gaps (out of allowlist for this lane)
///
/// - No FFI entry point exists yet from `LocalRuntime`/`CodeRightMobileRuntime`
///   to deliver a `CredentialRequestAction` back into the engine's
///   `PendingCredentialRequests::take_and_resolve`, and no
///   `ProtocolEvent::CredentialRequested` case exists for a daemon-driven
///   (non-local-runtime) session to surface this card in the first place.
///   Both `engine/crates/mobile_runtime/src/ffi.rs` and the protocol crate
///   are outside this lane's allowlist.
///   `apps/coderight-ios/CodeRight/LocalRuntime/LocalRuntime.swift` is where
///   such a bridging call would be added.
/// - This card is not yet wired into any transcript/list view (e.g. wherever
///   `ApprovalInteractionCard` is placed in `LocalSessionView.swift`); doing
///   so requires a session-view-owned array of `CredentialRequestState` and a
///   dispatch point for the (not-yet-existing) daemon event, both outside
///   this lane's allowlist.
struct CredentialRequestState: Identifiable {
    enum Status {
        case pending
        case submitting
        case resolved(supplied: Bool)
        case failed(String)
        case stale
    }

    /// Bound to exactly one pending tool call on the engine side (the
    /// `tool_call_id` key of `PendingCredentialRequests`).
    let id: String
    var requestID: String { id }
    /// Non-secret label for which credential is needed (e.g. "cloudflare",
    /// "flyio", or a provider-specific name) — safe to display and log.
    let credentialLabel: String
    /// Human-readable reason the tool needs it. Must never contain a secret.
    let reason: String
    var status: Status

    init(id: String, credentialLabel: String, reason: String, status: Status = .pending) {
        self.id = id
        self.credentialLabel = credentialLabel
        self.reason = reason
        self.status = status
    }

    var canSubmit: Bool {
        switch status {
        case .pending, .failed: return true
        case .submitting, .resolved, .stale: return false
        }
    }

    @discardableResult
    mutating func beginSubmit() -> Bool {
        guard canSubmit else { return false }
        status = .submitting
        return true
    }

    @discardableResult
    mutating func resolve(supplied: Bool) -> Bool {
        guard case .submitting = status else {
            guard case .pending = status else { return false }
            status = .resolved(supplied: supplied)
            return true
        }
        status = .resolved(supplied: supplied)
        return true
    }

    mutating func fail(_ message: String) {
        if case .resolved = status { return }
        if case .stale = status { return }
        status = .failed(message.isEmpty ? "Unable to submit credential" : message)
    }

    mutating func markStale() {
        if case .resolved = status { return }
        status = .stale
    }
}

/// A secret string that refuses to print itself, mirroring
/// `engine/crates/engine/src/credential_request.rs::SecretValue` on the Rust
/// side. Wrap operator input in this the moment it leaves the `TextField`
/// binding so nothing downstream can accidentally log the raw value via
/// string interpolation.
struct SecretString: CustomStringConvertible, CustomDebugStringConvertible {
    private let value: String

    init(_ value: String) {
        self.value = value
    }

    /// Explicit, deliberate access to the raw secret. Callers must pass the
    /// result only into the resume/FFI path that carries it to the engine —
    /// never into anything that logs, journals, or persists it.
    func expose() -> String { value }

    var description: String { "<redacted>" }
    var debugDescription: String { "SecretString(<redacted>)" }
}

/// The operator's response to a `CredentialRequestState`, handed to
/// `onResume`. Carries the secret only in the `.supply` case, inside a
/// `SecretString`.
enum CredentialRequestAction {
    case supply(SecretString)
    case decline
}

/// Blocking transcript card for a mid-turn credential request. Structurally
/// parallel to `ApprovalInteractionCard` (see that type's header comment) but
/// kept separate because its submitted payload is a secret.
@MainActor
struct CredentialRequestCard: View {
    let state: CredentialRequestState
    let onResume: (CredentialRequestAction) -> Void

    @State private var localState: CredentialRequestState
    @State private var secretInput = ""
    @FocusState private var fieldFocused: Bool

    init(state: CredentialRequestState, onResume: @escaping (CredentialRequestAction) -> Void) {
        self.state = state
        self.onResume = onResume
        _localState = State(initialValue: state)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 7) {
                Image(systemName: "key.fill")
                    .foregroundStyle(Theme.accent2)
                    .accessibilityHidden(true)
                Text("Credential needed")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.text)
                Text(state.credentialLabel)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.muted)
                Spacer(minLength: 4)
                Text(statusLabel)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(statusColor)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Credential request: \(state.credentialLabel)")
            .accessibilityValue(statusLabel)

            if !state.reason.isEmpty {
                Text(state.reason)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if case .failed(let message) = localState.status {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(Theme.danger)
                    .accessibilityLabel("Submission failed: \(message)")
            }
            if case .stale = localState.status {
                Label("This request is no longer active", systemImage: "clock.badge.xmark")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
            }

            controls
        }
        .padding(10)
        .background(Theme.roleTint(.approval), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Theme.accent2.opacity(0.55), lineWidth: 1)
        )
    }

    @ViewBuilder
    private var controls: some View {
        switch localState.status {
        case .pending, .failed:
            HStack(spacing: 8) {
                SecureField("Credential value", text: $secretInput)
                    .textFieldStyle(.roundedBorder)
                    .focused($fieldFocused)
                    .textContentType(.password)
                    .accessibilityLabel("Credential value")
                Button("Send") { supply() }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .disabled(secretInput.isEmpty)
                    .accessibilityLabel("Send credential")
                Button("Skip", role: .cancel) { decline() }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Decline credential request")
            }
        case .submitting:
            ProgressView()
                .accessibilityLabel("Submitting credential")
        case .resolved, .stale:
            EmptyView()
        }
    }

    private func supply() {
        guard localState.beginSubmit() else { return }
        let secret = SecretString(secretInput)
        // Clear local state immediately: the value has been handed off to
        // the resume callback and must not linger in this view's storage.
        secretInput = ""
        onResume(.supply(secret))
    }

    private func decline() {
        guard localState.beginSubmit() else { return }
        secretInput = ""
        onResume(.decline)
    }

    private var statusLabel: String {
        switch localState.status {
        case .pending: return "Needs credential"
        case .submitting: return "Submitting"
        case .resolved(let supplied): return supplied ? "Supplied" : "Declined"
        case .failed: return "Submission failed"
        case .stale: return "No longer available"
        }
    }

    private var statusColor: Color {
        switch localState.status {
        case .failed: return Theme.danger
        case .resolved(let supplied): return supplied ? Theme.accent2 : Theme.danger
        case .pending, .submitting, .stale: return Theme.muted
        }
    }
}
