import Foundation

#if os(macOS)
import AppKit
import UserNotifications

/// Notification permission as the app sees it, independent of `UserNotifications` types.
public enum RightKitNotificationAuthorization: String, Sendable, Equatable {
    case notDetermined
    case denied
    case authorized
    /// Delivered quietly to Notification Center (provisional authorization).
    case provisional
    /// Not available here: no app bundle (a test host or command-line tool), where
    /// `UNUserNotificationCenter` would trap.
    case unavailable

    init(_ status: UNAuthorizationStatus) {
        switch status {
        case .notDetermined: self = .notDetermined
        case .denied: self = .denied
        case .authorized: self = .authorized
        case .provisional: self = .provisional
        @unknown default: self = .denied
        }
    }

    /// Whether a banner can be delivered.
    public var canDeliver: Bool { self == .authorized || self == .provisional }
}

/// One banner. `sound` is the default alert sound only when true; otherwise the banner is silent.
public struct RightKitBanner: Equatable, Sendable {
    public var title: String
    public var body: String
    /// Notifications with the same thread stack together in Notification Center.
    public var thread: String?
    public var sound: Bool
    /// Prefix of the request identifier (default `banner`).
    public var identifierPrefix: String

    public init(title: String, body: String, thread: String? = nil, sound: Bool = false, identifierPrefix: String = "banner") {
        self.title = title
        self.body = body
        self.thread = thread
        self.sound = sound
        self.identifierPrefix = identifierPrefix
    }

    /// `<prefix>|<thread>|<epoch seconds>`: unique per second per thread, so a repeat within the same
    /// second replaces rather than stacks. Pure.
    public func identifier(now: Date) -> String {
        "\(identifierPrefix)|\(thread ?? "")|\(Int(now.timeIntervalSince1970))"
    }
}

/// What a "send me a test notification" button should do for each permission state.
public enum RightKitBannerTestAction: Equatable, Sendable {
    /// Ask the system (once), then post if granted.
    case requestThenPost
    /// Banners are off for this app: show the Notifications pane, because a test that shows nothing
    /// and says nothing reads as broken.
    case openSettings
    case post
    case unavailable

    public static func action(for authorization: RightKitNotificationAuthorization) -> RightKitBannerTestAction {
        switch authorization {
        case .notDetermined: .requestThenPost
        case .denied: .openSettings
        case .authorized, .provisional: .post
        case .unavailable: .unavailable
        }
    }
}

public enum RightKitBannerTestOutcome: Equatable, Sendable {
    case posted
    case deniedOpenedSettings
    case notGranted
    case unavailable
    case failed(String)
}

public enum RightKitBannerError: Error, Equatable, Sendable {
    case unavailable
    case notAuthorized(RightKitNotificationAuthorization)
    case rejected(String)
}

/// Banners through Notification Center. Permission is requested only through the standard system
/// prompt (`requestAuthorization`), and only when asked for and still undetermined; nothing here
/// changes a grant.
public enum RightKitBanners {
    /// True when the process is an app bundle, the only place `UNUserNotificationCenter` is usable.
    public static var isAvailable: Bool {
        Bundle.main.bundleIdentifier != nil && Bundle.main.bundleURL.pathExtension == "app"
    }

    /// Without a delegate, macOS delivers an app's own notifications quietly to the list while that
    /// app is frontmost. This presenter asks for the banner (and the sound, only when the banner
    /// carries one) whatever is in front.
    private final class Presenter: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
        func userNotificationCenter(
            _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        ) async -> UNNotificationPresentationOptions {
            RightKitBanners.presentationOptions(hasSound: notification.request.content.sound != nil)
        }
    }

    private static let presenter = Presenter()

    /// The banner options for a notification, with the sound only when it carries one.
    static func presentationOptions(hasSound: Bool) -> UNNotificationPresentationOptions {
        hasSound ? [.banner, .list, .sound] : [.banner, .list]
    }

    /// Installs the presenter as the center's delegate. Call once, early (before the first banner).
    public static func installPresenter() {
        guard isAvailable else { return }
        UNUserNotificationCenter.current().delegate = presenter
    }

    public static func authorization() async -> RightKitNotificationAuthorization {
        guard isAvailable else { return .unavailable }
        return await withCheckedContinuation { continuation in
            UNUserNotificationCenter.current().getNotificationSettings { settings in
                continuation.resume(returning: RightKitNotificationAuthorization(settings.authorizationStatus))
            }
        }
    }

    /// Asks the system for permission only when it is still undetermined, so the first banner is not
    /// also the first permission dialog. Returns the resulting state. Never call it for someone who
    /// keeps notifications elsewhere.
    @discardableResult
    public static func requestAuthorizationIfNeeded(options: UNAuthorizationOptions = [.alert, .sound]) async
        -> RightKitNotificationAuthorization
    {
        let current = await authorization()
        guard current == .notDetermined else { return current }
        let granted: Bool = await withCheckedContinuation { continuation in
            UNUserNotificationCenter.current().requestAuthorization(options: options) { granted, _ in
                continuation.resume(returning: granted)
            }
        }
        _ = granted
        return await authorization()
    }

    /// Posts a banner. Throws `unavailable` outside an app bundle, `notAuthorized` when the user has
    /// not allowed notifications (nothing is asked), `rejected` when the system refuses the request.
    public static func post(_ banner: RightKitBanner, now: Date = Date()) async throws {
        let state = await authorization()
        guard state != .unavailable else { throw RightKitBannerError.unavailable }
        guard state.canDeliver else { throw RightKitBannerError.notAuthorized(state) }
        let content = UNMutableNotificationContent()
        content.title = banner.title
        content.body = banner.body
        content.sound = banner.sound ? UNNotificationSound.default : nil
        if let thread = banner.thread { content.threadIdentifier = thread }
        let request = UNNotificationRequest(identifier: banner.identifier(now: now), content: content, trigger: nil)
        let failure: String? = await withCheckedContinuation { continuation in
            UNUserNotificationCenter.current().add(request) { error in
                continuation.resume(returning: error?.localizedDescription)
            }
        }
        if let failure { throw RightKitBannerError.rejected(failure) }
    }

    /// A test banner that answers whatever state the permission is in: asks once if undetermined,
    /// posts if allowed, opens the app's Notifications pane if denied.
    @MainActor
    public static func sendTest(_ banner: RightKitBanner) async -> RightKitBannerTestOutcome {
        let state = await authorization()
        switch RightKitBannerTestAction.action(for: state) {
        case .unavailable:
            return .unavailable
        case .openSettings:
            openNotificationSettings()
            return .deniedOpenedSettings
        case .requestThenPost:
            guard await requestAuthorizationIfNeeded().canDeliver else { return .notGranted }
            return await postForTest(banner)
        case .post:
            return await postForTest(banner)
        }
    }

    private static func postForTest(_ banner: RightKitBanner) async -> RightKitBannerTestOutcome {
        do {
            try await post(banner)
            return .posted
        } catch {
            return .failed(String(describing: error))
        }
    }

    /// The System Settings URL for this app's notification switches.
    public static func notificationSettingsURL(bundleIdentifier: String) -> URL? {
        URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(bundleIdentifier)")
    }

    @MainActor
    public static func openNotificationSettings() {
        guard let id = Bundle.main.bundleIdentifier, let url = notificationSettingsURL(bundleIdentifier: id) else { return }
        NSWorkspace.shared.open(url)
    }
}

/// Fires an alert once when a metric crosses a threshold upward, never again while it stays above,
/// and starts over when it drops back below the first threshold. Pure state, no notifications, so the
/// rule is testable without touching Notification Center.
///
/// The first reading for an id only records. Every id arrives with no history at launch, and that
/// reading is often an archived one already past a threshold; treating it as a crossing rings
/// "limit reached" on every start. A stale (archived) reading is not a baseline either: it makes the
/// tracker forget the id, so the first live reading after it only records.
public struct RightKitThresholdTracker: Equatable, Sendable {
    public let thresholds: [Int]
    private var crossed: [String: Int] = [:]

    /// `thresholds` are percentages, ascending (default 80 and 100).
    public init(thresholds: [Int] = [80, 100]) { self.thresholds = thresholds.sorted() }

    /// The thresholds newly crossed by this reading, ascending (a jump from 50 to 100 crosses both).
    /// A muted id still updates its level, so un-muting does not replay old crossings.
    public mutating func observe(id: String, percent: Double?, isStale: Bool = false, muted: Bool = false) -> [Int] {
        guard !isStale else {
            crossed.removeValue(forKey: id)
            return []
        }
        guard let percent else { return [] }
        let level = thresholds.last { Double($0) <= percent } ?? 0
        guard let previous = crossed[id] else {
            crossed[id] = level
            return []
        }
        crossed[id] = level
        guard level > previous, !muted else { return [] }
        return thresholds.filter { $0 > previous && $0 <= level }
    }

    public mutating func forget(id: String) { crossed.removeValue(forKey: id) }
}
#endif
