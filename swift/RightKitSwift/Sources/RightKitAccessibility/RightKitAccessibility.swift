import Foundation

#if os(macOS)
import ApplicationServices
import CoreGraphics

/// A failed Accessibility call, mapped from `AXError` so callers can switch on it without
/// importing ApplicationServices. Raw codes are the stable `kAXError*` values.
public enum RightKitAXFailure: Error, Equatable, Sendable {
    case failure
    case illegalArgument
    case invalidElement
    case invalidObserver
    case cannotComplete
    case attributeUnsupported
    case actionUnsupported
    case notificationUnsupported
    case notImplemented
    case notificationAlreadyRegistered
    case notificationNotRegistered
    /// Accessibility permission is not granted (or the API is disabled).
    case apiDisabled
    /// The attribute exists but has no value (for example no focused window).
    case noValue
    case other(Int32)

    public init(code: Int32) {
        switch code {
        case -25200: self = .failure
        case -25201: self = .illegalArgument
        case -25202: self = .invalidElement
        case -25203: self = .invalidObserver
        case -25204: self = .cannotComplete
        case -25205: self = .attributeUnsupported
        case -25206: self = .actionUnsupported
        case -25207: self = .notificationUnsupported
        case -25208: self = .notImplemented
        case -25209: self = .notificationAlreadyRegistered
        case -25210: self = .notificationNotRegistered
        case -25211: self = .apiDisabled
        case -25212: self = .noValue
        default: self = .other(code)
        }
    }
}

/// Process-level Accessibility trust.
public enum RightKitAXTrust {
    /// Whether this process may use the Accessibility API. With `prompt` true macOS shows its
    /// system prompt once if trust is missing.
    public static func isTrusted(prompt: Bool = false) -> Bool {
        // The literal is the value of `kAXTrustedCheckOptionPrompt`; the C global is not
        // concurrency-safe under Swift 6.
        let options = ["AXTrustedCheckOptionPrompt": prompt] as CFDictionary
        return AXIsProcessTrustedWithOptions(options)
    }
}

/// A thin, typed handle on one `AXUIElement`. Reads return `nil` or throw `RightKitAXFailure`;
/// writes throw. `AXUIElement` is an immutable, thread-safe Core Foundation object, so the handle
/// is `Sendable`, but Accessibility calls block on the target app: do not make them on the main
/// thread against an app that may be hung unless `setMessagingTimeout` is set.
public struct RightKitAXElement: @unchecked Sendable, Hashable {
    public let raw: AXUIElement

    public init(_ raw: AXUIElement) { self.raw = raw }
    public static var systemWide: RightKitAXElement { .init(AXUIElementCreateSystemWide()) }
    public static func application(pid: pid_t) -> RightKitAXElement { .init(AXUIElementCreateApplication(pid)) }

    public static func == (lhs: Self, rhs: Self) -> Bool { CFEqual(lhs.raw, rhs.raw) }
    public func hash(into hasher: inout Hasher) { hasher.combine(CFHash(raw)) }

    public var pid: pid_t? {
        var out: pid_t = 0
        return AXUIElementGetPid(raw, &out) == .success ? out : nil
    }

    /// Caps how long one call waits for the target app (seconds). 0 restores the system default.
    public func setMessagingTimeout(_ seconds: Float) {
        AXUIElementSetMessagingTimeout(raw, seconds)
    }

    // MARK: Reads

    public func value(_ attribute: String) throws(RightKitAXFailure) -> CFTypeRef {
        var out: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(raw, attribute as CFString, &out)
        guard error == .success else { throw RightKitAXFailure(code: error.rawValue) }
        guard let out else { throw .noValue }
        return out
    }

    public func string(_ attribute: String) -> String? { try? value(attribute) as? String }
    public func bool(_ attribute: String) -> Bool? { (try? value(attribute)) as? Bool }

    public func element(_ attribute: String) -> RightKitAXElement? {
        guard let v = try? value(attribute), CFGetTypeID(v) == AXUIElementGetTypeID() else { return nil }
        return RightKitAXElement(v as! AXUIElement)
    }

    public func elements(_ attribute: String) -> [RightKitAXElement] {
        guard let v = try? value(attribute), let list = v as? [AXUIElement] else { return [] }
        return list.map(RightKitAXElement.init)
    }

    public var role: String? { string(kAXRoleAttribute) }
    public var subrole: String? { string(kAXSubroleAttribute) }
    public var title: String? { string(kAXTitleAttribute) }
    public var windows: [RightKitAXElement] { elements(kAXWindowsAttribute) }
    public var focusedWindow: RightKitAXElement? { element(kAXFocusedWindowAttribute) }
    public var isMinimized: Bool? { bool(kAXMinimizedAttribute) }
    public var isFullScreen: Bool? { bool("AXFullScreen") }

    public var position: CGPoint? { cgValue(kAXPositionAttribute, .cgPoint, CGPoint.zero) }
    public var size: CGSize? { cgValue(kAXSizeAttribute, .cgSize, CGSize.zero) }

    /// Window frame in the global top-left-origin screen space Accessibility uses.
    public var frame: CGRect? {
        guard let position, let size else { return nil }
        return CGRect(origin: position, size: size)
    }

    private func cgValue<T>(_ attribute: String, _ type: AXValueType, _ zero: T) -> T? {
        guard let v = try? value(attribute), CFGetTypeID(v) == AXValueGetTypeID() else { return nil }
        var out = zero
        return AXValueGetValue(v as! AXValue, type, &out) ? out : nil
    }

    public func actions() -> [String] {
        var out: CFArray?
        guard AXUIElementCopyActionNames(raw, &out) == .success else { return [] }
        return out as? [String] ?? []
    }

    // MARK: Writes

    public func set(_ attribute: String, to value: CFTypeRef) throws(RightKitAXFailure) {
        let error = AXUIElementSetAttributeValue(raw, attribute as CFString, value)
        guard error == .success else { throw RightKitAXFailure(code: error.rawValue) }
    }

    public func perform(_ action: String) throws(RightKitAXFailure) {
        let error = AXUIElementPerformAction(raw, action as CFString)
        guard error == .success else { throw RightKitAXFailure(code: error.rawValue) }
    }

    public func setPosition(_ point: CGPoint) throws(RightKitAXFailure) {
        var p = point
        guard let v = AXValueCreate(.cgPoint, &p) else { throw .illegalArgument }
        try set(kAXPositionAttribute, to: v)
    }

    public func setSize(_ size: CGSize) throws(RightKitAXFailure) {
        var s = size
        guard let v = AXValueCreate(.cgSize, &s) else { throw .illegalArgument }
        try set(kAXSizeAttribute, to: v)
    }

    /// Moves then resizes (size, position, size again so a window that clamps against a screen
    /// edge still lands on the requested frame).
    public func setFrame(_ frame: CGRect) throws(RightKitAXFailure) {
        try setSize(frame.size)
        try setPosition(frame.origin)
        try setSize(frame.size)
    }
}

private func cfBool(_ value: Bool) -> CFBoolean { (value ? kCFBooleanTrue : kCFBooleanFalse)! }

/// Window-level actions over `RightKitAXElement`. All are best effort: an app may refuse any of them.
public enum RightKitAXWindowAction {
    public static func raise(_ window: RightKitAXElement) throws(RightKitAXFailure) {
        try window.perform(kAXRaiseAction)
    }

    public static func setMinimized(_ window: RightKitAXElement, _ minimized: Bool) throws(RightKitAXFailure) {
        try window.set(kAXMinimizedAttribute, to: cfBool(minimized))
    }

    public static func setFullScreen(_ window: RightKitAXElement, _ fullScreen: Bool) throws(RightKitAXFailure) {
        try window.set("AXFullScreen", to: cfBool(fullScreen))
    }

    /// Presses the window's close button.
    public static func close(_ window: RightKitAXElement) throws(RightKitAXFailure) {
        try press(window, kAXCloseButtonAttribute)
    }

    /// Presses the green zoom button (the "maximise" action; with the Option key not held this may
    /// enter full screen on current macOS, which is the app's behaviour to account for).
    public static func zoom(_ window: RightKitAXElement) throws(RightKitAXFailure) {
        try press(window, kAXZoomButtonAttribute)
    }

    /// Presses the minimise button.
    public static func minimize(_ window: RightKitAXElement) throws(RightKitAXFailure) {
        try press(window, kAXMinimizeButtonAttribute)
    }

    private static func press(_ window: RightKitAXElement, _ buttonAttribute: String) throws(RightKitAXFailure) {
        guard let button = window.element(buttonAttribute) else { throw .noValue }
        try button.perform(kAXPressAction)
    }
}

/// One Accessibility notification, delivered on the main actor.
public struct RightKitAXEvent: @unchecked Sendable {
    public let notification: String
    public let element: RightKitAXElement
}

/// Notifications the observer subscribes to on the application element. Raw values are the
/// `kAX*Notification` strings.
public enum RightKitAXNotifications {
    public static let windowCreated = kAXWindowCreatedNotification
    public static let focusedWindowChanged = kAXFocusedWindowChangedNotification
    public static let windowMoved = kAXWindowMovedNotification
    public static let windowResized = kAXWindowResizedNotification
    public static let windowMiniaturized = kAXWindowMiniaturizedNotification
    public static let windowDeminiaturized = kAXWindowDeminiaturizedNotification
    public static let elementDestroyed = kAXUIElementDestroyedNotification
    public static let applicationActivated = kAXApplicationActivatedNotification
    public static let applicationDeactivated = kAXApplicationDeactivatedNotification
    public static let titleChanged = kAXTitleChangedNotification

    /// What `RightKitAXObserver.init(pid:)` subscribes to when no list is given.
    public static let windowLifetime: [String] = [
        windowCreated, focusedWindowChanged, windowMoved, windowResized,
        windowMiniaturized, windowDeminiaturized, elementDestroyed,
    ]
}

/// `AXObserver` for one process. Subscribes on the application element and, for window lifetime,
/// on every window (a window's `AXUIElementDestroyed` is only sent to an observer registered on
/// that window). Events arrive on the main run loop as an `AsyncStream`.
///
/// Requires Accessibility permission. Registration failures for individual notifications are
/// tolerated (an app may not support one); `start()` throws only if the observer cannot be created.
@MainActor
public final class RightKitAXObserver {
    public let pid: pid_t
    public let events: AsyncStream<RightKitAXEvent>

    private let continuation: AsyncStream<RightKitAXEvent>.Continuation
    private let notifications: [String]
    private var observer: AXObserver?
    private var observedWindows: [RightKitAXElement] = []

    public init(pid: pid_t, notifications: [String] = RightKitAXNotifications.windowLifetime) {
        self.pid = pid
        self.notifications = notifications
        let pair = AsyncStream.makeStream(of: RightKitAXEvent.self, bufferingPolicy: .unbounded)
        events = pair.stream
        continuation = pair.continuation
    }

    /// Creates the observer, attaches it to the main run loop, and subscribes. Idempotent.
    public func start() throws(RightKitAXFailure) {
        guard observer == nil else { return }
        var created: AXObserver?
        let error = AXObserverCreate(pid, { _, element, notification, refcon in
            guard let refcon else { return }
            let owner = Unmanaged<RightKitAXObserver>.fromOpaque(refcon)
            let event = RightKitAXEvent(notification: notification as String, element: RightKitAXElement(element))
            MainActor.assumeIsolated { owner.takeUnretainedValue().receive(event) }
        }, &created)
        guard error == .success, let created else { throw RightKitAXFailure(code: error.rawValue) }
        observer = created
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(created), .commonModes)

        let app = RightKitAXElement.application(pid: pid)
        for name in notifications { subscribe(app, name) }
        for window in app.windows { observe(window: window) }
    }

    /// Unsubscribes and ends `events`. Safe to call twice.
    public func stop() {
        if let observer {
            for window in observedWindows { AXObserverRemoveNotification(observer, window.raw, kAXUIElementDestroyedNotification as CFString) }
            CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
        }
        observer = nil
        observedWindows.removeAll()
        continuation.finish()
    }

    public var observedWindowCount: Int { observedWindows.count }

    private func receive(_ event: RightKitAXEvent) {
        if event.notification == RightKitAXNotifications.windowCreated {
            observe(window: event.element)
        } else if event.notification == RightKitAXNotifications.elementDestroyed {
            observedWindows.removeAll { $0 == event.element }
        }
        continuation.yield(event)
    }

    private func observe(window: RightKitAXElement) {
        guard !observedWindows.contains(window) else { return }
        if subscribe(window, kAXUIElementDestroyedNotification) { observedWindows.append(window) }
    }

    @discardableResult
    private func subscribe(_ element: RightKitAXElement, _ name: String) -> Bool {
        guard let observer else { return false }
        let error = AXObserverAddNotification(
            observer, element.raw, name as CFString, Unmanaged.passUnretained(self).toOpaque()
        )
        return error == .success || error == .notificationAlreadyRegistered
    }
}
#endif
