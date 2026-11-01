import Foundation

/// Connection coordinates for one the app desktop daemon. The desktop app prints
/// `{"Port":…, "Token":…}` on `serve`; the PWA takes the same values as `?base=&token=`.
/// The iOS client pairs by receiving a base URL + bearer token (manual entry or a
/// scanned `coderight://pair?base=…&token=…` QR payload) and persists them after confirmation.
public struct DaemonConfig: Codable, Equatable, Sendable, Identifiable {
    public enum TargetKind: String, Codable, Equatable, Sendable {
        case computer
        case cloud

        public var label: String { self == .cloud ? "Cloud Workspace" : "Connected Computer" }
    }
    /// Stable identity for a paired daemon, derived from its base URL.
    public var id: String { base }

    /// Base URL, e.g. `http://192.168.1.20:5099` — no trailing slash.
    public var base: String
    /// Bearer token for `Authorization: Bearer …`.
    public var token: String
    /// Human label shown in the daemon picker (defaults to the host).
    public var label: String
    public var targetKind: TargetKind

    /// Whether transport protects the bearer token in transit.
    public var usesTLS: Bool {
        URL(string: base)?.scheme?.lowercased() == "https"
    }

    /// Human-readable origin used by pairing confirmation UI.
    public var origin: String {
        URL(string: base)?.host.map { host in
            if let port = URL(string: base)?.port { return "\(host):\(port)" }
            return host
        } ?? base
    }

    public init(
        base: String,
        token: String,
        label: String = "",
        targetKind: TargetKind = .computer
    ) {
        self.base = DaemonConfig.normalizeBase(base)
        self.token = token.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
        self.label = trimmedLabel.isEmpty ? DaemonConfig.hostLabel(from: base) : trimmedLabel
        self.targetKind = targetKind
    }

    private enum CodingKeys: String, CodingKey {
        case base
        case label
        case targetKind
        // Kept only so old Keychain records can be read and migrated. New writes never encode it.
        case token
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let base = try container.decode(String.self, forKey: .base)
        let label = try container.decodeIfPresent(String.self, forKey: .label) ?? ""
        let token = try container.decodeIfPresent(String.self, forKey: .token) ?? ""
        let targetKind = try container.decodeIfPresent(TargetKind.self, forKey: .targetKind) ?? .computer
        self.init(base: base, token: token, label: label, targetKind: targetKind)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(base, forKey: .base)
        try container.encode(label, forKey: .label)
        try container.encode(targetKind, forKey: .targetKind)
    }

    public static func normalizeBase(_ raw: String) -> String {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }

    public static func isValidBase(_ raw: String) -> Bool {
        guard let url = URL(string: normalizeBase(raw)),
              ["http", "https"].contains(url.scheme?.lowercased()),
              url.host?.isEmpty == false else { return false }
        return true
    }

    public static func hostLabel(from raw: String) -> String {
        guard let url = URL(string: normalizeBase(raw)), let host = url.host else { return "daemon" }
        if let port = url.port { return "\(host):\(port)" }
        return host
    }

    /// Parse a QR payload inside PairingView. External URL routing must not call this parser.
    /// PWA-style `?base=&token=` payloads are accepted by the scanner but still require Pair.
    public static func fromPairingURL(_ url: URL) -> DaemonConfig? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        let items = components.queryItems ?? []
        func value(_ name: String) -> String? {
            items.first(where: { $0.name == name })?.value?.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        guard let base = value("base"), let token = value("token"),
              isValidBase(base),
              !token.isEmpty else {
            return nil
        }
        let targetKind = value("target") == "cloud" ? TargetKind.cloud : .computer
        return DaemonConfig(
            base: base,
            token: token,
            label: value("label") ?? "",
            targetKind: targetKind
        )
    }
}
