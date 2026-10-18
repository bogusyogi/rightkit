import CryptoKit
import Foundation

/// Async JSON-over-HTTP client. One value per app or feature; it is `Sendable` and stateless.
///
/// Policy, enforced before anything is sent: the URL is HTTPS (plain HTTP only to a loopback host),
/// carries no embedded credentials, and the response body stays under ``maxResponseBytes``.
/// A non-2xx status throws ``HTTPError/status(code:message:)`` unless the request lists it in
/// ``HTTPRequest/acceptedStatuses``.
public struct HTTPClient: Sendable {
    public let transport: any HTTPTransport
    public let allowInsecureLoopback: Bool
    /// When set, replaces the loopback-HTTP rule: the destination (and DNS answers) must satisfy it.
    /// Default nil keeps the original rule. Pass ``NetPolicy/publicOnly`` or an opt-in policy for
    /// user-configured endpoints.
    public let policy: NetPolicy?
    private let resolver: any HostResolving
    public let maxResponseBytes: Int
    public let userAgent: String?
    public let retry: HTTPRetryPolicy
    private let sleep: @Sendable (TimeInterval) async -> Void

    public init(
        transport: (any HTTPTransport)? = nil,
        userAgent: String? = nil,
        allowInsecureLoopback: Bool = true,
        policy: NetPolicy? = nil,
        resolver: any HostResolving = SystemHostResolver(),
        maxResponseBytes: Int = 8 * 1024 * 1024,
        retry: HTTPRetryPolicy = .none,
        sleep: @escaping @Sendable (TimeInterval) async -> Void = { seconds in
            try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
        },
    ) {
        // With a policy, the default transport enforces it on every redirect hop too, not only the first URL.
        self.transport = transport ?? URLSessionTransport(policy: policy, resolver: resolver)
        self.userAgent = userAgent
        self.allowInsecureLoopback = allowInsecureLoopback
        self.policy = policy
        self.resolver = resolver
        self.maxResponseBytes = maxResponseBytes
        self.retry = retry
        self.sleep = sleep
    }

    // MARK: URL policy

    /// HTTPS, or HTTP to `localhost` / `127.0.0.1` / `::1` when `allowInsecureLoopback`; never
    /// a URL with embedded user or password.
    public static func isPermitted(_ url: URL, allowInsecureLoopback: Bool) -> Bool {
        guard let scheme = url.scheme?.lowercased(), let host = url.host?.lowercased(), !host.isEmpty else { return false }
        if url.user != nil || url.password != nil { return false }
        if scheme == "https" { return true }
        guard scheme == "http", allowInsecureLoopback else { return false }
        return host == "localhost" || host == "127.0.0.1" || host == "::1"
    }

    private func validate(_ url: URL) async throws {
        if let policy {
            try await policy.vet(url, resolver: resolver)
            return
        }
        guard Self.isPermitted(url, allowInsecureLoopback: allowInsecureLoopback) else {
            throw HTTPError.insecureURL(url.host ?? "")
        }
    }

    // MARK: Send

    /// Sends one request and returns the 2xx (or explicitly accepted) response.
    public func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        try await validate(request.url)
        let urlRequest = try build(request)
        var attempt = 1
        while true {
            do {
                let response = try await exchange(urlRequest, for: request)
                if retry.maxAttempts > attempt, request.method.isIdempotent, Self.isTransient(status: response.status),
                   !request.acceptedStatuses.contains(response.status) {
                    await sleep(delay(attempt: attempt, retryAfter: response.header("retry-after")))
                    attempt += 1
                    continue
                }
                return try Self.checked(response, for: request)
            } catch let error as HTTPError {
                if retry.maxAttempts > attempt, request.method.isIdempotent, error.isRetryable {
                    await sleep(delay(attempt: attempt, retryAfter: nil))
                    attempt += 1
                    continue
                }
                throw error
            }
        }
    }

    /// Sends and decodes the body as `T`.
    public func json<T: Decodable>(
        _ request: HTTPRequest,
        as type: T.Type = T.self,
        decoder: JSONDecoder = JSONDecoder(),
    ) async throws -> T {
        try await send(request).decode(T.self, decoder: decoder)
    }

    private func exchange(_ urlRequest: URLRequest, for request: HTTPRequest) async throws -> HTTPResponse {
        do {
            let (data, http) = try await transport.send(urlRequest)
            guard data.count <= maxResponseBytes else { throw HTTPError.tooLarge(limit: maxResponseBytes) }
            return HTTPResponse(status: http.statusCode, headers: Self.headers(of: http), data: data)
        } catch let error as HTTPError {
            throw error
        } catch {
            throw Self.map(error)
        }
    }

    private static func checked(_ response: HTTPResponse, for request: HTTPRequest) throws -> HTTPResponse {
        if (200..<300).contains(response.status) || request.acceptedStatuses.contains(response.status) { return response }
        throw HTTPError.status(code: response.status, message: message(from: response.data, redacting: request.bearer))
    }

    private func build(_ request: HTTPRequest) throws -> URLRequest {
        var url = request.url
        if !request.query.isEmpty {
            guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
                throw HTTPError.insecureURL(url.host ?? "")
            }
            components.queryItems = (components.queryItems ?? []) + request.query
            guard let withQuery = components.url else { throw HTTPError.insecureURL(url.host ?? "") }
            url = withQuery
        }
        var urlRequest = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: request.timeout)
        urlRequest.httpMethod = request.method.rawValue
        urlRequest.httpBody = request.body
        urlRequest.setValue("application/json", forHTTPHeaderField: "Accept")
        if let userAgent { urlRequest.setValue(userAgent, forHTTPHeaderField: "User-Agent") }
        for (name, value) in request.headers { urlRequest.setValue(value, forHTTPHeaderField: name) }
        if let bearer = request.bearer, !bearer.isEmpty {
            urlRequest.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        return urlRequest
    }

    // MARK: Download

    /// Streams `request` to `destination` through a `.part` file and moves it into place only when
    /// the answer was `200` and the bytes match `expectedSize` and `expectedSHA256` (hex) when given.
    /// Redirects follow the transport's policy (HTTPS only). `progress` gets a 0...1 fraction, or nil
    /// while the total is unknown. A failed download leaves no file at `destination`.
    @discardableResult
    public func download(
        _ request: HTTPRequest,
        to destination: URL,
        expectedSize: Int64? = nil,
        expectedSHA256: String? = nil,
        progress: (@Sendable (Double?) -> Void)? = nil,
    ) async throws -> HTTPDownload {
        try await validate(request.url)
        let urlRequest = try build(request)
        let fileManager = FileManager.default
        let part = destination.appendingPathExtension("part")
        do {
            let streamed: (HTTPURLResponse, AsyncThrowingStream<Data, Error>)
            do { streamed = try await transport.stream(urlRequest) } catch { throw Self.map(error) }
            let head = streamed.0
            let chunks = streamed.1
            guard head.statusCode == 200 else { throw HTTPError.status(code: head.statusCode, message: "") }
            if let expectedSize, expectedSize > 0, head.expectedContentLength > 0, head.expectedContentLength != expectedSize {
                throw HTTPError.integrity("advertised size \(head.expectedContentLength) differs from expected \(expectedSize)")
            }
            try fileManager.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? fileManager.removeItem(at: part)
            guard fileManager.createFile(atPath: part.path, contents: nil) else {
                throw HTTPError.transport("cannot create download file")
            }
            let handle = try FileHandle(forWritingTo: part)
            var hasher = SHA256()
            var written: Int64 = 0
            let total: Int64? = expectedSize ?? (head.expectedContentLength > 0 ? head.expectedContentLength : nil)
            do {
                for try await chunk in chunks {
                    written += Int64(chunk.count)
                    if let expectedSize, expectedSize > 0, written > expectedSize {
                        throw HTTPError.integrity("download exceeds expected size \(expectedSize)")
                    }
                    try handle.write(contentsOf: chunk)
                    hasher.update(data: chunk)
                    progress?(total.map { min(1, Double(written) / Double($0)) })
                }
                try handle.close()
            } catch {
                try? handle.close()
                throw error
            }
            if let expectedSize, expectedSize > 0, written != expectedSize {
                throw HTTPError.integrity("download is \(written) bytes, expected \(expectedSize)")
            }
            let digest = hasher.finalize().map { String(format: "%02x", $0) }.joined()
            if let expectedSHA256, digest != expectedSHA256.lowercased() {
                throw HTTPError.integrity("SHA-256 mismatch")
            }
            try? fileManager.removeItem(at: destination)
            try fileManager.moveItem(at: part, to: destination)
            return HTTPDownload(fileURL: destination, byteCount: written, sha256: digest)
        } catch let error as HTTPError {
            try? fileManager.removeItem(at: part)
            throw error
        } catch {
            try? fileManager.removeItem(at: part)
            throw Self.map(error)
        }
    }

    // MARK: Helpers

    private func delay(attempt: Int, retryAfter: String?) -> TimeInterval {
        if let retryAfter, let seconds = TimeInterval(retryAfter.trimmingCharacters(in: .whitespaces)) {
            return min(retry.maxDelay, max(0, seconds))
        }
        return min(retry.maxDelay, retry.baseDelay * pow(2, Double(attempt - 1)))
    }

    private static func isTransient(status: Int) -> Bool { [429, 500, 502, 503, 504].contains(status) }

    static func headers(of response: HTTPURLResponse) -> [String: String] {
        var out: [String: String] = [:]
        for (key, value) in response.allHeaderFields {
            if let key = key as? String, let value = value as? String { out[key] = value }
        }
        return out
    }

    static func map(_ error: Error) -> HTTPError {
        if let error = error as? HTTPError { return error }
        if error is CancellationError { return .cancelled }
        if let urlError = error as? URLError {
            switch urlError.code {
            case .cancelled: return .cancelled
            case .timedOut: return .timeout
            default: return .transport("URLError \(urlError.code.rawValue)")
            }
        }
        return .transport(String(describing: Swift.type(of: error)))
    }

    /// The server's own error text from a JSON body (`error`, `error.message`, `message`,
    /// `error_description`, `detail`), else a short plain-text body; bounded to 512 characters, with
    /// `bearer` removed.
    public static func message(from data: Data, redacting bearer: String? = nil) -> String {
        var text = ""
        if let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let value = object["error"] as? String, !value.isEmpty { text = value }
            else if let nested = object["error"] as? [String: Any], let value = nested["message"] as? String { text = value }
            else if let value = object["message"] as? String { text = value }
            else if let value = object["error_description"] as? String { text = value }
            else if let value = object["detail"] as? String { text = value }
        } else if let plain = String(data: data.prefix(512), encoding: .utf8) {
            let trimmed = plain.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.contains("<") { text = trimmed }
        }
        if let bearer, !bearer.isEmpty { text = text.replacingOccurrences(of: bearer, with: "[redacted]") }
        return String(text.prefix(512))
    }
}

extension HTTPError {
    /// Transport trouble and a timeout may be repeated; a definite answer may not.
    fileprivate var isRetryable: Bool {
        switch self {
        case .transport, .timeout: return true
        default: return false
        }
    }
}

/// A finished download.
public struct HTTPDownload: Sendable, Equatable {
    public let fileURL: URL
    public let byteCount: Int64
    /// Lower-case hex SHA-256 of the bytes written.
    public let sha256: String
}
