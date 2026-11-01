import Foundation

/// The block list (#354: "a block list, the opposite of an allow list"). The agent may use any tool on any
/// computer or chat except what is blocked here. Two layers: a hard floor the owner cannot lower, and the
/// owner's own entries. Blocked chats are hidden from `chat_list`, unreadable and unsendable; a blocked
/// computer is unreachable for every tool. Matching is by computer name or id, and by chat title substring,
/// case-insensitive. The model never edits this list.
enum AgentBlocklist {
    static let storageKey = "phoneAgentBlocked"

    /// Never actionable, whatever the owner has typed: Pulse's own approval and settings surfaces, System
    /// Settings, and security prompts (passwords, keychain, administrator or sudo requests).
    static let hardFloor: [String] = [
        "pulse approval", "pulse settings", "system settings", "system preferences", "security",
        "password", "keychain", "administrator", "sudo", "authentication", "touch id", "face id",
    ]

    static var owner: [String] {
        get { (UserDefaults.standard.stringArray(forKey: storageKey) ?? []).map(normal).filter { !$0.isEmpty } }
        set { UserDefaults.standard.set(newValue.map(normal).filter { !$0.isEmpty }, forKey: storageKey) }
    }

    private static func normal(_ s: String) -> String { s.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }

    static func blocksChat(title: String) -> Bool {
        let t = title.lowercased()
        return (hardFloor + owner).contains { t.contains($0) }
    }

    static func blocksComputer(id: String, name: String) -> Bool {
        let own = owner
        return own.contains(normal(id)) || own.contains(normal(name))
    }
}
