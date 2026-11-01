#if os(macOS)
import AppKit
import Foundation
import UserNotifications

/// Where every notification goes: the notch (its peek, its cards, its
/// sounds) or a banner in Notification Center. One choice, because the
/// reasons for either hold for all of them at once: the notch is quiet and
/// stays out of Notification Center, a banner reaches a second display and a
/// hidden notch. Which events notify at all stays a switch per event.
enum NotificationChannel: String, CaseIterable, Identifiable {
    case notch, mac
    var id: String { rawValue }

    var title: String {
        switch self {
        case .notch: return L10n.t("In the notch")
        case .mac:   return L10n.t("Mac notifications")
        }
    }

    var explanation: String {
        switch self {
        case .notch: return L10n.t("The notch opens for a moment or shows a card, with the sound you chose. Nothing reaches Notification Center.")
        case .mac:   return L10n.t("A banner in Notification Center, which reaches you on another display or with the notch hidden. The notch stays quiet; a banner makes a sound only with Play a sound on banners on.")
        }
    }
}

/// The Mac side of the channel: banners for the events that had only the
/// notch, and a test button that answers whatever state the permission is in.
enum ChannelNotifications {
    /// Without a delegate, macOS delivers an app's own notifications quietly
    /// to the list while that app is frontmost: the test sent from Settings,
    /// with the Settings window in front, never showed. The delegate asks for
    /// the banner and the sound whatever is in front.
    private final class Presenter: NSObject, UNUserNotificationCenterDelegate {
        func userNotificationCenter(_ center: UNUserNotificationCenter,
                                    willPresent notification: UNNotification,
                                    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
            // The sound is presented only when the banner carries one (Play a sound on banners).
            completionHandler(notification.request.content.sound == nil
                              ? [.banner, .list]
                              : [.banner, .list, .sound])
        }
    }
    private static let presenter = Presenter()

    static func installPresenter() {
        UNUserNotificationCenter.current().delegate = presenter
    }

    /// Asked when the Mac channel is chosen, so the first banner is not also
    /// the first permission dialog. Never on the notch channel: someone who
    /// keeps everything in the notch is never asked about banners.
    static func requestAuthorizationIfNeeded() {
        let center = UNUserNotificationCenter.current()
        center.getNotificationSettings { settings in
            guard settings.authorizationStatus == .notDetermined else { return }
            center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
        }
    }

    /// A banner; or, when banners are switched off for the app in System
    /// Settings, that pane, because a test that shows nothing and says
    /// nothing reads as broken.
    static func test(sound: Bool) {
        let center = UNUserNotificationCenter.current()
        center.getNotificationSettings { settings in
            switch settings.authorizationStatus {
            case .notDetermined:
                center.requestAuthorization(options: [.alert, .sound]) { ok, _ in
                    if ok { post(title: L10n.t("the app test"), body: L10n.t("This is what one looks like."), sound: sound) }
                }
            case .denied:
                DispatchQueue.main.async {
                    let id = Bundle.main.bundleIdentifier ?? "dev.orthic.pulse"
                    if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(id)") {
                        NSWorkspace.shared.open(url)
                    }
                }
            default:
                post(title: L10n.t("the app test"), body: L10n.t("This is what one looks like."), sound: sound)
            }
        }
    }

    static func sessionEnded(name: String, blocked: Bool, sound: Bool) {
        post(title: blocked ? L10n.t("\(name) is waiting on you") : L10n.t("\(name) finished"),
             body: blocked ? L10n.t("The agent stopped to ask you something.") : L10n.t("The agent's turn is done."),
             thread: "session|\(name)", sound: sound)
    }

    /// A banner. Its sound is the default alert sound only when `sound` is on (Preferences
    /// `channelBannerSound`, off by default); otherwise the banner is silent.
    static func post(title: String, body: String, thread: String? = nil, sound: Bool = false) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = sound ? UNNotificationSound.default : nil
        Log.record(.usage, sound ? "notification.posted_with_sound" : "notification.posted")
        if let thread { content.threadIdentifier = thread }
        let identifier = "channel|\(thread ?? "")|\(Int(Date().timeIntervalSince1970))"
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil)) { error in
            if let error {
                Log.usage.error("notification not accepted: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
#endif
