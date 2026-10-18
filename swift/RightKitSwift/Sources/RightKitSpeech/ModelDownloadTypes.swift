#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// One downloadable Core ML model: where it comes from, what it must contain, and the directory it
/// becomes. The app owns the catalogue (ids, routes, digests); this owner owns the transaction.
public struct ModelArtifactSpec: Sendable, Equatable {
    /// Stable id for the model (for example `"parakeet-tdt"`). Names the staged download and the task.
    public let id: String
    public let backend: String
    public let platform: String
    /// Worker route the archive is served from, and the scope a grant is bound to.
    public let route: String
    /// The single top-level directory the archive holds, and the installed model directory's name.
    public let directoryName: String
    /// SHA-256 of the archive. Without one nothing is installed: an unverifiable archive is refused.
    public let expectedSHA256: String?
    /// Files that must exist in the installed directory, relative to it.
    public let requiredFiles: [String]
    /// Core ML stages (`<name>.mlmodelc` or `<name>.mlpackage`) that must exist and load.
    public let requiredStageNames: [String]
    /// Recorded in the install manifest.
    public let version: String?

    public static let parakeetRequiredFiles = ["pipeline.json", "vocab.txt"]
    public static let parakeetStageNames = ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"]

    public init(
        id: String,
        backend: String,
        platform: String,
        route: String,
        directoryName: String,
        expectedSHA256: String?,
        requiredFiles: [String] = ModelArtifactSpec.parakeetRequiredFiles,
        requiredStageNames: [String] = ModelArtifactSpec.parakeetStageNames,
        version: String? = nil
    ) {
        self.id = id
        self.backend = backend
        self.platform = platform
        self.route = route
        self.directoryName = directoryName
        self.expectedSHA256 = expectedSHA256
        self.requiredFiles = requiredFiles
        self.requiredStageNames = requiredStageNames
        self.version = version
    }

    /// The same spec with a runtime-resolved archive digest. Required files and stages are kept, so a
    /// download path cannot silently revert a non-Parakeet artifact to Parakeet defaults.
    public func withExpectedSHA256(_ sha256: String) -> ModelArtifactSpec {
        ModelArtifactSpec(
            id: id, backend: backend, platform: platform, route: route, directoryName: directoryName,
            expectedSHA256: sha256, requiredFiles: requiredFiles, requiredStageNames: requiredStageNames,
            version: version)
    }

    /// True when the directory holds every required file and stage. It does not load them; the ready
    /// marker (``CoreMLModelValidator/hasReadyMarker(at:)``) says a validation passed.
    public func isStructurallyInstalled(at directory: URL) -> Bool {
        let manager = FileManager.default
        guard manager.fileExists(atPath: directory.path) else { return false }
        guard requiredFiles.allSatisfy({ manager.fileExists(atPath: directory.appendingPathComponent($0).path) }) else {
            return false
        }
        return requiredStageNames.allSatisfy {
            manager.fileExists(atPath: directory.appendingPathComponent("\($0).mlmodelc").path)
                || manager.fileExists(atPath: directory.appendingPathComponent("\($0).mlpackage").path)
        }
    }
}

/// Where models live on disk: `<root>/models/<directoryName>` for installed models and
/// `<root>/downloads/<directoryName>.tar.gz` for an archive between download and install.
public struct ModelStoreLayout: Sendable, Equatable {
    public let root: URL

    public init(root: URL) {
        self.root = root
    }

    public var modelsRoot: URL { root.appendingPathComponent("models", isDirectory: true) }
    public var downloadsRoot: URL { root.appendingPathComponent("downloads", isDirectory: true) }

    public func modelDirectory(for spec: ModelArtifactSpec) -> URL {
        modelsRoot.appendingPathComponent(spec.directoryName, isDirectory: true)
    }

    public func archiveURL(for spec: ModelArtifactSpec) -> URL {
        downloadsRoot.appendingPathComponent("\(spec.directoryName).tar.gz")
    }

    /// Creates the folders and keeps models out of iCloud and device backups (a model is hundreds of
    /// megabytes and can always be downloaded again). Backup exclusion is best effort.
    public func prepare() throws {
        try FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: downloadsRoot, withIntermediateDirectories: true)
        Self.excludeFromBackup(root)
        Self.excludeFromBackup(modelsRoot)
        Self.excludeFromBackup(downloadsRoot)
    }

    static func excludeFromBackup(_ url: URL) {
        var target = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? target.setResourceValues(values)
    }
}

/// Where a model's archive comes from. Built from the app's build configuration.
public struct ModelDownloadConfig: Sendable, Equatable {
    /// The archive URL. Either the explicit one, or `<workerBase><route>`.
    public let signedURL: URL?
    public let expectedSHA256: String?
    /// Worker origin that issues a scoped download grant first. Nil for an explicit signed URL, which
    /// needs no grant. A route with no worker base and no explicit URL has no authorization path, and
    /// the download fails closed rather than sending an unauthenticated request.
    public let workerBase: String?

    public init(signedURL: URL?, expectedSHA256: String?, workerBase: String?) {
        self.signedURL = signedURL
        self.expectedSHA256 = expectedSHA256
        self.workerBase = workerBase
    }

    /// Resolves raw build values. Unexpanded `$(...)` placeholders and bare `http:`/`https:` count as unset.
    public static func fromValues(
        urlString: String?,
        shaString: String?,
        workerBaseString: String? = nil,
        route: String? = nil
    ) -> ModelDownloadConfig {
        let explicitURL = cleaned(urlString)
        let urlValue = explicitURL ?? routedURLString(workerBaseString: workerBaseString, route: route)
        return ModelDownloadConfig(
            signedURL: urlValue.flatMap(URL.init(string:)),
            expectedSHA256: cleaned(shaString),
            // A worker base applies only when the URL was routed through it.
            workerBase: explicitURL == nil ? cleaned(workerBaseString) : nil)
    }

    private static func routedURLString(workerBaseString: String?, route: String?) -> String? {
        guard let route, let base = cleaned(workerBaseString) else { return nil }
        return "\(base.trimmingCharacters(in: CharacterSet(charactersIn: "/")))\(route)"
    }

    private static func cleaned(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty,
              !value.hasPrefix("$("), value != "http:", value != "https:"
        else { return nil }
        return value
    }
}

public enum ModelDownloadError: Error, Equatable, Sendable, LocalizedError {
    case httpStatus(Int)
    case retryExhausted
    /// A download for the same model id already has a waiter.
    case alreadyInProgress
    /// No download URL could be resolved (no explicit URL and no worker base).
    case notConfigured
    /// The spec has no digest. Refused before any network or disk work.
    case missingExpectedDigest(String)
    /// The downloaded archive's digest does not match; the archive is deleted.
    case checksumMismatch(expected: String, actual: String)

    public var errorDescription: String? {
        switch self {
        case let .httpStatus(status):
            if status == 401 || status == 403 {
                return "Private model download is not configured for this build. Import a model archive before recording."
            }
            return "Model download failed with HTTP \(status)."
        case .retryExhausted: return "Model download failed after retries."
        case .alreadyInProgress: return "This model is already downloading."
        case .notConfigured: return "This build has no model download address."
        case let .missingExpectedDigest(name):
            return "No expected checksum is configured for \(name); refusing to install an unverifiable archive."
        case let .checksumMismatch(expected, actual): return "Model checksum mismatch. Expected \(expected), got \(actual)."
        }
    }
}

/// Retry and request rules for model downloads.
public enum ModelDownloadPolicy {
    public static let maxAttempts = 3

    public static func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        let value = error as NSError
        return value.domain == NSURLErrorDomain && value.code == NSURLErrorCancelled
    }

    /// Transient failures only: timeouts, lost connections, 408, 429 and 5xx. Never a cancellation, a 4xx
    /// other than 408/429, or the last attempt.
    public static func shouldRetry(_ error: Error, attempt: Int, maxAttempts: Int = ModelDownloadPolicy.maxAttempts) -> Bool {
        guard attempt < maxAttempts, !isCancellation(error) else { return false }
        if case let ModelDownloadError.httpStatus(status) = error {
            return status == 408 || status == 429 || (500..<600).contains(status)
        }
        let value = error as NSError
        guard value.domain == NSURLErrorDomain else { return false }
        switch value.code {
        case NSURLErrorTimedOut, NSURLErrorCannotFindHost, NSURLErrorCannotConnectToHost,
             NSURLErrorNetworkConnectionLost, NSURLErrorDNSLookupFailed, NSURLErrorNotConnectedToInternet:
            return true
        default:
            return false
        }
    }

    /// Back-off before attempt `attempt + 1`: one second per attempt made.
    public static func backoff(afterAttempt attempt: Int) -> TimeInterval { TimeInterval(attempt) }

    /// The download request: bearer token when there is one, a 60 second timeout, and the caller's
    /// network rules (`allowsCellular` also governs expensive paths; constrained paths are never used).
    public static func request(url: URL, bearerToken: String?, userAgent: String, allowsCellular: Bool) -> URLRequest {
        var request = URLRequest(url: url)
        request.timeoutInterval = 60
        request.allowsCellularAccess = allowsCellular
        request.allowsExpensiveNetworkAccess = allowsCellular
        request.allowsConstrainedNetworkAccess = false
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        if let bearerToken { request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization") }
        return request
    }
}

/// Download progress that never moves backwards and survives a transfer-size-unknown report.
///
/// A background or ranged response can report `NSURLSessionTransferSizeUnknown` (-1) between chunks;
/// dividing by it collapses progress to 0 and the bar bounces. The tracker remembers the last known
/// size and the high-water fraction. A real retry calls ``reset()``.
public struct ModelDownloadProgressTracker: Sendable, Equatable {
    private var lastKnownExpectedBytes: Int64 = 0
    private var highWater = 0.0

    public init() {}

    public mutating func update(downloadedBytes: Int64, expectedBytes: Int64) -> Double {
        if expectedBytes > 0 { lastKnownExpectedBytes = expectedBytes }
        guard lastKnownExpectedBytes > 0 else { return highWater }
        let fraction = min(max(Double(downloadedBytes) / Double(lastKnownExpectedBytes), 0), 1)
        highWater = max(highWater, fraction)
        return highWater
    }

    public mutating func reset() {
        lastKnownExpectedBytes = 0
        highWater = 0
    }
}
#endif
