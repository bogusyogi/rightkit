import Foundation
@preconcurrency import AVFoundation

/// Why the audio route changed. Mirrors `AVAudioSession.RouteChangeReason` without importing it,
/// so the mapping is testable on any platform.
public enum AudioRouteChangeReason: Sendable, Equatable {
    case unknown
    case newDeviceAvailable
    case oldDeviceUnavailable
    case categoryChange
    case override
    case wakeFromSleep
    case noSuitableRouteForCategory
    case routeConfigurationChange

    /// Maps the raw `AVAudioSessionRouteChangeReasonKey` value. Unknown values map to `.unknown`.
    public init(rawValue: UInt) {
        switch rawValue {
        case 1: self = .newDeviceAvailable
        case 2: self = .oldDeviceUnavailable
        case 3: self = .categoryChange
        case 4: self = .override
        case 6: self = .wakeFromSleep
        case 7: self = .noSuitableRouteForCategory
        case 8: self = .routeConfigurationChange
        default: self = .unknown
        }
    }

    /// True when the change takes away the input the session was recording from (headset unplugged,
    /// Bluetooth microphone gone, no route left). A recorder that was running should stop or rebuild.
    public var losesActiveInput: Bool {
        self == .oldDeviceUnavailable || self == .noSuitableRouteForCategory
    }
}

/// One audio-session lifecycle event, normalized across iOS and watchOS.
public enum AudioSessionEvent: Sendable, Equatable {
    /// The system took the session (phone call, Siri, alarm, another app). Recording has stopped or paused.
    case interruptionBegan
    /// The interruption is over. `shouldResume` is the system's recommendation.
    case interruptionEnded(shouldResume: Bool)
    case routeChanged(AudioRouteChangeReason)
    /// `mediaserverd` died. Every audio object is invalid; rebuild engines and recorders.
    case mediaServicesLost
    /// `mediaserverd` restarted. The session must be reconfigured and reactivated.
    case mediaServicesReset
}

/// Pure translation of notification payload values into ``AudioSessionEvent``. Takes raw integers so
/// it needs no `AVAudioSession` and runs on macOS CI.
public enum AudioSessionEventMapper {
    /// `AVAudioSessionInterruptionTypeKey`: 1 = began, 0 = ended. `AVAudioSessionInterruptionOptionKey`:
    /// bit 0 = should resume. Returns nil when the type is missing or unknown.
    public static func interruption(typeRaw: UInt?, optionsRaw: UInt?) -> AudioSessionEvent? {
        switch typeRaw {
        case 1: return .interruptionBegan
        case 0: return .interruptionEnded(shouldResume: (optionsRaw ?? 0) & 1 == 1)
        default: return nil
        }
    }

    /// `AVAudioSessionRouteChangeReasonKey`.
    public static func routeChange(reasonRaw: UInt?) -> AudioSessionEvent? {
        guard let reasonRaw else { return nil }
        return .routeChanged(AudioRouteChangeReason(rawValue: reasonRaw))
    }
}

/// Observes `AVAudioSession` interruption, route-change and media-services notifications and reports
/// them as ``AudioSessionEvent``. On platforms without `AVAudioSession` (macOS) it is inert.
///
/// Events arrive on an arbitrary queue. On watchOS 27 the monitor also reports a system deactivation as
/// ``AudioSessionEvent/interruptionBegan`` and a resumption recommendation as
/// ``AudioSessionEvent/interruptionEnded(shouldResume:)``.
public final class AudioSessionMonitor: @unchecked Sendable {
    private let lock = NSLock()
    private let handler: @Sendable (AudioSessionEvent) -> Void
    private var observers: [NSObjectProtocol] = []

    public init(handler: @escaping @Sendable (AudioSessionEvent) -> Void) {
        self.handler = handler
    }

    deinit { stop() }

    /// True while observers are installed.
    public var isObserving: Bool { lock.withLock { !observers.isEmpty } }

    /// Installs the observers. Calling it again while observing is a no-op.
    public func start() {
        #if os(iOS) || os(watchOS) || os(tvOS)
        let alreadyObserving = lock.withLock { !observers.isEmpty }
        guard !alreadyObserving else { return }
        let center = NotificationCenter.default
        let session = AVAudioSession.sharedInstance()
        let handler = self.handler
        var installed: [NSObjectProtocol] = []

        installed.append(center.addObserver(
            forName: AVAudioSession.routeChangeNotification, object: session, queue: nil,
        ) { notification in
            let raw = (notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? NSNumber)?.uintValue
            if let event = AudioSessionEventMapper.routeChange(reasonRaw: raw) { handler(event) }
        })
        installed.append(center.addObserver(
            forName: AVAudioSession.mediaServicesWereLostNotification, object: session, queue: nil,
        ) { _ in handler(.mediaServicesLost) })
        installed.append(center.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification, object: session, queue: nil,
        ) { _ in handler(.mediaServicesReset) })

        #if os(watchOS)
        if #available(watchOS 27.0, *) {
            installed.append(center.addObserver(
                forName: AVAudioSession.didBecomeInactiveNotification, object: session, queue: nil,
            ) { notification in
                guard let context = notification.userInfo?[AVAudioSession.deactivationContextKey]
                    as? AVAudioSession.DeactivationContext, context.source == .system else { return }
                handler(.interruptionBegan)
            })
            installed.append(center.addObserver(
                forName: AVAudioSession.resumptionRecommendationNotification, object: session, queue: nil,
            ) { notification in
                var shouldResume = false
                if let context = notification.userInfo?[AVAudioSession.resumptionContextKey]
                    as? AVAudioSession.ResumptionContext {
                    shouldResume = context.recommendation.rawValue == 1
                }
                handler(.interruptionEnded(shouldResume: shouldResume))
            })
        } else {
            installed.append(Self.interruptionObserver(center: center, session: session, handler: handler))
        }
        #else
        installed.append(Self.interruptionObserver(center: center, session: session, handler: handler))
        #endif

        let extra = lock.withLock { () -> [NSObjectProtocol] in
            if observers.isEmpty { observers = installed; return [] }
            return installed
        }
        for observer in extra { center.removeObserver(observer) }  // lost a start() race
        #endif
    }

    /// Removes the observers. Safe to call repeatedly.
    public func stop() {
        let removed = lock.withLock { () -> [NSObjectProtocol] in
            defer { observers = [] }
            return observers
        }
        for observer in removed { NotificationCenter.default.removeObserver(observer) }
    }

    #if os(iOS) || os(watchOS) || os(tvOS)
    private static func interruptionObserver(
        center: NotificationCenter, session: AVAudioSession,
        handler: @escaping @Sendable (AudioSessionEvent) -> Void,
    ) -> NSObjectProtocol {
        center.addObserver(forName: AVAudioSession.interruptionNotification, object: session, queue: nil) { notification in
            let type = (notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? NSNumber)?.uintValue
            let options = (notification.userInfo?[AVAudioSessionInterruptionOptionKey] as? NSNumber)?.uintValue
            if let event = AudioSessionEventMapper.interruption(typeRaw: type, optionsRaw: options) { handler(event) }
        }
    }
    #endif
}
