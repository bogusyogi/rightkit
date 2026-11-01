import RightKitEngineClient
import Foundation
import RightKitHub

// Shared by the Overview and Monitor sections: engine DTOs for `status`, `metrics_history` and the
// notch state, the metrics history store, and the wording helpers. The names carry a `Mon` prefix so
// they cannot collide with another section's types in this module.

// MARK: - Engine calls

/// The page's JSON keys: params are camelCase, results are snake_case (`mount_point`, `start_time`).
enum MonJSON {
    static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }
}

enum MonEngine {
    /// One typed engine call with the page's params and a snake_case result.
    static func call<P: Encodable & Sendable, R: Decodable & Sendable>(
        _ engine: any EngineClient,
        _ method: String,
        _ params: P,
        as type: R.Type,
        timeout: Duration
    ) async throws -> R {
        let body = try JSONEncoder().encode(params)
        let data = try await engine.call(method, params: body, timeout: timeout)
        return try MonJSON.decode(type, from: data)
    }

    /// A call with no params (`{}`).
    static func call<R: Decodable & Sendable>(
        _ engine: any EngineClient,
        _ method: String,
        as type: R.Type,
        timeout: Duration
    ) async throws -> R {
        try await call(engine, method, EngineEmpty(), as: type, timeout: timeout)
    }

    /// The text to show for a failure: the engine's message, else the error's own description.
    static func text(_ error: any Error) -> String {
        if let engine = error as? EngineError { return engine.message }
        if let decoding = error as? DecodingError { return "Unexpected reply from the engine (\(decoding))" }
        return String(describing: error)
    }
}

/// Where the hub is running: the Windows hub says "PC" and a few strings differ.
enum MonPlatform {
    static var isWindows: Bool {
        #if os(Windows)
        return true
        #else
        return false
        #endif
    }

    /// The machine's name in sentences.
    static var machine: String { isWindows ? "PC" : "Mac" }
}

// MARK: - Levels

/// How busy something is: 0-70% ok, 70-90% warn, 90-100% bad.
enum MonLevel: Int, Comparable, Sendable {
    case ok = 0
    case warn
    case bad

    static func < (lhs: MonLevel, rhs: MonLevel) -> Bool { lhs.rawValue < rhs.rawValue }

    static func usage(_ fraction: Double) -> MonLevel {
        if fraction >= 0.9 { return .bad }
        if fraction >= 0.7 { return .warn }
        return .ok
    }

    static func pressure(_ text: String) -> MonLevel {
        let s = text.lowercased()
        if s.contains("critical") || s.contains("urgent") { return .bad }
        if s.contains("warn") || s.contains("elevated") { return .warn }
        return .ok
    }

    var tone: HubTone {
        switch self {
        case .ok: return .ok
        case .warn: return .warn
        case .bad: return .bad
        }
    }

    /// The word a screen reader and the notes use: "Fine", "Busy", "Critical".
    var word: String {
        switch self {
        case .ok: return "Fine"
        case .warn: return "Busy"
        case .bad: return "Critical"
        }
    }

    var symbol: String {
        switch self {
        case .ok: return "checkmark.circle"
        case .warn: return "exclamationmark.triangle"
        case .bad: return "xmark.circle"
        }
    }
}

// MARK: - Wording

/// The Tauri hub's number and date wording (`api.ts`), kept exact so both hubs read the same.
enum MonFormat {
    /// "17.5 GB", "980 MB", "5 B", and "—" for an unknown size. Decimal units, one decimal under 100.
    static func bytes(_ n: Double?) -> String {
        guard let n, n.isFinite else { return "—" }
        let units = ["B", "KB", "MB", "GB", "TB"]
        var value = n
        var index = 0
        while value >= 1000 && index < units.count - 1 {
            value /= 1000
            index += 1
        }
        let number = (value >= 100 || index == 0) ? String(Int(value.rounded())) : String(format: "%.1f", value)
        return "\(number) \(units[index])"
    }

    /// "17.5 GB" as ("17.5", "GB"), so the unit can be set small.
    static func splitBytes(_ n: Double?) -> (number: String, unit: String) {
        let text = bytes(n)
        guard let space = text.firstIndex(of: " ") else { return (text, "") }
        return (String(text[text.startIndex..<space]), String(text[text.index(after: space)...]))
    }

    static func clamp(_ fraction: Double) -> Double {
        fraction.isFinite ? min(max(fraction, 0), 1) : 0
    }

    /// "42%" from 0...1.
    static func percent(_ fraction: Double) -> String {
        "\(Int((clamp(fraction) * 100).rounded()))%"
    }

    /// "just now", "5 min ago", "2 h ago", or the date for anything older. `secs` is Unix seconds.
    static func ago(_ secs: Double, now: Date = Date()) -> String {
        let elapsed = max(0, now.timeIntervalSince1970 - secs)
        if elapsed < 90 { return "just now" }
        if elapsed < 3600 { return "\(Int((elapsed / 60).rounded())) min ago" }
        if elapsed < 86400 { return "\(Int((elapsed / 3600).rounded())) h ago" }
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return formatter.string(from: Date(timeIntervalSince1970: secs))
    }

    /// "136.3 GB more reclaimable by macOS" when it is at least 1 GB and 2% of the drive, else nil.
    static func reclaimableNote(total: Double?, reclaimable: Double?) -> String? {
        guard let reclaimable, let total, total > 0 else { return nil }
        return reclaimable >= 1e9 && reclaimable >= total * 0.02 ? "\(bytes(reclaimable)) more reclaimable by macOS" : nil
    }

    /// First letter up, the rest down: "elevated" becomes "Elevated".
    static func capitalized(_ text: String) -> String {
        guard let first = text.first else { return text }
        return String(first).uppercased() + text.dropFirst().lowercased()
    }
}

// MARK: - Status

/// One reading of `status`: a value, or nil when the platform does not report it.
struct MonMetric<Value: Decodable & Sendable>: Decodable, Sendable {
    let value: Value?
}

/// `status` (`SystemStatus`): live CPU, memory, swap and the platform's memory pressure.
struct MonStatus: Decodable, Sendable {
    let cpuUsagePercent: MonMetric<Double>?
    let memoryUsedBytes: MonMetric<Double>?
    let memoryTotalBytes: MonMetric<Double>?
    let memoryPressure: MonMetric<String>?
    let swapUsedBytes: MonMetric<Double>?
    let swapTotalBytes: MonMetric<Double>?

    var cpu: Double? { cpuUsagePercent?.value }
    var memUsed: Double? { memoryUsedBytes?.value }
    var memTotal: Double? { memoryTotalBytes?.value }
    /// macOS memory pressure (the kernel's level) or Windows commit pressure: "Normal", "Elevated", "Critical".
    var pressure: String? { memoryPressure?.value }
    var swapUsed: Double? { swapUsedBytes?.value }
    var swapTotal: Double? { swapTotalBytes?.value }
}

// MARK: - History

/// `metrics_history` params. A nil `sinceMs` is left out, which the engine reads as "everything".
struct MonSinceParams: Encodable, Sendable {
    var sinceMs: Int64?
}

/// One `metrics_history` sample (every 2 s, 30 minutes kept by the engine).
struct MonSample: Decodable, Sendable {
    /// Unix time, ms.
    let ts: Double
    /// Overall CPU, 0-100.
    let cpu: Double
    let memUsed: Double
    let memTotal: Double
    let swapUsed: Double
    let swapTotal: Double
    let pressure: String?
    /// Bytes per second; nil while unread.
    let netDown: Double?
    let netUp: Double?

    var time: Date { Date(timeIntervalSince1970: ts / 1000) }
    var memPercent: Double { memTotal > 0 ? memUsed / memTotal * 100 : 0 }
}

/// The last 30 minutes of samples, merged the way the page does: load once, then ask only for what
/// is newer than the newest sample held (`sinceMs`).
@MainActor
final class MonMetricsStore {
    static let keepMs: Double = 30 * 60 * 1000

    private(set) var samples: [MonSample] = []

    var latest: MonSample? { samples.last }

    /// Adds the samples newer than the newest held and drops anything past 30 minutes.
    func merge(_ fresh: [MonSample]) {
        let last = latest?.ts ?? 0
        let added = fresh.filter { $0.ts > last }
        if added.isEmpty { return }
        var all = samples + added
        let cutoff = (all.last?.ts ?? 0) - Self.keepMs
        all.removeAll { $0.ts < cutoff }
        samples = all
    }

    /// Reads what is new from the engine and merges it.
    func poll(_ engine: any EngineClient) async throws {
        let since = latest.map { Int64($0.ts) }
        let fresh = try await MonEngine.call(
            engine, "metrics_history", MonSinceParams(sinceMs: since), as: [MonSample].self, timeout: .seconds(8))
        merge(fresh)
    }

    /// Samples from the last `minutes`, measured back from the newest sample.
    func lastMinutes(_ minutes: Double) -> [MonSample] {
        guard let end = latest?.ts else { return [] }
        let cutoff = end - minutes * 60_000
        return samples.filter { $0.ts >= cutoff }
    }

    /// The part of the history one chart draws, or nil while fewer than two samples are held (the
    /// page's "Collecting readings…"). A history shorter than `minutes` starts at its first sample, so
    /// the samples fill the whole width.
    func window(minutes: Int) -> MonWindow? {
        guard samples.count >= 2, let end = latest?.ts, let first = samples.first?.ts else { return nil }
        let range = Double(minutes) * 60_000
        let short = first > end - range
        let start = short ? first : end - range
        return MonWindow(samples: samples.filter { $0.ts >= start }, start: start, end: end, short: short)
    }

    /// `minutes` of history as bars: one slot per `slotSeconds`, each the average of the samples in
    /// it, so 30 minutes at 10 s is 180 slots. A slot with no reading is a gap. The window ends at
    /// the newest sample, so a history shorter than the window leaves gaps at the left.
    func series(minutes: Int, slotSeconds: Int = 10, pick: (MonSample) -> Double?) -> HubSeries {
        let slots = max(1, minutes * 60 / slotSeconds)
        var series = HubSeries(capacity: slots)
        guard let end = latest?.ts else { return series }
        let slotMs = Double(slotSeconds) * 1000
        let start = end - Double(slots) * slotMs
        var sums = [Double](repeating: 0, count: slots)
        var counts = [Int](repeating: 0, count: slots)
        for sample in samples where sample.ts > start {
            let index = min(max(Int((sample.ts - start - 1) / slotMs), 0), slots - 1)
            if let value = pick(sample) {
                sums[index] += value
                counts[index] += 1
            }
        }
        for index in 0..<slots {
            let time = Date(timeIntervalSince1970: (start + Double(index + 1) * slotMs) / 1000)
            series.append(counts[index] == 0 ? nil : sums[index] / Double(counts[index]), at: time)
        }
        return series
    }
}

/// The samples one chart draws and the time span they cover (Unix ms).
struct MonWindow: Sendable {
    let samples: [MonSample]
    let start: Double
    let end: Double
    /// True when the history is shorter than the range asked for.
    let short: Bool

    /// The samples as `count` evenly spaced values across the span: each the average of the samples
    /// in its slot. A slot with no sample between two that have one repeats the earlier value (a
    /// line joining the points); a slot whose samples have no reading is a gap (nil).
    func bins(count: Int, pick: (MonSample) -> Double?) -> [Double?] {
        let slots = max(1, count)
        let span = max(end - start, 1)
        var sums = [Double](repeating: 0, count: slots)
        var counts = [Int](repeating: 0, count: slots)
        var seen = [Bool](repeating: false, count: slots)
        for sample in samples {
            let index = min(max(Int((sample.ts - start) / span * Double(slots)), 0), slots - 1)
            seen[index] = true
            if let value = pick(sample) {
                sums[index] += value
                counts[index] += 1
            }
        }
        guard let first = seen.firstIndex(of: true), let last = seen.lastIndex(of: true) else {
            return [Double?](repeating: nil, count: slots)
        }
        var out = [Double?](repeating: nil, count: slots)
        var carry: Double?
        for index in 0..<slots {
            if counts[index] > 0 {
                let value = sums[index] / Double(counts[index])
                out[index] = value
                carry = value
            } else if seen[index] {
                carry = nil
            } else if index > first && index < last {
                out[index] = carry
            }
        }
        return out
    }
}

// MARK: - Notch state

/// The notch's System readings beyond CPU and memory (`system` in its state).
struct MonSystemReadings: Decodable, Sendable {
    struct Network: Decodable, Sendable {
        let interface: String?
        let kind: String?
        let down: Double
        let up: Double
    }

    struct Battery: Decodable, Sendable {
        let percent: Double
        let charging: Bool?
        let cycles: Double?
        let health: Double?
        let celsius: Double?
        /// Signed watts: positive charging, negative discharging.
        let watts: Double?
        /// Minutes to empty, or to full when `toFull` is true.
        let minutes: Double?
        let toFull: Bool?
    }

    struct Fan: Decodable, Sendable {
        let name: String
        /// Set when the fan reports speed in revolutions per minute.
        let rpm: Double?
        /// Set when the fan reports speed as a percent of maximum.
        let percent: Double?
    }

    struct Temperature: Decodable, Sendable {
        let name: String
        let celsius: Double
    }

    let network: Network?
    let battery: Battery?
    let fans: [Fan]?
    let temperatures: [Temperature]?
}

struct MonLimit: Decodable, Sendable {
    let label: String
    /// 0-1, how much of the window is used.
    let usedFraction: Double
    /// Length of the window; absent when the provider did not say.
    let seconds: Double?
}

struct MonAccount: Decodable, Sendable, Identifiable {
    let id: String
    let name: String
    let connected: Bool?
    let limits: [MonLimit]?
}

struct MonPermission: Decodable, Sendable, Identifiable {
    let id: String
    let title: String?
    /// "granted", "needsApproval", "off" or "unknown".
    let status: String
    let required: Bool?
}

/// The parts of the notch's published state the Overview and Monitor read. Each part decodes on
/// its own, so a field this build does not know how to read leaves the others intact.
struct MonNotchState: Decodable, Sendable {
    let accounts: [MonAccount]
    let permissions: [MonPermission]
    let system: MonSystemReadings?

    private enum CodingKeys: String, CodingKey {
        case accounts, permissions, system
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        accounts = (try? container.decodeIfPresent([MonAccount].self, forKey: .accounts)) ?? []
        permissions = (try? container.decodeIfPresent([MonPermission].self, forKey: .permissions)) ?? []
        system = try? container.decodeIfPresent(MonSystemReadings.self, forKey: .system)
    }

    static func parse(_ data: Data?) -> MonNotchState? {
        guard let data else { return nil }
        return try? JSONDecoder().decode(MonNotchState.self, from: data)
    }
}
