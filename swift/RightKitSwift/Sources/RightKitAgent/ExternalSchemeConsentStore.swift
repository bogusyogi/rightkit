import Foundation
import Observation

/// ENG-113: per-scheme, user-confirmed allowlist for opening a non-http(s) URL scheme or
/// handing off to another app from embedded web content (the client-owned browser surface,
/// agent tool output that surfaces a link, or any other untrusted-content path).
///
/// The remembered decision is keyed by scheme alone (`mailto`, `tel`, `myapp`, ...), matching
/// `ExternalSchemeClassifier.consentKey`. A decision is never inferred from page
/// content, an agent's claim, or a prior session's route — it is written exactly once, by
/// `recordDecision`, and only ever from a human answering `ExternalSchemeConsentPrompt`.
/// Nothing in this file can produce a decision on its own; callers that need a fresh choice
/// call `presentIfNeeded` and always get `nil` back until a person has actually answered.
@MainActor
@Observable
public final class ExternalSchemeConsentStore {
    /// One remembered answer for one scheme.
    public struct Decision: Codable, Equatable, Sendable {
        public enum Choice: String, Codable, Sendable {
            case allow
            case deny
        }

        public let scheme: String
        public let choice: Choice
        public let decidedAt: Date

        public init(scheme: String, choice: Choice, decidedAt: Date = Date()) {
            self.scheme = scheme
            self.choice = choice
            self.decidedAt = decidedAt
        }
    }

    /// Persistence seam so tests never touch real `UserDefaults` — mirrors `DaemonPersisting`
    /// in `DaemonStore.swift`.
    public protocol Persisting: Sendable {
        func loadDecisions() -> [String: Decision]
        func saveDecisions(_ decisions: [String: Decision])
    }

    /// A scheme currently awaiting a fresh human decision. Presenting this is the ONLY path
    /// that can lead to an `.allow` outcome for a scheme with no remembered decision yet — a
    /// page requesting a scheme, or an agent naming one, only ever populates this; it never
    /// writes to `decisions` directly.
    public private(set) var pendingRequest: PendingRequest?

    /// Every remembered per-scheme decision, keyed by lowercased scheme.
    public private(set) var decisions: [String: Decision]

    public struct PendingRequest: Identifiable, Sendable {
        public let id = UUID()
        public let scheme: String
        public let url: URL
        public let origin: String?

        public init(scheme: String, url: URL, origin: String?) {
            self.scheme = scheme
            self.url = url
            self.origin = origin
        }
    }

    private let persistence: any Persisting
    private var continuations: [String: (Bool) -> Void] = [:]

    public convenience init() {
        self.init(persistence: UserDefaultsPersistence())
    }

    public init(persistence: any Persisting) {
        self.persistence = persistence
        self.decisions = persistence.loadDecisions()
    }

    /// Existing remembered decision, if any, for the scheme of `url`. `nil` means "never
    /// decided" — callers must not treat that as either allow or deny.
    public func remembered(for url: URL) -> Decision? {
        guard let key = ExternalSchemeClassifier.consentKey(for: url) else { return nil }
        return decisions[key]
    }

    /// Resolve whether `url` may be handed off, asking the human via `pendingRequest` when
    /// there is no remembered decision yet. Returns `true` only when the scheme is allowed —
    /// by a prior remembered decision or a fresh one just recorded. A page or agent calling
    /// this repeatedly for the same scheme gets exactly one prompt; the rest resolve from the
    /// remembered decision, matching ENG-113's "remembered per scheme" contract.
    ///
    /// This function performs no navigation, no `UIApplication.open`, and no scheme handoff
    /// itself — it only returns a boolean. The caller (e.g. `BrowserControlSurfaceView`) is
    /// responsible for the actual handoff, and must not perform it unless this returns `true`.
    public func resolve(for url: URL, origin: String?) async -> Bool {
        guard ExternalSchemeClassifier.requiresExternalConsent(url) else {
            // http(s) and the app's own scheme never reach this store.
            return true
        }
        guard let key = ExternalSchemeClassifier.consentKey(for: url) else { return false }
        if let remembered = decisions[key] {
            return remembered.choice == .allow
        }
        return await withCheckedContinuation { continuation in
            continuations[key] = { allowed in continuation.resume(returning: allowed) }
            pendingRequest = PendingRequest(scheme: key, url: url, origin: origin)
        }
    }

    /// Record the human's answer for the scheme currently pending, persist it, and resolve
    /// every caller waiting on `resolve(for:origin:)` for that scheme. `remember == false`
    /// still resolves the in-flight request (as denied or a one-time allow) without writing a
    /// durable decision, for a user who wants to decide again next time.
    public func recordDecision(choice: Decision.Choice, remember: Bool) {
        guard let request = pendingRequest else { return }
        pendingRequest = nil
        if remember {
            let decision = Decision(scheme: request.scheme, choice: choice)
            decisions[request.scheme] = decision
            persistence.saveDecisions(decisions)
        }
        continuations.removeValue(forKey: request.scheme)?(choice == .allow)
    }

    /// Dismiss without a decision — resolves as denied so a swiped-away prompt can never be
    /// mistaken for consent, and never persists anything.
    public func dismissPending() {
        guard let request = pendingRequest else { return }
        pendingRequest = nil
        continuations.removeValue(forKey: request.scheme)?(false)
    }

    /// Forget a remembered decision so the next encounter prompts again.
    public func forget(scheme: String) {
        let key = scheme.lowercased()
        guard decisions.removeValue(forKey: key) != nil else { return }
        persistence.saveDecisions(decisions)
    }
}

extension ExternalSchemeConsentStore {
    /// Default `UserDefaults`-backed persistence. Scheme consent is not a credential — it is
    /// a UI preference — so it belongs in `UserDefaults`, not the Keychain `DaemonStore` uses
    /// for pairing secrets.
    struct UserDefaultsPersistence: Persisting {
        private static let key = "com.coderight.externalSchemeConsent.v1"
        nonisolated(unsafe) private let defaults: UserDefaults

        init(defaults: UserDefaults = .standard) {
            self.defaults = defaults
        }

        func loadDecisions() -> [String: Decision] {
            guard let data = defaults.data(forKey: Self.key) else { return [:] }
            return (try? JSONDecoder().decode([String: Decision].self, from: data)) ?? [:]
        }

        func saveDecisions(_ decisions: [String: Decision]) {
            guard let data = try? JSONEncoder().encode(decisions) else { return }
            defaults.set(data, forKey: Self.key)
        }
    }
}
