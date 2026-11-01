import RightKitEngineClient
import Foundation
import Observation
import RightKitHub
import SwiftUI

// DTOs mirror `engine/src/chat.rs` (camelCase) and the interfaces in `hub/src/views/Chat.tsx`.

/// One linked computer and how its last poll went.
struct ChatLinkDTO: Decodable, Sendable, Hashable {
    let device: String
    let ssh: String
    let online: Bool
    let lastOkMs: Double?
    let lastErrorMs: Double?
    let lastError: String?
}

/// `chat_status`: the switch, whether Pulse's hub runs, chats here and the links.
struct ChatStatusDTO: Decodable, Sendable {
    let enabled: Bool
    let hubRunning: Bool
    let device: String
    let chatsHere: Int
    let links: [ChatLinkDTO]
}

/// A chat that can be messaged, here or on a linked computer.
struct ChatPeerDTO: Decodable, Sendable, Hashable {
    /// What a send takes: the chat id here, `<device>:<session>` elsewhere.
    let id: String
    let name: String
    /// "<chat title> on <device>".
    let display: String
    let device: String
    let local: Bool
    /// "claude" or "codex".
    let kind: String
    /// Claude: busy or idle. Codex: active or idle.
    let state: String
    /// "live", "stale" or "unknown".
    let liveness: String
    let updatedMs: Double?
    let folder: String
}

/// The receipt of one send.
struct ChatReceiptDTO: Decodable, Sendable {
    let msgId: String
    let chat: String
    let state: String
    let detail: String
}

struct ChatMessageDTO: Decodable, Sendable, Hashable {
    let id: String
    let chat: String
    let from: String
    let body: String
    let receivedMs: Double
}

struct ChatInboxDTO: Decodable, Sendable {
    let messages: [ChatMessageDTO]
    let evicted: Int
}

/// What a new link reported.
struct ChatLinkedDTO: Decodable, Sendable {
    let device: String
    let ssh: String
    let chats: Int
    let theirName: String?
}

struct ChatSendParams: Encodable, Sendable {
    let target: String
    let text: String
}

/// `chat_link` params. A nil `pulse` is omitted, which the engine reads like the page's `null`.
struct ChatLinkParams: Encodable, Sendable {
    let device: String
    let sshHost: String
    let pulse: String?
}

struct ChatUnlinkParams: Encodable, Sendable {
    let device: String
}

/// One computer's chats under its name.
struct ChatDeviceGroup: Sendable {
    let device: String
    let chats: [ChatPeerDTO]
}

/// Chat's state: status, chats, inbox, the draft and the link form. Mirrors `hub/src/views/Chat.tsx`.
/// The draft lives here, so it survives the view being rebuilt and switching sections.
@MainActor
@Observable
final class ChatModel {
    /// Local state (status and inbox) is read every 3 seconds, as the page does.
    static let liveSeconds = 3
    /// Chats, including linked computers over ssh, every 30 seconds.
    static let peersSeconds = 30
    /// The receipt states that mean the chat's side took the message, so the draft can go.
    static let tookIt: Set<String> = ["delivered", "sent", "queued", "held"]

    private let engine: any EngineClient

    private(set) var status: ChatStatusDTO?
    private(set) var liveError: String?
    private(set) var inbox: ChatInboxDTO?
    var inboxOpen = false
    private(set) var peers: [ChatPeerDTO]?
    private(set) var peersError: String?
    var query = ""
    var selectedId: String?
    var draft = ""
    private(set) var sending = false
    private(set) var receipt: ChatReceiptDTO?
    private(set) var sendError: String?
    var linkDevice = ""
    var linkSsh = ""
    var linkPulse = ""
    private(set) var linking = false
    private(set) var linkNote: String?
    private(set) var linkError: String?
    /// The computer the Unlink confirmation is about. Stays set until the dialog is answered.
    private(set) var unlinkTarget: String?
    var confirmingUnlink = false

    /// One chat listing at a time: linked computers answer over ssh, which can take seconds.
    private var peersBusy = false

    init(engine: any EngineClient) {
        self.engine = engine
    }

    // MARK: Derived

    /// The chats matching the search box.
    var visiblePeers: [ChatPeerDTO] {
        let list = peers ?? []
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if q.isEmpty { return list }
        return list.filter {
            [$0.name, $0.device, $0.kind, $0.folder, $0.state].joined(separator: " ").lowercased().contains(q)
        }
    }

    /// The selected chat, if it is still listed.
    var chosen: ChatPeerDTO? {
        guard let selectedId else { return nil }
        return (peers ?? []).first { $0.id == selectedId }
    }

    var canSend: Bool {
        chosen != nil && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !sending
    }

    var canLink: Bool {
        !linkDevice.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !linkSsh.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !linking
    }

    /// Chats grouped by computer: this computer first, then the linked ones in the order given.
    var groups: [ChatDeviceGroup] {
        var order: [String] = []
        var byDevice: [String: [ChatPeerDTO]] = [:]
        for chat in visiblePeers {
            if byDevice[chat.device] == nil { order.append(chat.device) }
            byDevice[chat.device, default: []].append(chat)
        }
        let local = order.filter { device in byDevice[device]?.contains { $0.local } ?? false }
        let linked = order.filter { !local.contains($0) }
        return (local + linked).map { ChatDeviceGroup(device: $0, chats: byDevice[$0] ?? []) }
    }

    // MARK: Bindings (text fields)

    var queryBinding: Binding<String> { binding(get: { self.query }, set: { self.query = $0 }) }
    var draftBinding: Binding<String> { binding(get: { self.draft }, set: { self.draft = $0 }) }
    var linkDeviceBinding: Binding<String> { binding(get: { self.linkDevice }, set: { self.linkDevice = $0 }) }
    var linkSshBinding: Binding<String> { binding(get: { self.linkSsh }, set: { self.linkSsh = $0 }) }
    var linkPulseBinding: Binding<String> { binding(get: { self.linkPulse }, set: { self.linkPulse = $0 }) }
    var confirmingUnlinkBinding: Binding<Bool> {
        Binding(
            get: { MainActor.assumeIsolated { self.confirmingUnlink } },
            set: { value in MainActor.assumeIsolated { self.confirmingUnlink = value } }
        )
    }

    private func binding(
        get: @escaping @MainActor () -> String,
        set: @escaping @MainActor (String) -> Void
    ) -> Binding<String> {
        Binding(
            get: { MainActor.assumeIsolated { get() } },
            set: { value in MainActor.assumeIsolated { set(value) } }
        )
    }

    // MARK: Loading

    /// Reads status, inbox and chats once. `preparedView` awaits this so the off-screen render has data.
    func load() async {
        await refreshLive()
        await refreshPeers()
    }

    /// Local state every 3 seconds while the section is shown (the view's `.task` is cancelled
    /// when it goes away).
    // EVENTS: chat inbox and status, until events.subscribe. No chat event exists on the engine
    // yet (the page polls the same way), so incoming messages arrive only through this loop.
    func liveLoop(loadFirst: Bool) async {
        if loadFirst { await refreshLive() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(Self.liveSeconds))
            if Task.isCancelled { break }
            await refreshLive()
        }
    }

    /// Chats, including linked computers, every 30 seconds while the section is shown.
    func peersLoop(loadFirst: Bool) async {
        if loadFirst { await refreshPeers() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(Self.peersSeconds))
            if Task.isCancelled { break }
            await refreshPeers()
        }
    }

    func refreshLive() async {
        do {
            status = try await engine.call("chat_status", EngineEmpty(), as: ChatStatusDTO.self, timeout: .seconds(15))
            liveError = nil
        } catch is CancellationError {
            return
        } catch {
            liveError = Self.describe(error)
        }
        do {
            inbox = try await engine.call("chat_inbox", EngineEmpty(), as: ChatInboxDTO.self, timeout: .seconds(15))
        } catch is CancellationError {
            return
        } catch {
            liveError = Self.describe(error)
        }
    }

    func refreshPeers() async {
        guard !peersBusy else { return }
        peersBusy = true
        defer { peersBusy = false }
        do {
            peers = try await engine.call("chat_peers", EngineEmpty(), as: [ChatPeerDTO].self, timeout: .seconds(90))
            peersError = nil
        } catch is CancellationError {
            return
        } catch {
            peersError = Self.describe(error)
        }
    }

    // MARK: Actions

    /// Sends the draft to the chosen chat. Runs in its own task, so leaving the section does not
    /// cancel a send in flight.
    func send() {
        guard let chosen, canSend else { return }
        let text = draft
        let target = chosen.id
        sending = true
        sendError = nil
        Task {
            await self.deliver(target: target, text: text)
        }
    }

    private func deliver(target: String, text: String) async {
        defer { sending = false }
        do {
            let value = try await engine.call(
                "chat_send",
                ChatSendParams(target: target, text: text),
                as: ChatReceiptDTO.self,
                timeout: .seconds(120)
            )
            receipt = value
            // Keep the draft when the chat did not take it, so it can be sent again.
            if Self.tookIt.contains(value.state), draft == text { draft = "" }
        } catch {
            sendError = Self.describe(error)
        }
    }

    /// Links a computer. The engine checks the link over ssh before saving it, so this waits on the network.
    func link() {
        guard canLink else { return }
        let params = ChatLinkParams(
            device: linkDevice,
            sshHost: linkSsh,
            pulse: linkPulse.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? nil : linkPulse.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        linking = true
        linkError = nil
        linkNote = nil
        Task {
            await self.performLink(params)
        }
    }

    private func performLink(_ params: ChatLinkParams) async {
        defer { linking = false }
        do {
            let value = try await engine.call("chat_link", params, as: ChatLinkedDTO.self, timeout: .seconds(180))
            linkNote = "Linked \(value.device) (\(value.ssh)): \(ChatFormat.plural(value.chats, "chat")) there now."
            linkDevice = ""
            linkSsh = ""
            linkPulse = ""
            await refreshLive()
            await refreshPeers()
        } catch {
            linkError = Self.describe(error)
        }
    }

    /// Asks before unlinking `device`.
    func askUnlink(_ device: String) {
        unlinkTarget = device
        confirmingUnlink = true
    }

    /// Runs after the Unlink confirmation. The command answers with nothing, so its reply is ignored.
    func confirmUnlink() {
        guard let device = unlinkTarget else { return }
        linkError = nil
        linkNote = nil
        Task {
            await self.performUnlink(device)
        }
    }

    private func performUnlink(_ device: String) async {
        do {
            let body = try JSONEncoder().encode(ChatUnlinkParams(device: device))
            _ = try await engine.call("chat_unlink", params: body, timeout: .seconds(30))
            linkNote = "Unlinked \(device)."
            await refreshLive()
            await refreshPeers()
        } catch {
            linkError = Self.describe(error)
        }
    }

    private static func describe(_ error: any Error) -> String {
        (error as? EngineError)?.message ?? String(describing: error)
    }
}

/// Wording for the page: ages, plurals, receipts and the status line.
enum ChatFormat {
    /// What each receipt state means, in plain words. The receipt's own detail follows it.
    static func receiptWords(_ state: String) -> String {
        switch state {
        case "delivered": return "Delivered."
        case "sent": return "Sent, no verdict. The chat raised no objection, but it has not confirmed it read the message."
        case "queued": return "Queued. It shows at the chat's next turn; reading is not confirmed."
        case "held": return "Held. It will not be read until its owner continues or approves it."
        case "refused": return "Refused."
        case "unsupported": return "Unsupported. That chat cannot take messages this way."
        case "unknown": return "Unknown. It may or may not have arrived. Check the inbox, then send once more."
        default: return state
        }
    }

    /// "42s", "5min", "3h" or "2d" since `ms`.
    static func age(_ ms: Double) -> String {
        let secs = max(0, Int((Date().timeIntervalSince1970 * 1000 - ms) / 1000))
        if secs < 60 { return "\(secs)s" }
        if secs < 3600 { return "\(secs / 60)min" }
        if secs < 86400 { return "\(secs / 3600)h" }
        return "\(secs / 86400)d"
    }

    static func lastSeen(_ ms: Double?) -> String {
        guard let ms, ms > 0 else { return "not seen" }
        return "\(age(ms)) ago"
    }

    static func plural(_ count: Int, _ word: String) -> String {
        "\(count) \(word)\(count == 1 ? "" : "s")"
    }

    static func linkLine(_ link: ChatLinkDTO) -> String {
        if link.online {
            let ago = link.lastOkMs.flatMap { $0 > 0 ? age($0) : nil } ?? "just now"
            return "\(link.device): reached \(ago) ago"
        }
        let since = link.lastOkMs.flatMap { $0 > 0 ? ", last reached \(age($0)) ago" : nil } ?? ", never reached"
        return "\(link.device): offline\(since)"
    }

    static func statusLine(_ status: ChatStatusDTO?) -> String {
        guard let status else { return "Reading…" }
        if !status.enabled {
            return "Chat is off on this computer, so messages are refused. Turn it on in Settings, under General."
        }
        var line = "\(plural(status.chatsHere, "chat")) on \(status.device)."
        if !status.hubRunning {
            line += " Pulse's hub is not reporting as running, so messages to Claude chats may be held until it is."
        }
        return line
    }

    /// The meta line under a chat's name: "idle · live · 4min ago".
    static func meta(_ chat: ChatPeerDTO) -> String {
        var line = chat.state
        if chat.liveness != "unknown" { line += " · \(chat.liveness)" }
        return "\(line) · \(lastSeen(chat.updatedMs))"
    }
}
