#if os(macOS)
/// Provider ids the notch face needs without reaching into Mac-only files.
enum ProviderIDs {
    /// Nearby sharing: the Send cell (see Sharing/NearbySharing.swift).
    static let send = "system-send"
    /// The default Claude profile's id; `ClaudeProfile.defaultID` forwards here.
    static let claude = "claude"

    /// Whether a provider id names a Claude profile, default or otherwise.
    static func isClaude(providerID: String) -> Bool {
        providerID == claude || providerID.hasPrefix(claude + "-")
    }

    /// The slug back out of a Claude provider id, for code that only has the id.
    static func claudeSlug(fromProviderID id: String) -> String? {
        guard id.hasPrefix(claude + "-") else { return nil }
        let slug = String(id.dropFirst(claude.count + 1))
        return slug.isEmpty ? nil : slug
    }
}
#endif
