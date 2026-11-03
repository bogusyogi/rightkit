import Foundation

/// Why a URL was refused by ``URLPolicy``.
public enum URLPolicyViolation: Error, Equatable, Sendable {
    case malformed(String)
    case scheme(String)
    case missingHost
    case embeddedCredentials
    case port(Int)
    /// The host is a literal private, loopback, link-local or unspecified address, or a local-only name.
    case privateAddress(String)
    /// A single-label host such as `intranet`, which resolves only inside a private network.
    case localName(String)
}

/// Decides whether an outbound URL may be fetched. It guards the fetch against server-side request forgery.
///
/// The policy checks the URL as written: scheme, port, embedded credentials, and literal or local-only host names.
/// It cannot see what a public name resolves to. A caller that connects to a name must check the resolved address
/// with ``isPrivateAddress(_:)`` at connect time, or a public name that points at a private address can still be
/// reached (DNS rebinding).
public struct URLPolicy: Sendable, Equatable {
    /// Allowed schemes, lowercase. Default `https` only.
    public var allowedSchemes: Set<String>
    /// Allowed explicit ports. `nil` allows only each scheme's default port.
    public var allowedPorts: Set<Int>?
    /// Permit private, loopback and local-only hosts. Off by default; turn on only for a named, trusted local service.
    public var allowPrivateNetworks: Bool

    public init(
        allowedSchemes: Set<String> = ["https"],
        allowedPorts: Set<Int>? = nil,
        allowPrivateNetworks: Bool = false
    ) {
        self.allowedSchemes = allowedSchemes
        self.allowedPorts = allowedPorts
        self.allowPrivateNetworks = allowPrivateNetworks
    }

    /// The URL unchanged when it passes, otherwise the first violation found.
    public func validate(_ url: URL) throws -> URL {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw URLPolicyViolation.malformed(url.absoluteString)
        }
        let scheme = (components.scheme ?? "").lowercased()
        guard allowedSchemes.contains(scheme) else { throw URLPolicyViolation.scheme(scheme) }
        if components.user != nil || components.password != nil {
            throw URLPolicyViolation.embeddedCredentials
        }
        guard let rawHost = components.host, !rawHost.isEmpty else { throw URLPolicyViolation.missingHost }
        if let port = components.port {
            let allowed = allowedPorts?.contains(port) ?? false
            if allowedPorts != nil ? !allowed : !Self.isDefaultPort(port, scheme: scheme) {
                throw URLPolicyViolation.port(port)
            }
        }
        guard !allowPrivateNetworks else { return url }
        let host = Self.normalizedHost(rawHost)
        if Self.isLocalName(host) { throw URLPolicyViolation.localName(host) }
        if Self.isPrivateAddress(host) { throw URLPolicyViolation.privateAddress(host) }
        return url
    }

    /// True for literal IPv4 or IPv6 addresses that are loopback, private, link-local, shared, multicast or unspecified.
    /// Non-literal hosts return false: resolve them and call this again on the address.
    public static func isPrivateAddress(_ host: String) -> Bool {
        let text = normalizedHost(host).lowercased()
        if text.contains(":") {
            return isPrivateIPv6(text)
        }
        let octets = text.split(separator: ".", omittingEmptySubsequences: false).compactMap { UInt8($0) }
        guard octets.count == 4, text.split(separator: ".").count == 4 else { return false }
        let (a, b) = (octets[0], octets[1])
        switch a {
        case 0, 10, 127: return true
        case 100: return (64...127).contains(b)
        case 169: return b == 254
        case 172: return (16...31).contains(b)
        case 192: return b == 168 || (b == 0 && octets[2] == 0)
        case 198: return b == 18 || b == 19
        case 224...255: return true
        default: return false
        }
    }

    private static func isPrivateIPv6(_ text: String) -> Bool {
        if text == "::" || text == "::1" { return true }
        if let mapped = text.range(of: "::ffff:", options: [.anchored]) {
            return isPrivateAddress(String(text[mapped.upperBound...]))
        }
        let first = text.split(separator: ":", omittingEmptySubsequences: false).first ?? ""
        guard let group = UInt16(first, radix: 16) else { return false }
        if (group & 0xfe00) == 0xfc00 { return true } // unique local fc00::/7
        if (group & 0xffc0) == 0xfe80 { return true } // link-local fe80::/10
        if (group & 0xff00) == 0xff00 { return true } // multicast ff00::/8
        return false
    }

    private static func isLocalName(_ host: String) -> Bool {
        let lower = host.lowercased()
        if lower == "localhost" || lower.hasSuffix(".localhost") || lower.hasSuffix(".local") || lower.hasSuffix(".internal") {
            return true
        }
        // A single label has no dots. It resolves through search domains, so treat it as local.
        return !lower.contains(".") && !lower.contains(":") && !lower.isEmpty && UInt8(lower) == nil
    }

    private static func isDefaultPort(_ port: Int, scheme: String) -> Bool {
        switch scheme {
        case "https": port == 443
        case "http": port == 80
        default: false
        }
    }

    /// Lowercased host without IPv6 brackets or a trailing dot.
    private static func normalizedHost(_ host: String) -> String {
        var text = host.lowercased()
        if text.hasPrefix("[") && text.hasSuffix("]") { text = String(text.dropFirst().dropLast()) }
        if text.hasSuffix(".") { text.removeLast() }
        return text
    }
}
