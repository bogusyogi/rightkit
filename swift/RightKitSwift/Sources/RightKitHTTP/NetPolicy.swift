import Foundation
#if canImport(Darwin)
import Darwin
#endif

// Outbound address policy for user-configured endpoints (issue 667). It extends the URL policy in
// ``HTTPClient`` rather than adding a second one: classification mirrors `rightkit-http`'s
// `netpolicy.rs` (`classify_ip`, `AddressClass`), so Rust and Swift reach the same verdict for the
// same address. Default is public-web only. Loopback and private networks are opt-in, per policy
// value (create one for each endpoint the user configured). Link-local (which holds the cloud
// metadata address 169.254.169.254), unspecified, multicast and reserved destinations can never be
// allowed.

/// Address class of a destination.
public enum AddressClass: String, Sendable, Equatable, Hashable, CaseIterable {
    case `public`
    case loopback
    /// `169.254/16`, `fe80::/10`, and cloud metadata hosts.
    case linkLocal
    /// RFC 1918, CGNAT `100.64/10`, ULA `fc00::/7`, site-local `fec0::/10`, and `.local` names.
    case privateNetwork
    /// Documentation, benchmarking, protocol assignments, `240/4`, Teredo.
    case reserved
    case unspecified
    case multicast
}

/// Resolves a host name to IP literals, so a name that points at a denied address is caught.
public protocol HostResolving: Sendable {
    func resolve(host: String) async throws -> [String]
}

/// `getaddrinfo`. The policy check and the later connection are two lookups, so this narrows but does
/// not close a DNS-rebinding window; pin the endpoint to an IP literal where that matters.
public struct SystemHostResolver: HostResolving {
    public init() {}

    public func resolve(host: String) async throws -> [String] {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                var hints = addrinfo()
                hints.ai_socktype = SOCK_STREAM
                var result: UnsafeMutablePointer<addrinfo>?
                let status = getaddrinfo(host, nil, &hints, &result)
                guard status == 0, let first = result else {
                    continuation.resume(throwing: URLError(.cannotFindHost))
                    return
                }
                defer { freeaddrinfo(result) }
                var out: [String] = []
                var cursor: UnsafeMutablePointer<addrinfo>? = first
                while let info = cursor {
                    var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                    if getnameinfo(info.pointee.ai_addr, info.pointee.ai_addrlen, &buffer, socklen_t(buffer.count), nil, 0, NI_NUMERICHOST) == 0 {
                        out.append(String(cString: buffer))
                    }
                    cursor = info.pointee.ai_next
                }
                continuation.resume(returning: out)
            }
        }
    }
}

/// Which non-public destinations an endpoint may reach. Public hosts are always reachable over HTTPS.
public struct NetPolicy: Sendable, Equatable {
    /// Only ``AddressClass/loopback`` and ``AddressClass/privateNetwork`` can be allowed; anything
    /// else in the set is ignored.
    public let allowing: Set<AddressClass>

    public init(allowing: Set<AddressClass> = []) {
        self.allowing = allowing.intersection([.loopback, .privateNetwork])
    }

    /// The default: public hosts only. Loopback, private and link-local are denied.
    public static let publicOnly = NetPolicy()
    /// For a user-configured local model server (Ollama, LM Studio) on this machine.
    public static let allowingLoopback = NetPolicy(allowing: [.loopback])
    /// Loopback and LAN, for a self-hosted endpoint.
    public static let allowingLocalNetworks = NetPolicy(allowing: [.loopback, .privateNetwork])

    func permits(_ addressClass: AddressClass) -> Bool {
        addressClass == .public || allowing.contains(addressClass)
    }

    // MARK: Classification

    /// Classifies an IP literal (legacy IPv4 spellings such as `2130706433` or `0x7f.1` included) or
    /// well-known name (`localhost`, `*.localhost`, `*.local`, cloud metadata hosts). A DNS name that
    /// is none of those returns nil: it needs resolution.
    public static func classify(host rawHost: String) -> AddressClass? {
        var host = rawHost.lowercased()
        if host.hasPrefix("["), host.hasSuffix("]") { host = String(host.dropFirst().dropLast()) }
        if host.hasSuffix(".") { host.removeLast() }
        guard !host.isEmpty else { return .unspecified }
        if let v6 = ipv6Bytes(host) { return classifyV6(v6) }
        if let v4 = ipv4Bytes(host) { return classifyV4(v4) }
        if host == "localhost" || host.hasSuffix(".localhost") { return .loopback }
        if host.hasSuffix(".local") { return .privateNetwork }
        if host == "metadata.google.internal" { return .linkLocal }
        return nil
    }

    private static func ipv4Bytes(_ host: String) -> [UInt8]? {
        guard let first = host.unicodeScalars.first, first.value >= 48, first.value <= 57 else { return nil }
        var address = in_addr()
        guard inet_aton(host, &address) != 0 else { return nil }
        let value = UInt32(bigEndian: address.s_addr)
        return [UInt8(value >> 24), UInt8((value >> 16) & 0xff), UInt8((value >> 8) & 0xff), UInt8(value & 0xff)]
    }

    private static func ipv6Bytes(_ host: String) -> [UInt8]? {
        guard host.contains(":") else { return nil }
        var text = host
        if let zone = text.firstIndex(of: "%") { text = String(text[..<zone]) }
        var address = in6_addr()
        guard inet_pton(AF_INET6, text, &address) == 1 else { return nil }
        return withUnsafeBytes(of: &address) { Array($0) }
    }

    static func classifyV4(_ ip: [UInt8]) -> AddressClass {
        let (a, b, c) = (ip[0], ip[1], ip[2])
        if a == 127 { return .loopback }
        if a == 0 { return .unspecified }
        if a == 169 && b == 254 { return .linkLocal }
        if a == 10 || (a == 172 && (16...31).contains(b)) || (a == 192 && b == 168) || (a == 100 && (64...127).contains(b)) {
            return .privateNetwork
        }
        if (224...239).contains(a) { return .multicast }
        if a >= 240 || (a == 192 && b == 0 && c == 0) || (a == 192 && b == 0 && c == 2) || (a == 198 && (b == 18 || b == 19))
            || (a == 198 && b == 51 && c == 100) || (a == 203 && b == 0 && c == 113) { return .reserved }
        return .public
    }

    static func classifyV6(_ ip: [UInt8]) -> AddressClass {
        let zeroPrefix = ip[0..<10].allSatisfy { $0 == 0 }
        if ip.allSatisfy({ $0 == 0 }) { return .unspecified }
        if ip[0..<15].allSatisfy({ $0 == 0 }) && ip[15] == 1 { return .loopback }
        // Forms that embed an IPv4 address take the embedded class.
        if zeroPrefix && ip[10] == 0xff && ip[11] == 0xff { return classifyV4(Array(ip[12..<16])) }
        if ip[0..<12].allSatisfy({ $0 == 0 }) { return classifyV4(Array(ip[12..<16])) }
        if ip[0] == 0x00, ip[1] == 0x64, ip[2] == 0xff, ip[3] == 0x9b, ip[4..<12].allSatisfy({ $0 == 0 }) { return classifyV4(Array(ip[12..<16])) }
        if ip[0] == 0x20 && ip[1] == 0x02 { return classifyV4(Array(ip[2..<6])) }
        // AWS's IPv6 metadata endpoint sits inside ULA space and must never be allowed.
        if ip[0] == 0xfd && ip[1] == 0x00 && ip[2] == 0x0e && ip[3] == 0xc2 { return .linkLocal }
        if ip[0] == 0xfe && (ip[1] & 0xc0) == 0x80 { return .linkLocal }
        if ip[0] == 0xfe && (ip[1] & 0xc0) == 0xc0 { return .privateNetwork }
        if (ip[0] & 0xfe) == 0xfc { return .privateNetwork }
        if ip[0] == 0xff { return .multicast }
        if ip[0] == 0x20 && ip[1] == 0x01 && ((ip[2] == 0x0d && ip[3] == 0xb8) || (ip[2] == 0 && ip[3] == 0)) { return .reserved }
        return .public
    }

    // MARK: Checks

    /// Synchronous check of the URL as written: scheme, credentials, and the host when it is an IP
    /// literal or well-known name. A DNS name is judged as public here (so HTTPS passes and plain
    /// HTTP fails); use ``vet(_:resolver:)`` to resolve it.
    public func isAllowed(_ url: URL) -> Bool {
        (try? evaluate(url, resolved: nil, pendingResolution: false)) != nil
    }

    /// Full check: ``isAllowed(_:)`` plus resolution of DNS names. One denied address denies the host,
    /// and a name that resolves to nothing is denied. A resolver failure is not a denial for HTTPS (the
    /// connection would fail the same way) but is for plain HTTP.
    public func vet(_ url: URL, resolver: any HostResolving = SystemHostResolver()) async throws {
        try evaluate(url, resolved: nil, pendingResolution: true)
        guard let host = url.host, Self.classify(host: host) == nil else { return }
        let addresses: [String]
        do { addresses = try await resolver.resolve(host: host) } catch {
            // Without an answer plain HTTP cannot be shown to stay on an opted-in network.
            if url.scheme?.lowercased() == "http" { throw HTTPError.insecureURL(host) }
            return
        }
        guard !addresses.isEmpty else { throw HTTPError.blockedDestination("host resolves to no address") }
        try evaluate(url, resolved: addresses, pendingResolution: false)
    }

    @discardableResult
    private func evaluate(_ url: URL, resolved: [String]?, pendingResolution: Bool) throws -> [AddressClass] {
        guard let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http" else {
            throw HTTPError.insecureURL(url.host ?? "")
        }
        guard let host = url.host, !host.isEmpty, url.user == nil, url.password == nil else {
            throw HTTPError.insecureURL(url.host ?? "")
        }
        let classes: [AddressClass]
        if let literal = Self.classify(host: host) {
            classes = [literal]
        } else if let resolved {
            classes = resolved.map { Self.classify(host: $0) ?? .reserved }
        } else if pendingResolution {
            classes = []
        } else {
            classes = [.public]
        }
        for addressClass in classes where !permits(addressClass) {
            throw HTTPError.blockedDestination("\(addressClass.rawValue) destinations are not allowed")
        }
        // Plain HTTP only to a destination the policy opened (loopback or LAN), never to the public internet.
        if scheme == "http", classes.contains(.public) { throw HTTPError.insecureURL(host) }
        return classes
    }

    // MARK: Session helper

    /// A `URLSession` on an ephemeral configuration whose redirects are checked against this policy
    /// (blocked hops are refused and `Authorization` is dropped on a cross-host hop). The first URL is
    /// not checked by the session; call ``vet(_:resolver:)`` on it, or send through ``HTTPClient``.
    public func makeSession(configuration: URLSessionConfiguration? = nil, resolver: any HostResolving = SystemHostResolver()) -> URLSession {
        let config = configuration ?? {
            let c = URLSessionConfiguration.ephemeral
            c.httpCookieStorage = nil
            c.httpShouldSetCookies = false
            c.urlCache = nil
            return c
        }()
        return URLSession(configuration: config, delegate: PolicyRedirectGuard(policy: self, resolver: resolver), delegateQueue: nil)
    }
}
