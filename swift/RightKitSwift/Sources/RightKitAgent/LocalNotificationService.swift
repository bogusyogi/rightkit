#if os(iOS)
import Foundation
import UserNotifications

/// What happened to a local session that is worth surfacing while the app is backgrounded
/// or the session isn't the one currently on screen (Atom 45 — local-only for now, no APNs).
public enum LocalSessionNotificationKind: Equatable, Sendable {
    /// The agent turn finished on its own (no pending approval).
    case turnCompleted
    /// The agent is blocked on an approval, question, or plan confirmation.
    case needsInput
}

/// Posts on-device local notifications for local-runtime session lifecycle events.
///
/// Deliberately `UNUserNotificationCenter` local notifications only — no APNs, no server
/// round-trip, matching Atom 45's phased scope ("local completion/needs-input first, APNs
/// later"). Callers (e.g. `LocalSessionViewModel`, `LocalContinuedProcessingController`) call
/// `notify` once per lifecycle event; this service does not itself observe the runtime, so it
/// stays reusable across local and (future) other session kinds without depending on their
/// concrete types.
public protocol LocalNotificationCenter: Sendable {
    func requestAuthorization(options: UNAuthorizationOptions) async throws -> Bool
    func add(_ request: UNNotificationRequest) async throws
    func authorizationStatus() async -> UNAuthorizationStatus
}

// `requestAuthorization(options:)` and `add(_:)` are satisfied directly by
// `UNUserNotificationCenter`'s own async APIs — only `authorizationStatus()` needs an
// adapter, since the real API surfaces it via `notificationSettings()`.
extension UNUserNotificationCenter: LocalNotificationCenter {
    public func authorizationStatus() async -> UNAuthorizationStatus {
        await notificationSettings().authorizationStatus
    }
}

public actor LocalNotificationService {
    /// Key placed in the notification's `userInfo` under this identifier, carrying the
    /// session id so a tap handler can deep-link back via `DeepLinkRoute.session(id:)`
    /// without this service needing to know about routing.
    public static let sessionIdUserInfoKey = "sessionId"

    private let center: any LocalNotificationCenter
    private var didRequestAuthorization = false

    public init(center: any LocalNotificationCenter = UNUserNotificationCenter.current()) {
        self.center = center
    }

    /// Ask for permission once per process lifetime; a denial is left to Settings, never
    /// re-prompted from here.
    @discardableResult
    public func requestAuthorizationIfNeeded() async -> Bool {
        guard !didRequestAuthorization else {
            return await center.authorizationStatus() == .authorized
        }
        didRequestAuthorization = true
        return (try? await center.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
    }

    /// Post exactly one local notification for this event. Uses `sessionId + kind` as the
    /// notification identifier so a rapid duplicate call (e.g. two drains of the same
    /// terminal event) replaces rather than stacks.
    public func notify(
        sessionId: String,
        title: String,
        kind: LocalSessionNotificationKind
    ) async {
        guard await center.authorizationStatus() == .authorized else { return }

        let content = UNMutableNotificationContent()
        content.title = kind == .needsInput ? "Needs your input" : "Turn complete"
        content.body = title
        content.sound = .default
        content.userInfo = [Self.sessionIdUserInfoKey: sessionId]

        let identifier = "local-session-\(sessionId)-\(kind == .needsInput ? "needs-input" : "completed")"
        let request = UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        try? await center.add(request)
    }
}
#endif
