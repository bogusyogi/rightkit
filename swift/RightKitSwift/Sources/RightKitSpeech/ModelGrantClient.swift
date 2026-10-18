#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// The one HTTP call the model-download owner makes besides the archive download itself. Internal on
/// purpose: RightKitHTTP owns general HTTP, and this seam is what the grant client and tests use until
/// the owner of that module replaces it.
protocol ModelHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

/// `URLSession` transport that never follows a redirect: a grant request carries a device id, and a
/// redirect would send it somewhere the app did not configure.
struct URLSessionModelTransport: ModelHTTPTransport {
    private final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
        func urlSession(
            _ session: URLSession, task: URLSessionTask,
            willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
            completionHandler: @escaping (URLRequest?) -> Void
        ) {
            completionHandler(nil)
        }
    }

    let session: URLSession

    init(session: URLSession = URLSession(configuration: .ephemeral)) {
        self.session = session
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await session.data(for: request, delegate: NoRedirects())
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return (data, http)
    }
}

/// A worker-issued, route-and-hash-scoped download token.
public struct ModelGrant: Sendable, Equatable {
    public let token: String
    /// Unix seconds.
    public let expiresAt: UInt64

    public init(token: String, expiresAt: UInt64) {
        self.token = token
        self.expiresAt = expiresAt
    }

    /// The `Authorization` header value.
    public var bearer: String { "Bearer \(token)" }

    /// True when the grant is expired or expires within `skew` seconds of `now`.
    public func expires(within skew: UInt64, now: Date = Date()) -> Bool {
        let seconds = UInt64(max(0, now.timeIntervalSince1970))
        return expiresAt <= seconds &+ skew
    }
}

/// Client for the model Worker's `/v1/model-grants` endpoint: the Swift counterpart of
/// `rightkit-models` `acquire_grant_pinned`.
///
/// The Worker answers a POST of the exact route, model, platform, SHA-256 and a stable device id with a
/// short-lived token bound to that tuple, which the download then presents as its bearer. The endpoint
/// must be `https`, carry no credentials, and is never followed through a redirect.
public struct ModelGrantClient: Sendable {
    public struct GrantRequest: Sendable, Equatable {
        public let workerBase: String
        public let route: String
        public let model: String
        public let platform: String
        public let sha256: String
        public let deviceID: String

        public init(workerBase: String, route: String, model: String, platform: String, sha256: String, deviceID: String) {
            self.workerBase = workerBase
            self.route = route
            self.model = model
            self.platform = platform
            self.sha256 = sha256
            self.deviceID = deviceID
        }
    }

    public enum GrantError: Error, Equatable, Sendable, LocalizedError {
        case badWorkerBase
        case transport(String)
        case httpStatus(Int)
        /// HTTP 429. The grant POST is never replayed; the caller decides when to try again.
        case rateLimited(retryAfter: TimeInterval?)
        case malformedResponse
        case scopeMismatch
        case grantExpired

        public var errorDescription: String? {
            switch self {
            case .badWorkerBase: return "The model service address is not configured correctly."
            case let .transport(reason): return "Could not reach the model service. \(reason)"
            case let .httpStatus(status): return "The model service refused the download request (\(status))."
            case .rateLimited: return "The model service is busy. Try again shortly."
            case .malformedResponse: return "The model service returned an unexpected response."
            case .scopeMismatch: return "The model service authorized a different model than requested."
            case .grantExpired: return "The model service returned an already-expired download grant."
            }
        }
    }

    private struct Body: Encodable {
        let route: String
        let model: String
        let platform: String
        let sha256: String
        let deviceId: String
    }

    private struct Response: Decodable {
        let token: String
        let expiresAt: UInt64
        let route: String
        let sha256: String
    }

    /// How far the device clock may run ahead of the Worker before a just-expired grant is still accepted
    /// locally. The Worker remains the expiry authority.
    static let clockSkewSeconds: UInt64 = 30

    private let transport: any ModelHTTPTransport
    private let userAgent: String
    private let now: @Sendable () -> Date

    /// - Parameters:
    ///   - session: the session to send through; the default is an ephemeral one. Redirects are refused.
    ///   - userAgent: sent as `User-Agent`.
    public init(session: URLSession? = nil, userAgent: String = "RightKit-ModelGrant/1") {
        self.init(
            transport: session.map { URLSessionModelTransport(session: $0) } ?? URLSessionModelTransport(),
            userAgent: userAgent, now: { Date() })
    }

    init(transport: any ModelHTTPTransport, userAgent: String = "RightKit-ModelGrant/1", now: @escaping @Sendable () -> Date) {
        self.transport = transport
        self.userAgent = userAgent
        self.now = now
    }

    /// The endpoint for `workerBase`, or nil when the base is not an `https` URL with a host and no credentials.
    public static func endpoint(workerBase: String) -> URL? {
        let trimmed = workerBase.trimmingCharacters(in: CharacterSet(charactersIn: "/").union(.whitespacesAndNewlines))
        guard let url = URL(string: "\(trimmed)/v1/model-grants"),
              url.scheme?.lowercased() == "https",
              let host = url.host, !host.isEmpty,
              url.user == nil, url.password == nil
        else { return nil }
        return url
    }

    /// Asks the Worker for a grant. The returned grant is scoped to the requested route and SHA-256.
    public func acquireGrant(_ request: GrantRequest) async throws -> ModelGrant {
        guard let endpoint = Self.endpoint(workerBase: request.workerBase) else { throw GrantError.badWorkerBase }
        var urlRequest = URLRequest(url: endpoint)
        urlRequest.httpMethod = "POST"
        urlRequest.timeoutInterval = 30
        urlRequest.setValue("application/json", forHTTPHeaderField: "content-type")
        urlRequest.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        urlRequest.httpBody = try JSONEncoder().encode(Body(
            route: request.route, model: request.model, platform: request.platform,
            sha256: request.sha256, deviceId: request.deviceID))

        let data: Data
        let http: HTTPURLResponse
        do {
            (data, http) = try await transport.send(urlRequest)
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            if (error as NSError).code == NSURLErrorCancelled, (error as NSError).domain == NSURLErrorDomain {
                throw CancellationError()
            }
            throw GrantError.transport(error.localizedDescription)
        }
        if http.statusCode == 429 {
            throw GrantError.rateLimited(retryAfter: Self.retryAfter(http.value(forHTTPHeaderField: "Retry-After"), now: now()))
        }
        guard (200..<300).contains(http.statusCode) else { throw GrantError.httpStatus(http.statusCode) }
        guard let grant = try? JSONDecoder().decode(Response.self, from: data),
              !grant.token.trimmingCharacters(in: .whitespaces).isEmpty, grant.expiresAt != 0
        else { throw GrantError.malformedResponse }
        // The token is only usable for the tuple that was asked for; the Worker echoes the scope so a
        // misrouted grant is caught before any download.
        guard grant.route == request.route, grant.sha256.caseInsensitiveCompare(request.sha256) == .orderedSame else {
            throw GrantError.scopeMismatch
        }
        let nowSeconds = UInt64(max(0, now().timeIntervalSince1970))
        let deadline = grant.expiresAt > UInt64.max - Self.clockSkewSeconds ? UInt64.max : grant.expiresAt + Self.clockSkewSeconds
        guard deadline > nowSeconds else { throw GrantError.grantExpired }
        return ModelGrant(token: grant.token, expiresAt: grant.expiresAt)
    }

    /// `Retry-After` as seconds: either delta-seconds or an HTTP date.
    static func retryAfter(_ header: String?, now: Date) -> TimeInterval? {
        guard let header = header?.trimmingCharacters(in: .whitespaces), !header.isEmpty else { return nil }
        if let seconds = TimeInterval(header), seconds >= 0 { return seconds }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "GMT")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        guard let date = formatter.date(from: header) else { return nil }
        return max(0, date.timeIntervalSince(now))
    }

    // MARK: Device id

    /// True for ids the Worker accepts: 16 to 128 characters of `[A-Za-z0-9._:-]`, starting alphanumeric.
    public static func isValidDeviceID(_ id: String) -> Bool {
        guard (16...128).contains(id.count), let first = id.unicodeScalars.first,
              CharacterSet.alphanumerics.contains(first), first.isASCII
        else { return false }
        return id.unicodeScalars.allSatisfy { scalar in
            scalar.isASCII && (CharacterSet.alphanumerics.contains(scalar) || ".:_-".unicodeScalars.contains(scalar))
        }
    }

    /// A stable, non-PII device id. Uses `vendorIdentifier` (for example
    /// `UIDevice.current.identifierForVendor`) when given; otherwise a random id persisted in `defaults`
    /// under `key`, so a missing vendor id still yields one stable, contract-valid value.
    public static func stableDeviceID(
        prefix: String = "ios-", vendorIdentifier: UUID? = nil, defaults: UserDefaults = .standard,
        key: String = "rightkit.model.deviceID"
    ) -> String {
        if let vendorIdentifier {
            let id = prefix + vendorIdentifier.uuidString.replacingOccurrences(of: "-", with: "")
            if isValidDeviceID(id) { return id }
        }
        if let existing = defaults.string(forKey: key), isValidDeviceID(existing) { return existing }
        let generated = prefix + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        defaults.set(generated, forKey: key)
        return generated
    }
}
#endif
