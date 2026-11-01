import Foundation
import Darwin

/// One check for every outbound page load the article/carousel paths make:
/// the URLSession fetch, each of its redirects, and every WKWebView
/// navigation. http/https only, and no host that is obviously local: loopback,
/// private, link-local, CGNAT, multicast, unique-local, `localhost`, `.local`,
/// `.internal`, or a single-label name.
///
/// Limit, stated plainly: this inspects the URL's host as written. A public DNS
/// name that *resolves* to a private address is not caught (that needs a
/// resolver hook iOS does not give URLSession/WKWebView), and sub-resources a
/// page loads for itself (images, XHR) are not navigations and are not checked.
enum FetchURLPolicy {
    static func isAllowed(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = url.host?.lowercased(), !host.isEmpty else { return false }
        return isAllowedHost(host)
    }

    static func isAllowedHost(_ rawHost: String) -> Bool {
        let host = rawHost.trimmingCharacters(in: CharacterSet(charactersIn: "[]."))
        // A percent-encoded host could hide an address from the checks below.
        guard !host.isEmpty, !host.contains("%") else { return false }

        if host.contains(":") {
            var addr = in6_addr()
            guard inet_pton(AF_INET6, host, &addr) == 1 else { return false }
            return isPublic(v6: addr)
        }
        // inet_aton also accepts the shorthand forms browsers do ("127.1",
        // "2130706433", "0x7f.1"), so those cannot slip past a dotted-quad regex.
        var v4 = in_addr()
        if inet_aton(host, &v4) != 0 {
            return isPublic(v4: UInt32(bigEndian: v4.s_addr))
        }
        if host == "localhost" || !host.contains(".") { return false }
        for suffix in [".localhost", ".local", ".internal", ".lan", ".home.arpa", ".localdomain"]
        where host.hasSuffix(suffix) { return false }
        return true
    }

    private static func isPublic(v4 a: UInt32) -> Bool {
        let b0 = a >> 24, b1 = (a >> 16) & 0xFF
        if b0 == 0 || b0 == 10 || b0 == 127 || b0 >= 224 { return false }
        if b0 == 100 && (64...127).contains(b1) { return false }
        if b0 == 169 && b1 == 254 { return false }
        if b0 == 172 && (16...31).contains(b1) { return false }
        if b0 == 192 && b1 == 168 { return false }
        if b0 == 192 && b1 == 0 && ((a >> 8) & 0xFF) == 0 { return false }
        return true
    }

    private static func isPublic(v6 addr: in6_addr) -> Bool {
        let b = withUnsafeBytes(of: addr) { Array($0) }
        if b.dropLast().allSatisfy({ $0 == 0 }) && (b[15] == 0 || b[15] == 1) { return false }
        if b[0] == 0xFF { return false }
        if b[0] & 0xFE == 0xFC { return false }
        if b[0] == 0xFE && b[1] & 0xC0 == 0x80 { return false }
        // IPv4-compatible (::a.b.c.d), NAT64 (64:ff9b::/96) and 6to4 (2002::/16)
        // all carry an IPv4 address the checks above cannot see: refuse them.
        if b[0..<12].allSatisfy({ $0 == 0 }) { return false }
        if b[0] == 0x00 && b[1] == 0x64 && b[2] == 0xFF && b[3] == 0x9B { return false }
        if b[0] == 0x20 && b[1] == 0x02 { return false }
        // ::ffff:a.b.c.d — judge the embedded IPv4 address.
        if b[0..<10].allSatisfy({ $0 == 0 }) && b[10] == 0xFF && b[11] == 0xFF {
            let v4 = (UInt32(b[12]) << 24) | (UInt32(b[13]) << 16) | (UInt32(b[14]) << 8) | UInt32(b[15])
            return isPublic(v4: v4)
        }
        return true
    }
}

/// Stops URLSession from following a redirect to a host the policy refuses.
final class PolicyRedirectGuard: NSObject, URLSessionTaskDelegate {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(request.url.map(FetchURLPolicy.isAllowed) == true ? request : nil)
    }
}
