import Foundation
import RightKitHTTP

/// Release class carried by a RightKit update manifest: a `patch` is free for everyone, an `update`
/// is a feature release that may need Pro (`updateTierFromManifest`, `UPDATE_HOST_CONTRACT_VECTORS`).
public enum UpdateTier: String, Sendable, Equatable, Codable {
    case patch, update

    /// The shared host contract: patches are always eligible; feature updates need Pro.
    public func isEligible(pro: Bool) -> Bool { self == .patch || pro }
}

public struct UpdateAsset: Sendable, Equatable {
    public var name: String
    public var url: URL
    /// Advertised size in bytes; 0 when the feed does not say. A non-zero size is enforced.
    public var size: Int64
    /// Lower-case hex SHA-256 the download must match, when the feed publishes one.
    public var sha256: String?

    public init(name: String, url: URL, size: Int64 = 0, sha256: String? = nil) {
        self.name = name
        self.url = url
        self.size = size
        self.sha256 = sha256
    }
}

/// The newest published release, as much of it as the updater needs.
public struct UpdateRelease: Sendable, Equatable {
    public var version: String
    public var notes: String
    public var pageURL: URL?
    public var tier: UpdateTier?
    /// The installable artifact, or nil when the release has none (App Store listings).
    public var asset: UpdateAsset?

    public init(version: String, notes: String = "", pageURL: URL? = nil, tier: UpdateTier? = nil, asset: UpdateAsset? = nil) {
        self.version = version
        self.notes = notes
        self.pageURL = pageURL
        self.tier = tier
        self.asset = asset
    }
}

public enum UpdateError: Error, Equatable, Sendable {
    /// The feed host limits requests (HTTP 403/429).
    case rateLimited
    case network(String)
    /// The feed answered something that is not a usable release.
    case invalidFeed(String)
    /// The release has no installable artifact.
    case missingAsset
    /// The artifact or a redirect is not HTTPS.
    case insecureURL
    case notNewer
    /// The artifact failed identity, version, signature or Gatekeeper verification. Never installed.
    case verificationFailed(String)
    case installFailed(String)
    case toolFailed(tool: String, detail: String)
    /// This platform or configuration cannot install; check-only.
    case unsupported(String)
    /// The release is not eligible for this user (for example a Pro feature update).
    case notEligible
    /// The staged-update handle is not the current one.
    case staleHandle
    /// The host reported it is not safe to install now.
    case deferred
    case nothingAvailable
}

public enum FeedResult: Sendable, Equatable {
    /// A release (nil: the app is not listed), with the validator to send next time.
    case fresh(UpdateRelease?, etag: String?)
    /// Nothing changed since the validator that was sent.
    case unchanged
}

/// Where the newest release comes from.
public protocol UpdateFeed: Sendable {
    func latest(etag: String?) async throws -> FeedResult
}

func mapFeedError(_ error: HTTPError) -> UpdateError {
    switch error {
    case let .status(code, _) where code == 403 || code == 429: return .rateLimited
    case let .status(code, _): return .network("HTTP \(code)")
    case .insecureURL, .blockedDestination: return .insecureURL
    case let .transport(text): return .network(text)
    case .timeout: return .network("timeout")
    case .cancelled: return .network("cancelled")
    case let .decoding(text), let .encoding(text), let .integrity(text): return .invalidFeed(text)
    case .tooLarge: return .invalidFeed("feed is too large")
    }
}

// MARK: GitHub Releases

/// `GET /repos/{owner}/{repo}/releases/latest`, anonymous, with an ETag so an unchanged release is a
/// 304 with no body. Only HTTPS asset URLs are accepted. An asset's `digest` (`sha256:<hex>`), when
/// GitHub provides it, becomes the download's required SHA-256.
public struct GitHubReleaseFeed: UpdateFeed {
    public let owner: String
    public let repo: String
    public let http: HTTPClient
    /// Asset names in priority order; then any asset with `assetPrefix` and `assetSuffix`.
    public let assetNames: [String]
    public let assetPrefix: String?
    public let assetSuffix: String?
    public let apiBase: URL

    public init(
        owner: String, repo: String, assetNames: [String] = [], assetPrefix: String? = nil, assetSuffix: String? = nil,
        http: HTTPClient = HTTPClient(), apiBase: URL = URL(string: "https://api.github.com")!,
    ) {
        self.owner = owner
        self.repo = repo
        self.assetNames = assetNames
        self.assetPrefix = assetPrefix
        self.assetSuffix = assetSuffix
        self.http = http
        self.apiBase = apiBase
    }

    struct Release: Decodable {
        struct Asset: Decodable {
            var name: String
            var browserDownloadUrl: URL
            var size: Int64
            var digest: String?
        }
        var tagName: String
        var body: String?
        var htmlUrl: URL?
        var assets: [Asset]
    }

    public func latest(etag: String?) async throws -> FeedResult {
        var headers = ["Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"]
        if let etag { headers["If-None-Match"] = etag }
        let url = apiBase.appendingPathComponent("repos/\(owner)/\(repo)/releases/latest")
        let response: HTTPResponse
        do {
            response = try await http.send(HTTPRequest(url: url, headers: headers, timeout: 20, acceptedStatuses: [304]))
        } catch let error as HTTPError { throw mapFeedError(error) }
        if response.status == 304 { return .unchanged }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let release = try? decoder.decode(Release.self, from: response.data) else {
            throw UpdateError.invalidFeed("release data is not in the expected form")
        }
        return .fresh(makeRelease(release), etag: response.header("etag"))
    }

    func makeRelease(_ release: Release) -> UpdateRelease {
        let secure = release.assets.filter { $0.browserDownloadUrl.scheme?.lowercased() == "https" }
        var chosen: Release.Asset?
        for name in assetNames where chosen == nil { chosen = secure.first { $0.name == name } }
        if chosen == nil, assetPrefix != nil || assetSuffix != nil {
            chosen = secure.first { ($0.name.hasPrefix(assetPrefix ?? "")) && $0.name.hasSuffix(assetSuffix ?? "") }
        }
        let tag = release.tagName
        let version = (tag.hasPrefix("v") || tag.hasPrefix("V")) ? String(tag.dropFirst()) : tag
        return UpdateRelease(
            version: version, notes: release.body ?? "", pageURL: release.htmlUrl, tier: nil,
            asset: chosen.map { UpdateAsset(name: $0.name, url: $0.browserDownloadUrl, size: $0.size, sha256: Self.sha256(from: $0.digest)) },
        )
    }

    static func sha256(from digest: String?) -> String? {
        guard let digest, digest.lowercased().hasPrefix("sha256:") else { return nil }
        let hex = digest.dropFirst("sha256:".count).lowercased()
        return hex.count == 64 && hex.allSatisfy(\.isHexDigit) ? String(hex) : nil
    }
}

// MARK: Static manifest (Tauri v2 `latest.json` shape)

/// A static `latest.json`: `{version, notes, pub_date, tier, platforms: {"darwin-aarch64": {url, signature}}}`.
/// `tier` must be `patch` or `update` (fail closed otherwise), as `updateTierFromManifest` requires.
/// The manifest's minisign `signature` is not checked here: a Swift install is trusted only through
/// the Developer ID verification of the staged app bundle.
public struct StaticManifestFeed: UpdateFeed {
    public let url: URL
    public let platformKey: String
    public let requireTier: Bool
    public let http: HTTPClient

    public init(url: URL, platformKey: String, requireTier: Bool = true, http: HTTPClient = HTTPClient()) {
        self.url = url
        self.platformKey = platformKey
        self.requireTier = requireTier
        self.http = http
    }

    /// `darwin-aarch64` or `darwin-x86_64` for the running Mac.
    public static var currentDarwinKey: String {
        #if arch(arm64)
        return "darwin-aarch64"
        #else
        return "darwin-x86_64"
        #endif
    }

    public func latest(etag: String?) async throws -> FeedResult {
        var headers: [String: String] = [:]
        if let etag { headers["If-None-Match"] = etag }
        let response: HTTPResponse
        do {
            response = try await http.send(HTTPRequest(url: url, headers: headers, timeout: 20, acceptedStatuses: [304]))
        } catch let error as HTTPError { throw mapFeedError(error) }
        if response.status == 304 { return .unchanged }
        return .fresh(try Self.parse(response.data, platformKey: platformKey, requireTier: requireTier), etag: response.header("etag"))
    }

    static func parse(_ data: Data, platformKey: String, requireTier: Bool) throws -> UpdateRelease? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let version = object["version"] as? String, SemanticVersion(version) != nil
        else { throw UpdateError.invalidFeed("manifest has no valid version") }
        var tier: UpdateTier?
        if let raw = object["tier"] {
            guard let text = raw as? String, let parsed = UpdateTier(rawValue: text) else {
                throw UpdateError.invalidFeed("manifest tier must be \"patch\" or \"update\"")
            }
            tier = parsed
        } else if requireTier {
            throw UpdateError.invalidFeed("manifest tier must be \"patch\" or \"update\"")
        }
        guard let platforms = object["platforms"] as? [String: Any], let platform = platforms[platformKey] as? [String: Any] else {
            return nil
        }
        guard let text = platform["url"] as? String, let assetURL = URL(string: text), assetURL.scheme?.lowercased() == "https" else {
            throw UpdateError.invalidFeed("manifest asset URL is missing or not HTTPS")
        }
        return UpdateRelease(
            version: version, notes: (object["notes"] as? String) ?? "", pageURL: nil, tier: tier,
            asset: UpdateAsset(name: assetURL.lastPathComponent, url: assetURL, size: 0, sha256: nil),
        )
    }
}

// MARK: App Store (iOS)

/// Apple's public lookup endpoint. Sends only the bundle identifier. iOS apps cannot install
/// themselves: the result is a version and the store page to open.
public struct AppStoreLookupFeed: UpdateFeed {
    public let bundleID: String
    public let country: String?
    public let http: HTTPClient

    public init(bundleID: String, country: String? = nil, http: HTTPClient = HTTPClient()) {
        self.bundleID = bundleID
        self.country = country
        self.http = http
    }

    private struct Lookup: Decodable {
        struct Listing: Decodable {
            var version: String
            var trackViewUrl: URL?
            var releaseNotes: String?
        }
        var results: [Listing]
    }

    public func latest(etag: String?) async throws -> FeedResult {
        var query = [URLQueryItem(name: "bundleId", value: bundleID)]
        if let country { query.append(URLQueryItem(name: "country", value: country)) }
        let request = HTTPRequest(url: URL(string: "https://itunes.apple.com/lookup")!, query: query, timeout: 10)
        let response: HTTPResponse
        do { response = try await http.send(request) } catch let error as HTTPError { throw mapFeedError(error) }
        guard let lookup = try? JSONDecoder().decode(Lookup.self, from: response.data) else {
            throw UpdateError.invalidFeed("lookup answer is not in the expected form")
        }
        guard let listing = lookup.results.first else { return .fresh(nil, etag: nil) }
        return .fresh(UpdateRelease(version: listing.version, notes: listing.releaseNotes ?? "", pageURL: listing.trackViewUrl), etag: nil)
    }
}

// MARK: Release status and nightly discovery

/// `{schema: 1, appKey, platform, patchVersion, updateVersion}`: the metadata-only release status
/// `fetchReleaseStatus` reads. Unknown keys are refused.
public struct ReleaseStatus: Sendable, Equatable {
    public var appKey: String
    public var platform: String?
    public var patchVersion: String?
    public var updateVersion: String?

    public static func fetch(url: URL, http: HTTPClient = HTTPClient()) async throws -> ReleaseStatus {
        let response: HTTPResponse
        do { response = try await http.send(HTTPRequest(url: url, headers: ["Cache-Control": "no-store"], timeout: 15)) } catch let error as HTTPError {
            throw mapFeedError(error)
        }
        return try parse(response.data)
    }

    static func parse(_ data: Data) throws -> ReleaseStatus {
        let allowed: Set<String> = ["schema", "appKey", "platform", "patchVersion", "updateVersion"]
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              object.keys.allSatisfy({ allowed.contains($0) }),
              (object["schema"] as? NSNumber)?.intValue == 1,
              let appKey = object["appKey"] as? String, !appKey.isEmpty
        else { throw UpdateError.invalidFeed("release status response is invalid") }
        func optional(_ key: String) throws -> String? {
            guard let value = object[key], !(value is NSNull) else { return nil }
            guard let text = value as? String else { throw UpdateError.invalidFeed("release status response is invalid") }
            return text
        }
        return ReleaseStatus(
            appKey: appKey, platform: try optional("platform"), patchVersion: try optional("patchVersion"), updateVersion: try optional("updateVersion"),
        )
    }
}

/// Finds nightly manifests: the newest prerelease GitHub releases that carry a `latest.json`.
public enum NightlyDiscovery {
    private struct Entry: Decodable {
        struct Asset: Decodable { var name: String; var browserDownloadUrl: URL }
        var prerelease: Bool
        var draft: Bool
        var assets: [Asset]
    }

    /// The HTTPS `assetName` URLs of up to `limit` non-draft prereleases, in the API's order (newest first).
    public static func manifestURLs(releasesJSON: Data, limit: Int, assetName: String = "latest.json") throws -> [URL] {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let entries = try? decoder.decode([Entry].self, from: releasesJSON) else {
            throw UpdateError.invalidFeed("releases list is not in the expected form")
        }
        var urls: [URL] = []
        for entry in entries where entry.prerelease && !entry.draft {
            guard urls.count < limit else { break }
            if let asset = entry.assets.first(where: { $0.name == assetName && $0.browserDownloadUrl.scheme?.lowercased() == "https" }) {
                urls.append(asset.browserDownloadUrl)
            }
        }
        return urls
    }

    /// Fetches the release list and returns the nightly manifest URLs. Never throws: a failed discovery
    /// returns `[]` and calls `onWarning` once until a discovery succeeds again.
    public static func discover(
        owner: String, repo: String, limit: Int, http: HTTPClient = HTTPClient(), gate: WarnOnceGate,
        apiBase: URL = URL(string: "https://api.github.com")!, onWarning: @Sendable (String) -> Void,
    ) async -> [URL] {
        let url = apiBase.appendingPathComponent("repos/\(owner)/\(repo)/releases")
        let request = HTTPRequest(
            url: url, headers: ["Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"],
            query: [URLQueryItem(name: "per_page", value: String(max(limit * 3, 10)))], timeout: 20,
        )
        do {
            let response = try await http.send(request)
            let urls = try manifestURLs(releasesJSON: response.data, limit: limit)
            await gate.recordSuccess()
            return urls
        } catch {
            if await gate.shouldWarn() { onWarning("nightly discovery failed: \(error)") }
            return []
        }
    }
}
