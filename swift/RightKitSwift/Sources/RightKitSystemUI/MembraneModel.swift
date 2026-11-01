import Foundation
import Observation
import RightKitHub

// Membrane's state: port of the data half of `hub/src/views/Membrane.tsx`. Status and freshness
// refresh every 5 s while the section is open; logs, memory and context load only on request.
// Every call is a `membrane.*` engine command (engine/src/membrane_cmds.rs, engine/src/contract.rs).
// Only `membrane.restart` changes anything, and the page confirms it first.

// MARK: - Engine results

/// Any JSON value the engine reports whose shape Pulse does not assume (`model`, `watcherState`, ...).
enum MembraneJSON: Decodable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([MembraneJSON])
    case object([String: MembraneJSON])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let flag = try? container.decode(Bool.self) {
            self = .bool(flag)
        } else if let number = try? container.decode(Double.self) {
            self = .number(number)
        } else if let text = try? container.decode(String.self) {
            self = .string(text)
        } else if let items = try? container.decode([MembraneJSON].self) {
            self = .array(items)
        } else {
            self = .object(try container.decode([String: MembraneJSON].self))
        }
    }

    /// The value as compact JSON, keys sorted, for shapes the page only shows as text.
    var compact: String {
        switch self {
        case .null: return "null"
        case .bool(let flag): return flag ? "true" : "false"
        case .number(let number): return membraneNumber(number)
        case .string(let text): return "\"\(text)\""
        case .array(let items): return "[" + items.map(\.compact).joined(separator: ",") + "]"
        case .object(let dict):
            let pairs = dict.sorted { $0.key < $1.key }.map { "\"\($0.key)\":\($0.value.compact)" }
            return "{" + pairs.joined(separator: ",") + "}"
        }
    }
}

/// `membrane.status`. `model` is the resident's health JSON verbatim.
struct MembraneAvailability: Decodable, Sendable {
    /// "ok" usable or off by design; "temporary" clears without repair; "fault" needs a repair (#433).
    let state: String
    let reason: String
}

struct MembraneStatus: Decodable, Sendable {
    let state: String
    let running: Bool
    let pid: Double?
    let uptimeSecs: Double?
    let model: MembraneJSON?
    let availability: MembraneAvailability?
    let lastError: String?
    let health: String
    let healthError: String?
}

/// `membrane.freshness`. Unknown shapes are shown as text, never assumed.
struct MembraneProject: Decodable, Sendable {
    let root: MembraneJSON?
    let generationId: MembraneJSON?
    let complete: MembraneJSON?
    let eventSequence: MembraneJSON?
    let lastEvent: MembraneJSON?
}

struct MembraneFreshness: Decodable, Sendable {
    let projects: [MembraneProject]
    let watcherState: MembraneJSON?
    let coverage: MembraneJSON?
    let freshness: MembraneJSON?
    let reason: String?
}

/// `membrane.logs`.
struct MembraneLogs: Decodable, Sendable {
    let lines: [String]
    let count: Int
    let truncated: Bool
    let available: Bool
}

/// `membrane.memory`. `ts` is whatever unit the store reports; nil when it reports none.
struct MembraneNote: Decodable, Sendable {
    let id: MembraneJSON?
    let ts: Double?
    let title: String?
    let repository: String?
}

struct MembraneMemory: Decodable, Sendable {
    let available: Bool
    let reason: String?
    let count: Int
    let notes: [MembraneNote]
}

/// `membrane.context`: counts of delivery receipts, never the injected text.
struct MembraneContextLast: Decodable, Sendable {
    let event: MembraneJSON?
    let harness: MembraneJSON?
    let state: MembraneJSON?
    let ts: Double?
}

struct MembraneContext: Decodable, Sendable {
    let available: Bool
    let reason: String?
    let receipts: Int
    let byState: [String: Int]
    let byEvent: [String: Int]
    let sessions: Int
    let last: MembraneContextLast?
}

struct MembraneLogsParams: Encodable, Sendable {
    let lines: Int
}

struct MembraneMemoryParams: Encodable, Sendable {
    let limit: Int
}

// MARK: - Text

/// A number as the page shows it: whole numbers without a decimal point.
func membraneNumber(_ value: Double) -> String {
    if value == value.rounded(), abs(value) < 1e15 { return String(Int(value)) }
    return String(value)
}

/// The page's `text()` for an optional engine value: nil, null and "" read as None.
func membraneText(_ value: MembraneJSON?) -> String {
    guard let value else { return "None" }
    switch value {
    case .null: return "None"
    case .string(let text): return text.isEmpty ? "None" : text
    case .number, .bool: return value.compact
    case .array, .object:
        let json = value.compact
        return json.count > 240 ? String(json.prefix(240)) + "…" : json
    }
}

/// The same for a plain string field.
func membraneString(_ value: String?) -> String {
    guard let value, !value.isEmpty else { return "None" }
    return value
}

/// The same for an optional number field.
func membraneNumberText(_ value: Double?) -> String {
    guard let value else { return "None" }
    return membraneNumber(value)
}

/// The resident's model readiness: `model.state` with its `reason` when the health JSON gives them.
func membraneModelText(_ model: MembraneJSON?) -> String {
    if case .object(let dict)? = model, let state = dict["state"] {
        let reason = dict["reason"]
        if membraneTruthy(reason) {
            return "\(membraneText(state)): \(membraneText(reason))"
        }
        return membraneText(state)
    }
    return membraneText(model)
}

/// JavaScript truthiness for an engine value: null, false, 0 and "" are false.
func membraneTruthy(_ value: MembraneJSON?) -> Bool {
    switch value {
    case nil, .null?: return false
    case .bool(let flag)?: return flag
    case .number(let number)?: return number != 0
    case .string(let text)?: return !text.isEmpty
    default: return true
    }
}

/// "2h 17m", or "4m 9s" under an hour.
func membraneUptime(_ seconds: Double?) -> String {
    guard let seconds else { return "None" }
    let hours = Int(seconds / 3600)
    let minutes = Int(seconds.truncatingRemainder(dividingBy: 3600) / 60)
    if hours > 0 { return "\(hours)h \(minutes)m" }
    return "\(minutes)m \(Int(seconds.truncatingRemainder(dividingBy: 60)))s"
}

/// "a 3, b 1" for a count map, sorted by key; "None" when empty.
func membraneCounts(_ map: [String: Int]) -> String {
    let pairs = map.sorted { $0.key < $1.key }.map { "\($0.key) \($0.value)" }
    return pairs.isEmpty ? "None" : pairs.joined(separator: ", ")
}

// MARK: - Model

@MainActor
@Observable
final class MembraneModel {
    private let engine: any EngineClient

    private(set) var status: MembraneStatus?
    private(set) var statusError: String?
    private(set) var freshness: MembraneFreshness?
    private(set) var freshnessError: String?
    private(set) var logs: MembraneLogs?
    private(set) var logsError: String?
    private(set) var logsLoading = false
    private(set) var memory: MembraneMemory?
    private(set) var memoryError: String?
    private(set) var memoryLoading = false
    private(set) var context: MembraneContext?
    private(set) var contextError: String?
    private(set) var contextLoading = false

    /// The restart confirmation is open.
    var confirmRestart = false
    private(set) var restarting = false
    private(set) var restartError: String?

    /// Status and freshness refresh this often while the section is open.
    static let refreshSeconds = 5
    static let logLines = 200
    static let memoryNotes = 20

    init(engine: any EngineClient) {
        self.engine = engine
    }

    /// The first read, for a picture or a page that is already loaded.
    func load() async {
        await refresh()
    }

    /// Refreshes status and freshness until the page goes away. `initialLoad` reads at once; otherwise
    /// the first read waits one interval, since the caller already loaded the page.
    func run(initialLoad: Bool) async {
        if initialLoad { await refresh() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(Self.refreshSeconds))
            if Task.isCancelled { break }
            await refresh()
        }
    }

    func refresh() async {
        do {
            status = try await engine.call("membrane.status", EngineEmpty(), as: MembraneStatus.self, timeout: .seconds(10))
            statusError = nil
        } catch {
            statusError = ACFormat.describe(error)
        }
        do {
            freshness = try await engine.call("membrane.freshness", EngineEmpty(), as: MembraneFreshness.self, timeout: .seconds(10))
            freshnessError = nil
        } catch {
            freshnessError = ACFormat.describe(error)
        }
    }

    /// Restarts the resident (`membrane.restart` clears a latch and starts it again). The page asks first.
    func restart() async {
        restarting = true
        defer { restarting = false }
        do {
            _ = try await engine.call("membrane.restart", params: Data("{}".utf8), timeout: .seconds(30))
            restartError = nil
            confirmRestart = false
            await refresh()
        } catch {
            restartError = ACFormat.describe(error)
        }
    }

    func loadLogs() async {
        logsLoading = true
        defer { logsLoading = false }
        do {
            logs = try await engine.call("membrane.logs", MembraneLogsParams(lines: Self.logLines), as: MembraneLogs.self, timeout: .seconds(20))
            logsError = nil
        } catch {
            logsError = ACFormat.describe(error)
        }
    }

    func loadMemory() async {
        memoryLoading = true
        defer { memoryLoading = false }
        do {
            memory = try await engine.call("membrane.memory", MembraneMemoryParams(limit: Self.memoryNotes), as: MembraneMemory.self, timeout: .seconds(20))
            memoryError = nil
        } catch {
            memoryError = ACFormat.describe(error)
        }
    }

    func loadContext() async {
        contextLoading = true
        defer { contextLoading = false }
        do {
            context = try await engine.call("membrane.context", EngineEmpty(), as: MembraneContext.self, timeout: .seconds(20))
            contextError = nil
        } catch {
            contextError = ACFormat.describe(error)
        }
    }
}
