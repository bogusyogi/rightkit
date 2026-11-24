import Foundation

#if os(macOS)

/// Keeps an app extension (a Finder Sync extension, say) from running while its visible app is not,
/// and brings it back at the next launch only for a user who had it on (PL-34).
///
/// `suspendAtQuit()` disables an enabled extension; `resumeAtLaunch()` enables it again when the user had
/// it enabled. The user's choice is remembered in two flags in the `UserDefaults` the host passes:
/// "was enabled" (it was on when the app last quit or launched) and "resume failed" (the last launch could
/// not turn it back on, so an "off" read at the next Quit is not the user's choice). An extension the user
/// turned off, or one macOS never registered (every build that is signed but not notarized), is never
/// touched. An app that is killed cannot run `suspendAtQuit`; the next launch turns the extension on only
/// for a user who had it on, and the next Quit ends it.
///
/// Both entry points run on one serial queue, so a quick Quit cannot interleave with the launch resume's
/// reads and writes of the flags. Each call can take up to two `timeout`s; call them off the main thread.
public final class RightKitExtensionLifecycle: @unchecked Sendable {
    public struct Outcome: Sendable, Equatable {
        /// Whether the extension was switched (on at launch, off at Quit).
        public var changed: Bool
        /// Whether the `pluginkit` call that was made exited 0 in time (nil when none was made).
        public var ok: Bool?
    }

    private let identifier: String
    private let defaults: UserDefaults
    private let wasEnabledKey: String
    private let resumeFailedKey: String
    private let timeout: TimeInterval
    private let status: @Sendable (String, TimeInterval) -> RightKitPermissionStatus
    private let set: @Sendable (String, Bool, TimeInterval) -> Bool
    private let queue: DispatchQueue

    /// - Parameters:
    ///   - identifier: the extension's bundle identifier.
    ///   - defaults: where the two flags live.
    ///   - wasEnabledKey / resumeFailedKey: the flag names (a host keeps the names it already wrote).
    ///   - timeout: the bound of each `pluginkit` call (Pulse uses 3 s).
    ///   - status / set: the probes; the defaults call `RightKitPermissionProbes`.
    public init(
        identifier: String,
        defaults: UserDefaults = .standard,
        wasEnabledKey: String,
        resumeFailedKey: String,
        timeout: TimeInterval = 3,
        status: @escaping @Sendable (String, TimeInterval) -> RightKitPermissionStatus = {
            RightKitPermissionProbes.extensionStatus(identifier: $0, timeout: $1)
        },
        set: @escaping @Sendable (String, Bool, TimeInterval) -> Bool = {
            RightKitPermissionProbes.setExtension(identifier: $0, enabled: $1, timeout: $2)
        }
    ) {
        self.identifier = identifier
        self.defaults = defaults
        self.wasEnabledKey = wasEnabledKey
        self.resumeFailedKey = resumeFailedKey
        self.timeout = timeout
        self.status = status
        self.set = set
        self.queue = DispatchQueue(label: "rightkit.extension-lifecycle.\(identifier)")
    }

    /// Call off the main thread at launch. Returns what it did (`changed` is true when it turned the
    /// extension on).
    @discardableResult
    public func resumeAtLaunch() -> Outcome {
        queue.sync {
            switch status(identifier, timeout) {
            case .granted:
                defaults.set(true, forKey: wasEnabledKey)
                defaults.set(false, forKey: resumeFailedKey)
                return Outcome(changed: false, ok: nil)
            case .off:
                guard defaults.bool(forKey: wasEnabledKey) else { return Outcome(changed: false, ok: nil) }
                let ok = set(identifier, true, timeout)
                defaults.set(!ok, forKey: resumeFailedKey)
                return Outcome(changed: true, ok: ok)
            default:
                return Outcome(changed: false, ok: nil)
            }
        }
    }

    /// Call at Quit, after the app's children are stopped. Returns what it did (`changed` is true when it
    /// turned the extension off).
    @discardableResult
    public func suspendAtQuit() -> Outcome {
        queue.sync {
            switch status(identifier, timeout) {
            case .granted:
                defaults.set(true, forKey: wasEnabledKey)
                let ok = set(identifier, false, timeout)
                return Outcome(changed: true, ok: ok)
            case .off:
                // Off at Quit is the user's choice unless the launch could not turn it back on.
                if !defaults.bool(forKey: resumeFailedKey) { defaults.set(false, forKey: wasEnabledKey) }
                return Outcome(changed: false, ok: nil)
            default:
                return Outcome(changed: false, ok: nil)
            }
        }
    }
}

#endif
