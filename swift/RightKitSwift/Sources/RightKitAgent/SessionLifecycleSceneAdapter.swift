@preconcurrency import Foundation

/*
 Provenance (exact): openmausbot commit
 `6951c40156ba0916a10b6e49e5051b7785198b93`,
 `ios/App/Session.swift:296-318` forwards foreground recovery to `connect()`,
 while `ios/App/Session.swift:367-412` owns background teardown and its
 bounded linger timer. This adapter keeps those edges injectable; it does not
 change CodeRightApp or its existing notification names.
*/

/// Notification names reserved for an eventual app-root integration. They are
/// intentionally separate from CodeRight's current shared notifications so
/// this port can be staged without changing unrelated files.
public enum SessionLifecycleSceneNotifications {
    public static let didBecomeActive = Notification.Name(
        "CodeRight.Porting.SessionLifecycle.didBecomeActive"
    )
    public static let didEnterBackground = Notification.Name(
        "CodeRight.Porting.SessionLifecycle.didEnterBackground"
    )
}

/// Bridges app lifecycle notifications to the isolated scene-handling
/// protocol. Passing notification names lets an integration map existing
/// `scenePhase` posts without coupling this port to a particular app root.
@MainActor
public final class SessionLifecycleSceneAdapter {
    private weak var handler: (any SessionLifecycleSceneHandling)?
    private let notificationCenter: NotificationCenter
    private var observerTokens: [NSObjectProtocol] = []

    public init(
        handler: any SessionLifecycleSceneHandling,
        notificationCenter: NotificationCenter = .default,
        activeNotification: Notification.Name = SessionLifecycleSceneNotifications.didBecomeActive,
        backgroundNotification: Notification.Name = SessionLifecycleSceneNotifications.didEnterBackground
    ) {
        self.handler = handler
        self.notificationCenter = notificationCenter
        observerTokens = [
            notificationCenter.addObserver(
                forName: activeNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.handler?.sessionLifecycleDidBecomeActive()
                }
            },
            notificationCenter.addObserver(
                forName: backgroundNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.handler?.sessionLifecycleDidEnterBackground()
                }
            }
        ]
    }

    deinit {
        for token in observerTokens {
            notificationCenter.removeObserver(token)
        }
    }
}
