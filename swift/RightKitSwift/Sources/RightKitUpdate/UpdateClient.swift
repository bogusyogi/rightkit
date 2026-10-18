import Foundation
import RightKitHTTP

public enum UpdateStatus: Sendable, Equatable {
    case idle
    case checking
    case upToDate
    case available(version: String, tier: UpdateTier?, eligible: Bool)
    case downloading(version: String, fraction: Double?)
    /// Downloaded and verified; nothing is installed yet.
    case staged(version: String)
    /// Swapped in place; relaunch to finish.
    case installed(version: String)
    case error(String)
}

/// Opaque handle for one verified, staged update. ``UpdateClient/installStaged(_:)`` accepts only the
/// handle for the update that is still current; a newer check invalidates it.
public struct StagedUpdateHandle: Sendable, Equatable {
    public let version: String
    let token: UUID
}

/// A verified copy of the new app, ready to install.
public struct StagedBundle: Sendable, Equatable {
    public var appURL: URL
    public var version: String
    public var workDirectory: URL

    public init(appURL: URL, version: String, workDirectory: URL) {
        self.appURL = appURL
        self.version = version
        self.workDirectory = workDirectory
    }
}

/// Platform install step. macOS provides ``MacAppInstaller``; iOS has none (check only).
public protocol UpdateInstalling: Sendable {
    /// Extracts the downloaded artifact into `workDirectory` and verifies the app inside it.
    /// Throws ``UpdateError/verificationFailed(_:)`` unless every check passes.
    func stage(artifact: URL, release: UpdateRelease, currentVersion: String, workDirectory: URL) async throws -> StagedBundle
    /// Replaces the installed app in place. Keeps no backup copy (decision D1).
    func install(_ staged: StagedBundle) async throws
    /// Starts a detached helper that opens the installed app once this process exits, and cleans up.
    func relaunch(_ staged: StagedBundle) throws
}

/// Check, download, verify, install. One client per app.
///
/// Policy hooks are the app's: `isEligible` (default: feature `update` releases need Pro, so
/// `tier != .update`), `canInstall` (host idle). With no `installer` the client only checks.
public actor UpdateClient {
    public typealias Eligibility = @Sendable (UpdateRelease) async -> Bool

    private let currentVersion: String
    private let feed: any UpdateFeed
    private let http: HTTPClient
    private let installer: (any UpdateInstalling)?
    private let gate: CheckGate<UpdateOutcome>
    private let isEligible: Eligibility
    private let canInstall: @Sendable () async -> Bool
    private let onStatus: @Sendable (UpdateStatus) -> Void
    private let workRoot: URL
    private let allowShortVersions: Bool

    private var etag: String?
    private var lastRelease: UpdateRelease?
    private var generation = 0
    private var available: (release: UpdateRelease, generation: Int)?
    private var staged: (handle: StagedUpdateHandle, bundle: StagedBundle, generation: Int)?

    public private(set) var status: UpdateStatus = .idle

    struct UpdateOutcome: Sendable { var release: UpdateRelease?; var unchanged: Bool; var etag: String? }

    public init(
        currentVersion: String, feed: any UpdateFeed, installer: (any UpdateInstalling)? = nil,
        http: HTTPClient = HTTPClient(), minCheckInterval: TimeInterval = 300, allowShortVersions: Bool = false,
        workRoot: URL = FileManager.default.temporaryDirectory,
        isEligible: @escaping Eligibility = { $0.tier != .update },
        canInstall: @escaping @Sendable () async -> Bool = { true },
        onStatus: @escaping @Sendable (UpdateStatus) -> Void = { _ in },
    ) {
        self.currentVersion = currentVersion
        self.feed = feed
        self.installer = installer
        self.http = http
        self.gate = CheckGate(minInterval: minCheckInterval)
        self.isEligible = isEligible
        self.canInstall = canInstall
        self.onStatus = onStatus
        self.workRoot = workRoot
        self.allowShortVersions = allowShortVersions
    }

    private func set(_ new: UpdateStatus) {
        status = new
        onStatus(new)
    }

    /// One check. Calls inside `minCheckInterval` share the previous answer; `force` always asks the feed.
    @discardableResult
    public func checkOnce(force: Bool = false) async -> UpdateStatus {
        generation += 1
        let myGeneration = generation
        set(.checking)
        do {
            let feed = feed
            let sentEtag = etag
            let outcome = try await gate.run(force: force) {
                switch try await feed.latest(etag: sentEtag) {
                case .unchanged: return UpdateOutcome(release: nil, unchanged: true, etag: sentEtag)
                case let .fresh(release, etag): return UpdateOutcome(release: release, unchanged: false, etag: etag)
                }
            }
            var release = outcome.release
            if outcome.unchanged { release = lastRelease } else { lastRelease = outcome.release; etag = outcome.etag }
            guard let release else {
                available = nil
                set(.upToDate)
                return status
            }
            guard let candidate = SemanticVersion(release.version, allowShort: allowShortVersions),
                  let current = SemanticVersion(currentVersion, allowShort: true)
            else { throw UpdateError.invalidFeed("version is not a semantic version") }
            guard candidate > current else {
                available = nil
                set(.upToDate)
                return status
            }
            let eligible = await isEligible(release)
            available = eligible ? (release, myGeneration) : nil
            staged = nil
            set(.available(version: release.version, tier: release.tier, eligible: eligible))
        } catch let error as UpdateError {
            set(.error(Self.message(error)))
        } catch {
            set(.error("update check failed"))
        }
        return status
    }

    /// Downloads the available update and verifies it. Nothing is installed.
    public func downloadAvailable() async throws -> StagedUpdateHandle {
        guard let installer else { throw UpdateError.unsupported("this platform installs updates through the App Store") }
        guard let (release, forGeneration) = available else { throw UpdateError.nothingAvailable }
        guard await isEligible(release) else { throw UpdateError.notEligible }
        guard let asset = release.asset else { throw UpdateError.missingAsset }
        guard asset.url.scheme?.lowercased() == "https" else { throw UpdateError.insecureURL }
        let work = workRoot.appendingPathComponent("rightkit-update-\(UUID().uuidString)", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
            set(.downloading(version: release.version, fraction: nil))
            let name = Self.safeFileName(asset.name)
            let onStatus = onStatus
            let version = release.version
            let download = try await http.download(
                HTTPRequest(url: asset.url, headers: ["Accept": "application/octet-stream"], timeout: 900),
                to: work.appendingPathComponent(name),
                expectedSize: asset.size > 0 ? asset.size : nil, expectedSHA256: asset.sha256,
                progress: { onStatus(.downloading(version: version, fraction: $0)) },
            )
            let bundle = try await installer.stage(
                artifact: download.fileURL, release: release, currentVersion: currentVersion, workDirectory: work,
            )
            guard generation == forGeneration else { throw UpdateError.staleHandle }
            let handle = StagedUpdateHandle(version: release.version, token: UUID())
            staged = (handle, bundle, forGeneration)
            set(.staged(version: release.version))
            return handle
        } catch {
            try? FileManager.default.removeItem(at: work)
            let mapped = Self.map(error)
            set(.error(Self.message(mapped)))
            throw mapped
        }
    }

    /// Installs the update `handle` names, if it is still the current one and the host says it is safe.
    public func installStaged(_ handle: StagedUpdateHandle) async throws {
        guard let installer else { throw UpdateError.unsupported("this platform installs updates through the App Store") }
        guard let current = staged, current.handle == handle, current.generation == generation else { throw UpdateError.staleHandle }
        guard await canInstall() else { throw UpdateError.deferred }
        do {
            try await installer.install(current.bundle)
        } catch {
            let mapped = Self.map(error)
            set(.error(Self.message(mapped)))
            throw mapped
        }
        set(.installed(version: current.handle.version))
    }

    /// Opens the installed app after this process exits. Call after ``installStaged(_:)``, then quit.
    public func relaunch(_ handle: StagedUpdateHandle) throws {
        guard let installer else { throw UpdateError.unsupported("this platform installs updates through the App Store") }
        guard let current = staged, current.handle == handle else { throw UpdateError.staleHandle }
        try installer.relaunch(current.bundle)
    }

    /// The release the last check found, for showing notes and the store page.
    public var availableRelease: UpdateRelease? { available?.release }

    static func safeFileName(_ name: String) -> String {
        let last = (name as NSString).lastPathComponent
        let cleaned = last.replacingOccurrences(of: "..", with: "_")
        return cleaned.isEmpty ? "update.bin" : cleaned
    }

    static func map(_ error: Error) -> UpdateError {
        if let error = error as? UpdateError { return error }
        if let error = error as? HTTPError {
            if case .integrity(let text) = error { return .verificationFailed(text) }
            if case .insecureURL = error { return .insecureURL }
            return .network(String(describing: error))
        }
        return .installFailed(String(describing: type(of: error)))
    }

    static func message(_ error: UpdateError) -> String {
        switch error {
        case .rateLimited: return "The update server is limiting checks. Try again later."
        case let .network(text): return "Couldn't reach the update server (\(text))."
        case let .invalidFeed(text): return "The update information was not usable (\(text))."
        case .missingAsset: return "This release has nothing to install."
        case .insecureURL: return "The update's download address is not HTTPS, so it was refused."
        case .notNewer: return "The update is not newer than the installed copy, so it was not installed."
        case let .verificationFailed(text): return "The update failed verification (\(text)), so it was not installed."
        case let .installFailed(text): return "The update could not be installed (\(text))."
        case let .toolFailed(tool, detail): return "\(tool) failed: \(detail)"
        case let .unsupported(text): return text
        case .notEligible: return "This update needs a Pro license."
        case .staleHandle: return "A newer check replaced this update; check again."
        case .deferred: return "The app is busy; the update will wait."
        case .nothingAvailable: return "No update is available."
        }
    }
}
