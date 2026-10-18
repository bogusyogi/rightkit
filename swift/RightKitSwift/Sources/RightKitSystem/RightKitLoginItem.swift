import Foundation

#if os(macOS)
import AppKit
import ServiceManagement

/// Registration state of a login item, independent of `ServiceManagement` types.
public enum RightKitLoginItemStatus: String, Sendable, Equatable {
    case enabled
    /// Registered, but the user must approve it in System Settings > Login Items.
    case requiresApproval
    case notRegistered
    /// The service's plist or app was not found (a copy that is not where it must be).
    case notFound

    init(_ status: SMAppService.Status) {
        switch status {
        case .enabled: self = .enabled
        case .requiresApproval: self = .requiresApproval
        case .notRegistered: self = .notRegistered
        case .notFound: self = .notFound
        @unknown default: self = .notFound
        }
    }
}

public enum RightKitLoginItemError: Error, Equatable, Sendable {
    /// The running copy is not at the one path allowed to own the login item.
    case notAtExpectedInstallPath(expected: String, actual: String)
    case registrationFailed(String)
}

/// What to do after a register or unregister attempt, given whether the call threw and the status
/// read afterwards. Pure.
public struct RightKitLoginItemOutcome: Equatable, Sendable {
    /// The call's failure should be reported. A throw is ignored when the status shows the item is
    /// enabled or only waiting for approval: the system's answer wins over the exception.
    public var reportsError: Bool
    /// Point the user at Login Items settings.
    public var opensSettings: Bool
    /// The item is enabled now (the value to show in a switch).
    public var isEnabled: Bool

    public static func afterRegister(threw: Bool, status: RightKitLoginItemStatus) -> RightKitLoginItemOutcome {
        RightKitLoginItemOutcome(
            reportsError: threw && status != .enabled && status != .requiresApproval,
            opensSettings: status == .requiresApproval,
            isEnabled: status == .enabled)
    }
}

/// Start-at-login through `SMAppService` (macOS 13+). Registering changes only this app's own login
/// item; it never touches another app's items or any privacy grant.
public struct RightKitLoginItem: Sendable {
    public enum Kind: Equatable, Sendable {
        case mainApp
        case agent(plistName: String)
        case daemon(plistName: String)
    }

    public let kind: Kind
    /// macOS keys login items, background items and privacy grants to path and identity, so a copy
    /// running from a build folder, a QA temp install or a mounted dmg must never register: that would
    /// re-point the one registration at a path that goes away. When set, `setEnabled` refuses unless
    /// the running bundle is exactly here.
    public let requiredInstallPath: String?

    public init(kind: Kind = .mainApp, requiredInstallPath: String? = nil) {
        self.kind = kind
        self.requiredInstallPath = requiredInstallPath
    }

    private var service: SMAppService {
        switch kind {
        case .mainApp: SMAppService.mainApp
        case .agent(let name): SMAppService.agent(plistName: name)
        case .daemon(let name): SMAppService.daemon(plistName: name)
        }
    }

    public var status: RightKitLoginItemStatus { RightKitLoginItemStatus(service.status) }
    public var isEnabled: Bool { status == .enabled }

    /// Whether `bundleURL` (symlinks resolved) is exactly `expectedPath`. Pure.
    public static func isInstalled(bundleURL: URL, at expectedPath: String) -> Bool {
        bundleURL.resolvingSymlinksInPath().path == URL(fileURLWithPath: expectedPath).resolvingSymlinksInPath().path
    }

    /// True when this process may own the login item: no required path, or running from it.
    public var isAtRequiredInstallPath: Bool {
        guard let requiredInstallPath else { return true }
        return Self.isInstalled(bundleURL: Bundle.main.bundleURL, at: requiredInstallPath)
    }

    /// Registers or unregisters, then reads the status back. Throws `notAtExpectedInstallPath` before
    /// touching anything when the install-path rule fails (unregistering is always allowed).
    /// A thrown system error is returned as `error` only when the status does not
    /// already show success or pending approval.
    @discardableResult
    public func setEnabled(_ enabled: Bool) throws -> (status: RightKitLoginItemStatus, outcome: RightKitLoginItemOutcome, error: String?) {
        if enabled, !isAtRequiredInstallPath {
            throw RightKitLoginItemError.notAtExpectedInstallPath(
                expected: requiredInstallPath ?? "", actual: Bundle.main.bundleURL.resolvingSymlinksInPath().path)
        }
        var failure: String?
        do {
            if enabled { try service.register() } else { try service.unregister() }
        } catch {
            failure = error.localizedDescription
        }
        let status = self.status
        let outcome = enabled
            ? RightKitLoginItemOutcome.afterRegister(threw: failure != nil, status: status)
            : RightKitLoginItemOutcome(reportsError: failure != nil && status == .enabled, opensSettings: false, isEnabled: status == .enabled)
        return (status, outcome, outcome.reportsError ? failure : nil)
    }

    /// Opens System Settings > Login Items.
    @MainActor
    public static func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
#endif
