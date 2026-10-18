import Foundation

// Async JSON request API shared by the Right Suite's native apps: the Swift counterpart of the
// Rust owner `rightkit-http` (HTTPS-only endpoints with a loopback exception for local services,
// bounded response bodies, status mapping, redacted errors). Foundation only.

/// HTTP verbs the client sends.
public enum HTTPMethod: String, Sendable, Equatable {
    case get = "GET"
    case post = "POST"
    case put = "PUT"
    case patch = "PATCH"
    case delete = "DELETE"
    case head = "HEAD"

    /// Verbs a retry may repeat without changing server state.
    var isIdempotent: Bool {
        switch self {
        case .get, .head, .put, .delete: return true
        case .post, .patch: return false
        }
    }
}

/// One request. Build with the initializer or the `json` / `form` helpers.
public struct HTTPRequest: Sendable {
    public var method: HTTPMethod
    public var url: URL
    public var headers: [String: String]
    public var query: [URLQueryItem]
    public var body: Data?
    /// Sent as `Authorization: Bearer <token>`; never copied into an error message.
    public var bearer: String?
    public var timeout: TimeInterval
    /// Non-2xx statuses to return as a response instead of throwing (for example 304 or 404).
    public var acceptedStatuses: Set<Int>

    public init(
        method: HTTPMethod = .get,
        url: URL,
        headers: [String: String] = [:],
        query: [URLQueryItem] = [],
        body: Data? = nil,
        bearer: String? = nil,
        timeout: TimeInterval = 30,
        acceptedStatuses: Set<Int> = [],
    ) {
        self.method = method
        self.url = url
        self.headers = headers
        self.query = query
        self.body = body
        self.bearer = bearer
        self.timeout = timeout
        self.acceptedStatuses = acceptedStatuses
    }

    /// A request with a JSON body (`Content-Type: application/json`).
    public static func json<Body: Encodable>(
        _ method: HTTPMethod = .post,
        _ url: URL,
        body: Body,
        encoder: JSONEncoder = JSONEncoder(),
        headers: [String: String] = [:],
        bearer: String? = nil,
        timeout: TimeInterval = 30,
    ) throws -> HTTPRequest {
        var all = headers
        all["Content-Type"] = "application/json"
        let data: Data
        do { data = try encoder.encode(body) } catch { throw HTTPError.encoding(String(describing: type(of: error))) }
        return HTTPRequest(method: method, url: url, headers: all, body: data, bearer: bearer, timeout: timeout)
    }

    /// A request with an `application/x-www-form-urlencoded` body (OAuth token endpoints).
    public static func form(
        _ method: HTTPMethod = .post,
        _ url: URL,
        fields: [(String, String)],
        headers: [String: String] = [:],
        bearer: String? = nil,
        timeout: TimeInterval = 30,
    ) -> HTTPRequest {
        var all = headers
        all["Content-Type"] = "application/x-www-form-urlencoded"
        let text = fields.map { "\(formEncode($0.0))=\(formEncode($0.1))" }.joined(separator: "&")
        return HTTPRequest(method: method, url: url, headers: all, body: Data(text.utf8), bearer: bearer, timeout: timeout)
    }
}

/// Percent-encodes for `application/x-www-form-urlencoded`: unreserved ASCII passes, space is `+`.
public func formEncode(_ text: String) -> String {
    var out = ""
    for byte in text.utf8 {
        switch byte {
        case UInt8(ascii: "A")...UInt8(ascii: "Z"), UInt8(ascii: "a")...UInt8(ascii: "z"),
             UInt8(ascii: "0")...UInt8(ascii: "9"), UInt8(ascii: "-"), UInt8(ascii: "."),
             UInt8(ascii: "_"), UInt8(ascii: "~"):
            out.unicodeScalars.append(Unicode.Scalar(byte))
        case UInt8(ascii: " "):
            out += "+"
        default:
            out += String(format: "%%%02X", byte)
        }
    }
    return out
}

/// A completed response. Header names are lower-cased.
public struct HTTPResponse: Sendable, Equatable {
    public let status: Int
    public let headers: [String: String]
    public let data: Data

    public init(status: Int, headers: [String: String] = [:], data: Data = Data()) {
        self.status = status
        self.headers = Dictionary(uniqueKeysWithValues: headers.map { ($0.key.lowercased(), $0.value) })
        self.data = data
    }

    public func header(_ name: String) -> String? { headers[name.lowercased()] }

    public var text: String { String(decoding: data, as: UTF8.self) }

    /// Decodes the body; a body that does not match `T` is ``HTTPError/decoding(_:)``.
    public func decode<T: Decodable>(_ type: T.Type = T.self, decoder: JSONDecoder = JSONDecoder()) throws -> T {
        do { return try decoder.decode(T.self, from: data) } catch {
            throw HTTPError.decoding(String(describing: Swift.type(of: error)))
        }
    }
}

/// Typed failure. Messages never contain a bearer token or a request body.
public enum HTTPError: Error, Equatable, Sendable {
    /// The URL is not HTTPS (and not a loopback host), or carries credentials.
    case insecureURL(String)
    /// The destination's address class is not allowed by the ``NetPolicy`` in force.
    case blockedDestination(String)
    case encoding(String)
    case decoding(String)
    /// The request never produced an HTTP answer (offline, DNS, TLS, refused redirect).
    case transport(String)
    case timeout
    case cancelled
    /// A non-2xx status. `message` is the server's `error` / `message` / `detail` text, bounded.
    case status(code: Int, message: String)
    case tooLarge(limit: Int)
    /// A download whose size or SHA-256 differs from what the caller expected.
    case integrity(String)

    /// The status code for ``status(code:message:)``, else nil.
    public var statusCode: Int? {
        if case let .status(code, _) = self { return code }
        return nil
    }
}

/// Retry policy for transient failures. Only idempotent verbs are retried; `.none` sends once.
public struct HTTPRetryPolicy: Sendable {
    public var maxAttempts: Int
    public var baseDelay: TimeInterval
    public var maxDelay: TimeInterval

    public init(maxAttempts: Int, baseDelay: TimeInterval = 0.5, maxDelay: TimeInterval = 8) {
        self.maxAttempts = max(1, maxAttempts)
        self.baseDelay = baseDelay
        self.maxDelay = maxDelay
    }

    public static let none = HTTPRetryPolicy(maxAttempts: 1)
}
