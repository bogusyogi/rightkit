import Foundation

/// The network seam under ``HTTPClient``. Production uses ``URLSessionTransport``; tests inject a
/// fake that never opens a socket.
public protocol HTTPTransport: Sendable {
    /// One buffered exchange.
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
    /// A streamed exchange: the response head, then the body in chunks (downloads).
    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>)
}

/// `URLSession` transport on an ephemeral configuration: no cookies, no credential storage, no
/// URL cache. A redirect off HTTPS is refused, and `Authorization` is dropped when a redirect
/// changes host, so a bearer token never reaches a different origin.
public struct URLSessionTransport: HTTPTransport {
    private let session: URLSession
    private let redirectGuard: PolicyRedirectGuard

    /// - Parameter policy: when set, every redirect hop is checked against it (see ``NetPolicy``).
    public init(configuration: URLSessionConfiguration? = nil, policy: NetPolicy? = nil, resolver: any HostResolving = SystemHostResolver()) {
        self.redirectGuard = PolicyRedirectGuard(policy: policy, resolver: resolver)
        let config = configuration ?? {
            let c = URLSessionConfiguration.ephemeral
            c.httpCookieStorage = nil
            c.httpShouldSetCookies = false
            c.urlCache = nil
            c.requestCachePolicy = .reloadIgnoringLocalCacheData
            return c
        }()
        self.session = URLSession(configuration: config)
    }

    public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await session.data(for: request, delegate: redirectGuard)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return (data, http)
    }

    public func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) {
        let (bytes, response) = try await session.bytes(for: request, delegate: redirectGuard)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        let source = UncheckedSendable(value: bytes)
        let chunks = AsyncThrowingStream<Data, Error> { continuation in
            let task = Task {
                do {
                    var buffer = Data()
                    buffer.reserveCapacity(65_536)
                    for try await byte in source.value {
                        buffer.append(byte)
                        if buffer.count >= 65_536 {
                            continuation.yield(buffer)
                            buffer.removeAll(keepingCapacity: true)
                        }
                    }
                    if !buffer.isEmpty { continuation.yield(buffer) }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
        return (http, chunks)
    }
}

/// `URLSession.AsyncBytes` is consumed by exactly one task; this carries it across the boundary.
private struct UncheckedSendable<Value>: @unchecked Sendable { let value: Value }

/// Redirect policy for a session: every hop must satisfy the ``NetPolicy`` (or, with none, be HTTPS
/// or loopback HTTP), and `Authorization` is dropped when a hop changes host. Attach it as a session or
/// task delegate; ``NetPolicy/makeSession(configuration:resolver:)`` does that for you.
public final class PolicyRedirectGuard: NSObject, URLSessionTaskDelegate, Sendable {
    private let policy: NetPolicy?
    private let resolver: any HostResolving

    public init(policy: NetPolicy?, resolver: any HostResolving = SystemHostResolver()) {
        self.policy = policy
        self.resolver = resolver
        super.init()
    }

    /// Opt-in classes for redirect hops, for example `[.loopback, .privateNetwork]`.
    public convenience init(allowing: Set<AddressClass>) { self.init(policy: NetPolicy(allowing: allowing)) }

    public func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
    ) async -> URLRequest? {
        guard let policy else { return Self.filter(newRequest: request, originalHost: task.originalRequest?.url?.host) }
        guard let url = request.url else { return nil }
        do { try await policy.vet(url, resolver: resolver) } catch { return nil }
        return Self.stripped(request, originalHost: task.originalRequest?.url?.host)
    }

    /// Legacy policy (no `NetPolicy`), tested directly.
    static func filter(newRequest request: URLRequest, originalHost: String?) -> URLRequest? {
        guard let url = request.url, HTTPClient.isPermitted(url, allowInsecureLoopback: true) else { return nil }
        return stripped(request, originalHost: originalHost)
    }

    static func stripped(_ request: URLRequest, originalHost: String?) -> URLRequest {
        var next = request
        if let originalHost, request.url?.host?.lowercased() != originalHost.lowercased() {
            next.setValue(nil, forHTTPHeaderField: "Authorization")
        }
        return next
    }
}

typealias RedirectGuard = PolicyRedirectGuard
