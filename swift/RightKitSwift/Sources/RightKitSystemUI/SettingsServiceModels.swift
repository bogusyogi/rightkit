#if os(macOS)
import RightKitEngineClient
import Foundation
import Observation
import RightKitHub

// The engine-backed parts of the Settings pages: the sharing service (Nearby sharing, the agent
// chat, device trust, the Local network permission row) and the Permissions page's own probes
// (Full Disk Access, Windows permissions, stale grants). Each mirrors a type in api.ts or the
// React view that called the command, with every field the engine may omit optional.

// MARK: - Sharing (hub/src/views/NearbySettings.tsx, engine share_state)

struct SettingsShareDevice: Decodable, Sendable, Equatable {
    var fingerprint: String
    var alias: String?
    var deviceType: String?
    var ip: String?
    var state: String?
}

struct SettingsTrustDevice: Decodable, Sendable, Equatable, Identifiable {
    var fingerprint: String
    var label: String?
    var alias: String?
    var model: String?
    var kind: String?
    /// "allow", "ask" or "deny".
    var state: String?
    var present: Bool?
    var verified: Bool?
    /// "certificate", "callback" or nil.
    var proof: String?
    var firstSeenMs: Double?
    var lastSeenMs: Double?
    var refused: Int?

    var id: String { fingerprint }
}

struct SettingsTrust: Decodable, Sendable, Equatable {
    /// "ask" or "deny".
    var defaultState: String?
    var devices: [SettingsTrustDevice]?
}

struct SettingsChatLink: Decodable, Sendable, Equatable {
    var device: String
    var ssh: String?
    var chats: Int?
    var error: String?
    var lastOkMs: Double?
    var lastErrorMs: Double?
}

struct SettingsChatActivity: Decodable, Sendable, Equatable {
    var sentMs: Double?
    var receivedMs: Double?
    var lastOutcome: String?
}

struct SettingsChatRow: Decodable, Sendable, Equatable {
    var name: String
    /// "claude" or "codex".
    var kind: String
    var status: String?
    var device: String?
    var local: Bool?
    var cwd: String?
    var updatedMs: Double?
    var liveness: String?
    var unread: Int?
    var evicted: Int?
}

struct SettingsChat: Decodable, Sendable, Equatable {
    var enabled: Bool?
    var claudeHookDelivery: Bool?
    var terminalChats: Bool?
    var chatClaudeProfiles: Bool?
    var chatMuse: Bool?
    var stampUserWords: Bool?
    var codexWakeIdle: Bool?
    var claudeWakeIdle: Bool?
    var museWakeIdle: Bool?
    var chatReceiptNotices: Bool?
    var localChats: Int?
    var links: [SettingsChatLink]?
    var activity: SettingsChatActivity?
    var chats: [SettingsChatRow]?
    var lastError: String?
    var device: String?
}

struct SettingsShareState: Sendable, Equatable {
    var running: Bool
    var error: String?
    var alias: String?
    var saveDir: String?
    var devices: [SettingsShareDevice]
    var trust: SettingsTrust?
    var warnings: [String]
    /// "unknown", "granted" or "blocked".
    var localNetwork: String
    var chat: SettingsChat?
}

extension SettingsShareState: Decodable {
    private enum Keys: String, CodingKey {
        case running, error, alias, saveDir, devices, trust, warnings, localNetwork, chat
    }

    private static func field<T: Decodable>(_ container: KeyedDecodingContainer<Keys>, _ key: Keys) -> T? {
        (try? container.decodeIfPresent(T.self, forKey: key)) ?? nil
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Keys.self)
        running = Self.field(container, .running) ?? false
        error = Self.field(container, .error)
        alias = Self.field(container, .alias)
        saveDir = Self.field(container, .saveDir)
        devices = Self.field(container, .devices) ?? []
        trust = Self.field(container, .trust)
        warnings = Self.field(container, .warnings) ?? []
        localNetwork = Self.field(container, .localNetwork) ?? "unknown"
        chat = Self.field(container, .chat)
    }
}

struct SettingsChatChange: Decodable, Sendable, Equatable {
    var target: String
    var path: String?
    var action: String?
    var note: String?
}

struct SettingsChatReport: Decodable, Sendable, Equatable {
    var dryRun: Bool?
    var changes: [SettingsChatChange]
}

/// Reads and drives the sharing service. `share_state` is read on `share-devices` events and every 3 s while the page is open.
@MainActor
@Observable
final class SettingsShareModel {
    private let engine: any EngineClient

    /// The last answer; nil before the first answer or when the call failed.
    private(set) var share: SettingsShareState?
    /// Last failure of a trust or chat command, shown under the group.
    var message: String?
    private(set) var report: SettingsChatReport?
    private(set) var installing = false

    init(engine: any EngineClient) {
        self.engine = engine
    }

    func load() async {
        do {
            share = try await engine.call("share_state", EngineEmpty(), as: SettingsShareState.self, timeout: .seconds(10))
        } catch {
            share = nil
        }
    }

    /// Re-reads when the sharing service says its devices changed (`share-devices`, through
    /// `Events`, and once when the stream goes live or resyncs), and every 3 s while the page
    /// is open, as the page does: Chat's links and activity change without an event.
    func poll() async {
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await self.safetyRead() }
            group.addTask { await self.followEvents() }
        }
    }

    private func safetyRead() async {
        while !Task.isCancelled {
            await load()
            try? await Task.sleep(for: .seconds(3))
        }
    }

    private func followEvents() async {
        for await _ in Events.stream(engine: engine, topics: ["share-devices"]) {
            if Task.isCancelled { break }
            await load()
        }
    }

    /// A command that returns nothing. A failure shows as `message`.
    func run(_ method: String, _ params: some Encodable) async {
        message = nil
        do {
            let body = try JSON.encoder.encode(params)
            _ = try await engine.call(method, params: body, timeout: .seconds(15))
        } catch {
            message = SettingsFormat.describe(error)
        }
        await load()
    }

    func setChatEnabled(_ on: Bool) async { await run("chat_set_enabled", ["on": on]) }
    func setClaudeHookDelivery(_ on: Bool) async { await run("chat_set_claude_hook_delivery", ["on": on]) }
    func setTerminalChats(_ on: Bool) async { await run("chat_set_terminal_chats", ["on": on]) }
    func setClaudeProfiles(_ on: Bool) async { await run("chat_set_claude_profiles", ["on": on]) }
    func setMuseChats(_ on: Bool) async { await run("chat_set_muse_chats", ["on": on]) }
    func setStampUserWords(_ on: Bool) async { await run("chat_set_stamp_user_words", ["on": on]) }
    func setCodexWakeIdle(_ on: Bool) async { await run("chat_set_codex_wake_idle", ["on": on]) }
    func setClaudeWakeIdle(_ on: Bool) async { await run("chat_set_claude_wake_idle", ["on": on]) }
    func setMuseWakeIdle(_ on: Bool) async { await run("chat_set_muse_wake_idle", ["on": on]) }
    func setReceiptNotices(_ on: Bool) async { await run("chat_set_receipt_notices", ["on": on]) }
    func setTrust(_ fingerprint: String, _ state: String) async {
        await run("share_trust_set", ["fingerprint": fingerprint, "state": state])
    }
    func setTrustDefault(_ state: String) async { await run("share_trust_default", ["state": state]) }
    func setLabel(_ fingerprint: String, _ label: String) async {
        await run("share_trust_label", ["fingerprint": fingerprint, "label": label])
    }
    func forget(_ fingerprint: String) async { await run("share_trust_forget", ["fingerprint": fingerprint]) }

    func installSkill() async {
        installing = true
        message = nil
        do {
            report = try await engine.call("chat_install_skill", EngineEmpty(), as: SettingsChatReport.self, timeout: .seconds(60))
        } catch {
            report = nil
            message = SettingsFormat.describe(error)
        }
        installing = false
    }

    func openLocalNetworkSettings() async {
        do {
            let body = try JSON.encoder.encode(EngineEmpty())
            _ = try await engine.call("open_local_network_settings", params: body, timeout: .seconds(10))
        } catch {
            message = SettingsFormat.describe(error)
        }
    }
}

// MARK: - Permissions (engine permissions.rs)

struct SettingsStaleService: Decodable, Sendable, Equatable {
    var service: String
    var label: String
}

struct SettingsStaleApp: Decodable, Sendable, Equatable, Identifiable {
    var id: String
    /// "bundle" can be reset with tccutil; "path" must be removed in System Settings.
    var kind: String
    var services: [SettingsStaleService]
}

struct SettingsResetOutcome: Decodable, Sendable, Equatable {
    var id: String
    var service: String
    var ok: Bool
    var message: String
}

private struct SettingsResetItem: Encodable, Sendable {
    var id: String
    var services: [String]
}

private struct SettingsResetParams: Encodable, Sendable {
    var items: [SettingsResetItem]
}

private struct SettingsIDParams: Encodable, Sendable {
    var id: String
}

/// The Permissions page's own engine calls: the hub's Full Disk Access probe, the Windows
/// permission rows, and the clean-up of grants that belong to apps that no longer exist.
@MainActor
@Observable
final class SettingsPermissionsModel {
    private let engine: any EngineClient

    /// `fda_status`: nil until the first answer; "unknown" when the probe failed.
    private(set) var fdaStatus: String?
    /// `windows_permissions`: the rows Windows can answer; nil until the first answer.
    private(set) var windowsRows: [SettingsPermission]?
    private(set) var windowsError: String?
    /// The failure of the last "Open Settings" press.
    var openError: String?

    private(set) var stale: [SettingsStaleApp]?
    private(set) var picked: Set<String> = []
    private(set) var busy = false
    private(set) var results: [SettingsResetOutcome]?
    private(set) var staleError: String?

    init(engine: any EngineClient) {
        self.engine = engine
    }

    static func key(_ id: String, _ service: String) -> String { "\(id)\u{0}\(service)" }

    func loadFDA() async {
        do {
            fdaStatus = try await engine.call("fda_status", EngineEmpty(), as: String.self, timeout: .seconds(10))
        } catch {
            fdaStatus = "unknown"
        }
    }

    func loadWindows() async {
        do {
            windowsRows = try await engine.call("windows_permissions", EngineEmpty(), as: [SettingsPermission].self, timeout: .seconds(10))
            windowsError = nil
        } catch {
            windowsError = SettingsFormat.describe(error)
        }
    }

    /// Polled every 3 s while the Permissions page is open, as the page did.
    func poll(windows: Bool) async {
        while !Task.isCancelled {
            if windows { await loadWindows() } else { await loadFDA() }
            try? await Task.sleep(for: .seconds(3))
        }
    }

    func requestFDA() async {
        openError = nil
        do {
            let body = try JSON.encoder.encode(EngineEmpty())
            _ = try await engine.call("fda_request", params: body, timeout: .seconds(10))
        } catch {
            openError = SettingsFormat.describe(error)
        }
    }

    func openWindowsPermission(_ id: String) async {
        openError = nil
        do {
            let body = try JSON.encoder.encode(SettingsIDParams(id: id))
            _ = try await engine.call("windows_permission_open", params: body, timeout: .seconds(10))
        } catch {
            openError = SettingsFormat.describe(error)
        }
    }

    // MARK: Stale grants

    func scan() async {
        busy = true
        staleError = nil
        results = nil
        do {
            let found = try await engine.call("tcc_stale_scan", EngineEmpty(), as: [SettingsStaleApp].self, timeout: .seconds(60))
            stale = found
            var next: Set<String> = []
            for app in found where app.kind == "bundle" {
                for service in app.services { next.insert(Self.key(app.id, service.service)) }
            }
            picked = next
        } catch {
            staleError = SettingsFormat.describe(error)
        }
        busy = false
    }

    func toggle(_ key: String, on: Bool) {
        if on { picked.insert(key) } else { picked.remove(key) }
    }

    var bundles: [SettingsStaleApp] { (stale ?? []).filter { $0.kind == "bundle" } }
    var paths: [SettingsStaleApp] { (stale ?? []).filter { $0.kind == "path" } }

    /// Resets the picked grants with `tccutil` (never by writing the database).
    func clearPicked() async {
        guard let apps = stale else { return }
        var items: [SettingsResetItem] = []
        for app in apps where app.kind == "bundle" {
            let chosen = app.services.filter { picked.contains(Self.key(app.id, $0.service)) }.map { $0.service }
            if !chosen.isEmpty { items.append(SettingsResetItem(id: app.id, services: chosen)) }
        }
        busy = true
        do {
            results = try await engine.call("tcc_reset", SettingsResetParams(items: items), as: [SettingsResetOutcome].self, timeout: .seconds(120))
            stale = nil
        } catch {
            staleError = SettingsFormat.describe(error)
        }
        busy = false
    }
}
#endif
