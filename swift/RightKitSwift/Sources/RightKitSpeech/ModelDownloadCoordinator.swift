#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation
import RightKitArtifacts

/// Where a model download or install is, for a progress bar.
public struct ModelDownloadProgress: Sendable, Equatable {
    public enum Phase: Sendable, Equatable {
        case checking
        case requestingGrant
        case downloading
        /// A transient failure; the download restarts as `attempt` of ``ModelDownloadPolicy/maxAttempts``.
        case retrying(attempt: Int)
        /// The archive was adopted from staging or a retained copy; no download was needed.
        case usingStagedArchive
        case installing
        case validating
        case ready
    }

    public let phase: Phase
    /// 0...1 across the whole job, never decreasing within one attempt.
    public let fraction: Double

    public init(phase: Phase, fraction: Double) {
        self.phase = phase
        self.fraction = fraction
    }
}

/// Makes a model ready: reuse what is installed, else adopt a staged or retained archive, else
/// (grant, download with retries) and then install transactionally. The Swift counterpart of
/// `rightkit-models` `ensure_with_grant`.
///
/// ```
/// let coordinator = ModelDownloadCoordinator(layout: layout, downloader: downloader, userAgent: "MyApp/1.0",
///                                            deviceID: { ModelGrantClient.stableDeviceID() })
/// let directory = try await coordinator.prepare(spec, config: config) { progress in ... }
/// ```
///
/// Cancel by cancelling the task that runs ``prepare(_:config:progress:)``; the transfer is cancelled
/// and the retained archive (if any) is kept for the next attempt.
public struct ModelDownloadCoordinator: Sendable {
    typealias Fetch = @Sendable (URLRequest, String, @escaping @Sendable (Int64, Int64) -> Void) async throws -> URL
    typealias Sleep = @Sendable (TimeInterval) async throws -> Void

    public let layout: ModelStoreLayout
    public let staging: ModelDownloadStaging
    public let installer: ModelInstaller
    public let grantClient: ModelGrantClient
    public let userAgent: String

    private let fetch: Fetch
    private let sleep: Sleep
    private let deviceID: @Sendable () -> String
    private let allowsCellular: @Sendable () -> Bool

    /// - Parameters:
    ///   - downloader: the app's single ``ModelDownloader``; its staging folder is used for adoption.
    ///   - deviceID: the stable per-install id sent with grant requests (see ``ModelGrantClient/stableDeviceID(prefix:vendorIdentifier:defaults:key:)``).
    ///   - allowsCellular: read at each download attempt, so a settings change applies to the next one.
    public init(
        layout: ModelStoreLayout,
        downloader: ModelDownloader,
        installer: ModelInstaller = ModelInstaller(),
        grantClient: ModelGrantClient = ModelGrantClient(),
        userAgent: String,
        deviceID: @escaping @Sendable () -> String,
        allowsCellular: @escaping @Sendable () -> Bool = { false }
    ) {
        self.init(
            layout: layout, staging: downloader.configuration.staging, installer: installer, grantClient: grantClient,
            userAgent: userAgent, deviceID: deviceID, allowsCellular: allowsCellular,
            fetch: { request, modelID, progress in try await downloader.download(request, modelID: modelID, onProgress: progress) },
            sleep: { try await Task.sleep(nanoseconds: UInt64(max(0, $0) * 1_000_000_000)) })
    }

    init(
        layout: ModelStoreLayout, staging: ModelDownloadStaging, installer: ModelInstaller, grantClient: ModelGrantClient,
        userAgent: String, deviceID: @escaping @Sendable () -> String, allowsCellular: @escaping @Sendable () -> Bool,
        fetch: @escaping Fetch, sleep: @escaping Sleep
    ) {
        self.layout = layout
        self.staging = staging
        self.installer = installer
        self.grantClient = grantClient
        self.userAgent = userAgent
        self.deviceID = deviceID
        self.allowsCellular = allowsCellular
        self.fetch = fetch
        self.sleep = sleep
    }

    /// True when the model is installed and validated. Resolves an interrupted install first.
    public func isReady(_ spec: ModelArtifactSpec) async -> Bool {
        await installer.isReady(spec, layout: layout)
    }

    /// Makes `spec` ready and returns its model directory.
    ///
    /// `config.expectedSHA256` overrides the spec's digest when present (a build-time digest wins over a
    /// catalogue default). With no digest from either, nothing is downloaded or installed.
    @discardableResult
    public func prepare(
        _ spec: ModelArtifactSpec,
        config: ModelDownloadConfig,
        progress: @escaping @Sendable (ModelDownloadProgress) -> Void = { _ in }
    ) async throws -> URL {
        let spec = config.expectedSHA256.map(spec.withExpectedSHA256) ?? spec
        guard let digest = spec.expectedSHA256?.lowercased() else {
            throw ModelDownloadError.missingExpectedDigest(spec.directoryName)
        }
        progress(.init(phase: .checking, fraction: 0))
        if await installer.isReady(spec, layout: layout) {
            progress(.init(phase: .ready, fraction: 1))
            return layout.modelDirectory(for: spec)
        }
        try layout.prepare()
        try Task.checkCancellation()

        let archive = layout.archiveURL(for: spec)
        let manager = FileManager.default
        var haveArchive = false
        if manager.fileExists(atPath: archive.path) {
            // A retained archive is reusable only if it is still exactly the expected bytes.
            if (try? ArtifactInstaller.sha256(of: archive))?.lowercased() == digest { haveArchive = true } else { try? manager.removeItem(at: archive) }
        }
        if !haveArchive, try staging.adopt(model: spec.id, to: archive) { haveArchive = true }
        if haveArchive {
            progress(.init(phase: .usingStagedArchive, fraction: 0.9))
        } else {
            try await download(spec: spec, config: config, to: archive, progress: progress)
        }

        try Task.checkCancellation()
        progress(.init(phase: .installing, fraction: 0.9))
        do {
            let directory = try await installer.install(archive: archive, spec: spec, layout: layout, onValidating: {
                progress(.init(phase: .validating, fraction: 0.95))
            })
            // The expanded model is the source of truth; keeping the archive would double the storage.
            try? manager.removeItem(at: archive)
            progress(.init(phase: .ready, fraction: 1))
            return directory
        } catch {
            // A cancelled install keeps the archive for the retry; a rejected archive is useless.
            if !ModelDownloadPolicy.isCancellation(error) { try? manager.removeItem(at: archive) }
            throw error
        }
    }

    // MARK: Download

    private final class ProgressBox: @unchecked Sendable {
        private let lock = NSLock()
        private var tracker = ModelDownloadProgressTracker()
        private let report: @Sendable (ModelDownloadProgress) -> Void

        init(report: @escaping @Sendable (ModelDownloadProgress) -> Void) { self.report = report }

        func update(downloaded: Int64, expected: Int64) {
            let fraction = lock.withLock { tracker.update(downloadedBytes: downloaded, expectedBytes: expected) }
            // The transfer is 2%...90% of the job; the rest is install and validation.
            report(.init(phase: .downloading, fraction: 0.02 + fraction * 0.88))
        }

        func reset() { lock.withLock { tracker.reset() } }
    }

    private func download(
        spec: ModelArtifactSpec,
        config: ModelDownloadConfig,
        to archive: URL,
        progress: @escaping @Sendable (ModelDownloadProgress) -> Void
    ) async throws {
        guard let url = config.signedURL else { throw ModelDownloadError.notConfigured }
        let box = ProgressBox(report: progress)
        var grant: ModelGrant?
        var refreshedAfterRejection = false
        let maxAttempts = ModelDownloadPolicy.maxAttempts

        for attempt in 1...maxAttempts {
            try Task.checkCancellation()
            if let workerBase = config.workerBase, grant == nil || grant?.expires(within: 60) == true {
                progress(.init(phase: .requestingGrant, fraction: 0.01))
                grant = try await grantClient.acquireGrant(.init(
                    workerBase: workerBase, route: spec.route, model: spec.id, platform: spec.platform,
                    sha256: spec.expectedSHA256 ?? "", deviceID: deviceID()))
            }
            let request = ModelDownloadPolicy.request(
                url: url, bearerToken: grant?.token, userAgent: userAgent, allowsCellular: allowsCellular())
            progress(.init(phase: .downloading, fraction: 0.02))
            do {
                let file = try await fetch(request, spec.id, { downloaded, expected in box.update(downloaded: downloaded, expected: expected) })
                let manager = FileManager.default
                if manager.fileExists(atPath: archive.path) { try manager.removeItem(at: archive) }
                try manager.moveItem(at: file, to: archive)
                return
            } catch {
                if ModelDownloadPolicy.isCancellation(error) { throw error }
                // The Worker may have rejected a grant that expired in flight: ask once more.
                if case let ModelDownloadError.httpStatus(status) = error, status == 401 || status == 403,
                   config.workerBase != nil, !refreshedAfterRejection, attempt < maxAttempts {
                    refreshedAfterRejection = true
                    grant = nil
                    continue
                }
                guard ModelDownloadPolicy.shouldRetry(error, attempt: attempt, maxAttempts: maxAttempts) else { throw error }
                box.reset()
                progress(.init(phase: .retrying(attempt: attempt + 1), fraction: 0.02))
                try await sleep(ModelDownloadPolicy.backoff(afterAttempt: attempt))
            }
        }
        throw ModelDownloadError.retryExhausted
    }
}
#endif
