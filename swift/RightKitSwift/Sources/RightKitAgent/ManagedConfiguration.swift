import Foundation

// ENG-112: admin-pushed AppConfig supplies validated, deduplicated connection
// presets through Apple's declarative `ManagedApp` stream and legacy
// `com.apple.configuration.managed` defaults key. The system/MDM owns writes;
// `ManagedConfigurationProvider` owns live observation, merge, and projection.
//
// A managed preset never carries a credential value. It may *name* a secret
// slot (a `SecretStore.SecretKind` + stable id, e.g. "forge-token" /
// "cr-forge") so the app knows which already-stored (or user-entered) secret
// to pair with the preset; the admin never pushes the secret itself through
// MDM.

/// One admin-supplied connection preset. Mirrors the connector shapes in
/// `CloudflareConnector.swift` / `LocalFlyioConnector.swift`: a preset names
/// a host/endpoint and, optionally, which existing secret slot to pair with
/// it — it never stores the secret value.
public struct ManagedConnectionPreset: Codable, Sendable, Equatable, Identifiable {
    /// The one connection type the app iOS currently renders & applies.
    /// Do not declare a kind until its iOS surface has a real consumer.
    public enum Kind: String, Codable, Sendable, CaseIterable {
        /// A selected provider account's base URL override.
        case providerBaseURL = "provider-base-url"
    }

    /// Stable identity for dedup: `kind` + lowercased host component of `endpoint`.
    /// Two presets naming the same kind and host are the same preset even if
    /// their display name or path differs.
    public var id: String { Self.identity(kind: kind, endpoint: endpoint) }

    public let kind: Kind
    /// Human-readable label shown in UI (e.g. "Acme Corp Daemon").
    public let displayName: String
    /// Must be `https://` (see `ManagedConfigurationValidator`); no bare host,
    /// no `http://`, no embedded credentials or query-string secrets.
    public let endpoint: String
    /// Optional: which already-known secret slot (never a value) this preset
    /// pairs with, e.g. `SecretStore.SecretKind.forgeToken` + a stable id.
    public let secretSlotKind: String?
    public let secretSlotID: String?

    public init(
        kind: Kind,
        displayName: String,
        endpoint: String,
        secretSlotKind: String? = nil,
        secretSlotID: String? = nil
    ) {
        self.kind = kind
        self.displayName = displayName
        self.endpoint = endpoint
        self.secretSlotKind = secretSlotKind
        self.secretSlotID = secretSlotID
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: ManagedConfigurationCodingKey.self)
        let permitted: Set<String> = [
            "kind", "displayName", "endpoint", "secretSlotKind", "secretSlotID",
        ]
        guard Set(container.allKeys.map(\.stringValue)).isSubset(of: permitted) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Managed presets cannot carry unknown or secret fields.")
            )
        }
        let kind = try container.decode(Kind.self, forKey: .kind)
        let displayName = try container.decode(String.self, forKey: .displayName)
        let endpoint = try container.decode(String.self, forKey: .endpoint)
        let secretSlotKind = try container.decodeIfPresent(String.self, forKey: .secretSlotKind)
        let secretSlotID = try container.decodeIfPresent(String.self, forKey: .secretSlotID)
        self.init(
            kind: kind,
            displayName: displayName,
            endpoint: endpoint,
            secretSlotKind: secretSlotKind,
            secretSlotID: secretSlotID
        )
    }

    static func identity(kind: Kind, endpoint: String) -> String {
        let host = URL(string: endpoint)?.host?.lowercased() ?? endpoint.lowercased()
        return "\(kind.rawValue)::\(host)"
    }
}

/// Top-level payload shape delivered by either channel: `{"presets": [...]}`.
public struct ManagedConfigurationPayload: Codable, Sendable, Equatable {
    public let presets: [ManagedConnectionPreset]

    public init(presets: [ManagedConnectionPreset]) {
        self.presets = presets
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: ManagedConfigurationCodingKey.self)
        guard Set(container.allKeys.map(\.stringValue)) == ["presets"] else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Managed configuration only accepts presets.")
            )
        }
        presets = try container.decode([ManagedConnectionPreset].self, forKey: .presets)
    }
}

private struct ManagedConfigurationCodingKey: CodingKey, Hashable {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }

    static let kind = ManagedConfigurationCodingKey(stringValue: "kind")!
    static let displayName = ManagedConfigurationCodingKey(stringValue: "displayName")!
    static let endpoint = ManagedConfigurationCodingKey(stringValue: "endpoint")!
    static let secretSlotKind = ManagedConfigurationCodingKey(stringValue: "secretSlotKind")!
    static let secretSlotID = ManagedConfigurationCodingKey(stringValue: "secretSlotID")!
    static let presets = ManagedConfigurationCodingKey(stringValue: "presets")!
}

/// Validates and deduplicates raw managed presets. Rejects malformed or
/// non-HTTPS entries rather than silently dropping fields — a rejected
/// preset is logged as invalid and excluded, it never falls back to a
/// partially-filled value.
public enum ManagedConfigurationValidator {
    /// Validates one preset. `nil` means "reject this entry."
    public static func validate(_ preset: ManagedConnectionPreset) -> ManagedConnectionPreset? {
        let trimmedName = preset.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else { return nil }

        guard let url = URL(string: preset.endpoint),
            let scheme = url.scheme?.lowercased(),
            scheme == "https",
            let host = url.host, !host.isEmpty
        else { return nil }

        // Belt-and-suspenders: a preset must never carry credential material
        // in the endpoint itself (basic-auth userinfo or an obvious secret
        // query parameter).
        if url.user != nil || url.password != nil { return nil }
        let secretQueryKeys: Set<String> = [
            "token", "accesstoken", "refreshtoken", "idtoken", "apikey",
            "authorization", "password", "credential", "secret",
        ]
        if URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.contains(where: { item in
            let normalized = item.name.lowercased().filter { $0.isLetter || $0.isNumber }
            return secretQueryKeys.contains(normalized)
        }) == true {
            return nil
        }

        let slotKind = preset.secretSlotKind?.trimmingCharacters(in: .whitespacesAndNewlines)
        let slotID = preset.secretSlotID?.trimmingCharacters(in: .whitespacesAndNewlines)
        // A slot is only a reference, but a half-filled reference is not
        // actionable and must not become an implicit credential fallback.
        guard (slotKind == nil) == (slotID == nil),
            slotKind.map({ !$0.isEmpty }) ?? true,
            slotID.map({ !$0.isEmpty }) ?? true
        else { return nil }

        return ManagedConnectionPreset(
            kind: preset.kind,
            displayName: trimmedName,
            endpoint: preset.endpoint,
            secretSlotKind: slotKind,
            secretSlotID: slotID
        )
    }

    /// Validates a raw list, dropping invalid entries, then dedups by
    /// `ManagedConnectionPreset.id` (kind + host). First occurrence wins —
    /// callers control precedence by the order they pass in (declarative
    /// entries before legacy entries, per `ManagedConfigurationProvider`).
    public static func validateAndDedup(_ raw: [ManagedConnectionPreset]) -> [ManagedConnectionPreset] {
        var seen = Set<String>()
        var result: [ManagedConnectionPreset] = []
        for candidate in raw {
            guard let valid = validate(candidate) else { continue }
            guard seen.insert(valid.id).inserted else { continue }
            result.append(valid)
        }
        return result
    }
}
