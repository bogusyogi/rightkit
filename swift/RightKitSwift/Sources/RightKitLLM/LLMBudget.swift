import Foundation

// A durable spend cap for model calls: the Swift counterpart of the reserve / in-flight / commit life
// of `rightkit-spend`. Money is an integer count of micro-units of one currency (1_000_000 = one unit,
// for example one US dollar); no floating point. The client reserves the worst case of a call, writes the
// reservation to disk, and only then sends; a call that would pass the cap is refused before it leaves
// the device. Prices are the caller's data (``LLMPriceBook``): RightKit ships no price table, because
// providers change them.

/// What one model costs, in micro-units per million tokens.
public struct LLMPrice: Sendable, Equatable, Codable {
    public var inputMicroPerMillionTokens: Int64
    public var outputMicroPerMillionTokens: Int64

    public init(inputMicroPerMillionTokens: Int64, outputMicroPerMillionTokens: Int64) {
        self.inputMicroPerMillionTokens = inputMicroPerMillionTokens
        self.outputMicroPerMillionTokens = outputMicroPerMillionTokens
    }

    /// A route that costs nothing (a local model).
    public static let free = LLMPrice(inputMicroPerMillionTokens: 0, outputMicroPerMillionTokens: 0)

    /// The cost of `inputTokens` and `outputTokens`, rounded up to a whole micro-unit; nil on overflow.
    func cost(inputTokens: Int, outputTokens: Int) -> Int64? {
        guard let input = Self.part(Int64(max(0, inputTokens)), inputMicroPerMillionTokens),
              let output = Self.part(Int64(max(0, outputTokens)), outputMicroPerMillionTokens) else { return nil }
        let (sum, overflow) = input.addingReportingOverflow(output)
        return overflow ? nil : sum
    }

    private static func part(_ tokens: Int64, _ perMillion: Int64) -> Int64? {
        let (product, overflow) = tokens.multipliedReportingOverflow(by: max(0, perMillion))
        if overflow { return nil }
        let (rounded, overflow2) = product.addingReportingOverflow(999_999)
        return overflow2 ? nil : rounded / 1_000_000
    }
}

/// Prices by route. Keys are `provider:model` (the model compared without case) or `provider:*` for every
/// model of a provider. A provider that needs no key (a local model) is free when no entry names it.
public struct LLMPriceBook: Sendable, Equatable {
    public var entries: [String: LLMPrice]

    public init(_ entries: [String: LLMPrice]) {
        var normalised: [String: LLMPrice] = [:]
        for (key, value) in entries { normalised[key.lowercased()] = value }
        self.entries = normalised
    }

    public static func key(provider: String, model: String) -> String { "\(provider):\(model)" }

    /// The price of `route`, or nil when none is known (the call is then refused: its cost cannot be bounded).
    public func price(for route: LLMRoute) -> LLMPrice? {
        let provider = route.provider.id.lowercased()
        if let exact = entries["\(provider):\(route.model.lowercased())"] { return exact }
        if let any = entries["\(provider):*"] { return any }
        return route.provider.requiresCredential ? nil : .free
    }
}

public enum LLMBudgetPeriod: String, Sendable, Codable, Equatable {
    /// One cap for ever.
    case lifetime
    /// One cap per UTC calendar day.
    case day
    /// One cap per UTC calendar month.
    case month
}

public struct LLMBudgetPolicy: Sendable, Equatable {
    public var limitMicro: Int64
    public var period: LLMBudgetPeriod

    public init(limitMicro: Int64, period: LLMBudgetPeriod) {
        self.limitMicro = limitMicro
        self.period = period
    }
}

/// Where the budget lives. It must survive a relaunch.
public protocol LLMBudgetStore: Sendable {
    /// The stored bytes, or nil when nothing was stored yet. Throws when storage cannot be read.
    func read() throws -> Data?
    /// Replaces the stored bytes; the new bytes are all there or the old ones remain.
    func write(_ data: Data) throws
}

/// The budget in one file, written atomically. Give each app its own path (for example under its data
/// root); one ``LLMBudget`` per file per process.
public struct FileLLMBudgetStore: LLMBudgetStore {
    public let url: URL
    public init(url: URL) { self.url = url }

    public func read() throws -> Data? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        return try Data(contentsOf: url)
    }

    public func write(_ data: Data) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: .atomic)
    }
}

/// A reservation held against the cap until the call settles.
public struct LLMBudgetHold: Sendable, Equatable {
    public let id: String
    public let period: String
    public let estimateMicro: Int64
}

public struct LLMBudgetStatus: Sendable, Equatable {
    public var period: String
    public var limitMicro: Int64
    /// Settled spend in the current period.
    public var committedMicro: Int64
    /// Worst-case holds of calls in progress.
    public var reservedMicro: Int64
    public var remainingMicro: Int64
    /// Calls charged at their ceiling because the provider reported no usage (or the process ended mid-call).
    public var billedUnknownCount: Int
    /// Calls whose actual cost exceeded their reservation.
    public var overrunCount: Int
}

/// The durable cap. An actor: every reserve, settle and read is serialised.
///
/// Life of a call: ``reserve(estimateMicro:)`` (refused when `committed + open holds + estimate` passes the
/// limit; the hold is on disk before it returns), ``markInFlight(_:)`` (on disk before the request is sent),
/// then ``commit(_:actualMicro:unknown:)`` or ``release(_:)``. On the next start a hold that was in flight
/// when the process ended counts at its ceiling and is flagged (the request may have been sent); a hold
/// that was never in flight is dropped. A write failure at reserve or in-flight refuses the call; a write
/// failure at settle keeps the in-memory total (so this process stays bounded) and the next write persists it.
public actor LLMBudget {
    public nonisolated let policy: LLMBudgetPolicy
    private let store: any LLMBudgetStore
    private let now: @Sendable () -> Date
    private var state: BudgetState

    private struct OpenHold: Codable {
        var period: String
        var estimateMicro: Int64
        var inFlight: Bool
    }

    private struct BudgetState: Codable {
        var version = 1
        var committed: [String: Int64] = [:]
        var open: [String: OpenHold] = [:]
        var billedUnknown = 0
        var overruns = 0
    }

    public init(policy: LLMBudgetPolicy, store: any LLMBudgetStore, now: @escaping @Sendable () -> Date = { Date() }) throws {
        guard policy.limitMicro >= 0 else { throw LLMError.budgetUnavailable("limit is negative") }
        var loaded = BudgetState()
        do {
            if let data = try store.read() {
                loaded = try JSONDecoder().decode(BudgetState.self, from: data)
            }
        } catch { throw LLMError.budgetUnavailable("budget file is unreadable") }
        guard loaded.version == 1 else { throw LLMError.budgetUnavailable("budget file has version \(loaded.version)") }
        if !loaded.open.isEmpty {
            for (_, hold) in loaded.open where hold.inFlight {
                loaded.committed[hold.period, default: 0] = Self.saturatingAdd(loaded.committed[hold.period, default: 0], hold.estimateMicro)
                loaded.billedUnknown += 1
            }
            loaded.open = [:]
            do { try store.write(try JSONEncoder().encode(loaded)) } catch { throw LLMError.budgetUnavailable("budget file could not be written") }
        }
        self.policy = policy
        self.store = store
        self.now = now
        self.state = loaded
    }

    // MARK: Life of a call

    /// Holds `estimateMicro` (the worst case of one call) or throws ``LLMError/budgetExceeded(limitMicro:usedMicro:requestedMicro:)``.
    public func reserve(estimateMicro: Int64) throws -> LLMBudgetHold {
        guard estimateMicro >= 0 else { throw LLMError.invalidRequest("negative budget estimate") }
        let period = periodKey()
        let used = usedMicro(in: period)
        let (total, overflow) = used.addingReportingOverflow(estimateMicro)
        if overflow || total > policy.limitMicro {
            throw LLMError.budgetExceeded(limitMicro: policy.limitMicro, usedMicro: used, requestedMicro: estimateMicro)
        }
        let hold = LLMBudgetHold(id: UUID().uuidString, period: period, estimateMicro: estimateMicro)
        state.open[hold.id] = OpenHold(period: period, estimateMicro: estimateMicro, inFlight: false)
        do { try persist() } catch {
            state.open[hold.id] = nil
            throw LLMError.budgetUnavailable("budget file could not be written")
        }
        return hold
    }

    /// Marks the call as about to be sent. Throws, and the caller must not send, when this cannot be written.
    public func markInFlight(_ hold: LLMBudgetHold) throws {
        guard var open = state.open[hold.id] else { throw LLMError.budgetUnavailable("hold is not open") }
        open.inFlight = true
        state.open[hold.id] = open
        do { try persist() } catch {
            open.inFlight = false
            state.open[hold.id] = open
            throw LLMError.budgetUnavailable("budget file could not be written")
        }
    }

    /// Settles the call at `actualMicro`. `unknown` flags a charge taken at the ceiling because usage is unknown.
    public func commit(_ hold: LLMBudgetHold, actualMicro: Int64, unknown: Bool = false) {
        guard state.open.removeValue(forKey: hold.id) != nil else { return }
        let actual = max(0, actualMicro)
        state.committed[hold.period] = Self.saturatingAdd(state.committed[hold.period, default: 0], actual)
        if unknown { state.billedUnknown += 1 }
        if actual > hold.estimateMicro { state.overruns += 1 }
        try? persist()
    }

    /// Drops the hold: the call was refused or never sent, so nothing was spent.
    public func release(_ hold: LLMBudgetHold) {
        guard state.open.removeValue(forKey: hold.id) != nil else { return }
        try? persist()
    }

    public func status() -> LLMBudgetStatus {
        let period = periodKey()
        let reserved = state.open.values.reduce(Int64(0)) { Self.saturatingAdd($0, $1.estimateMicro) }
        let committed = state.committed[period, default: 0]
        let used = Self.saturatingAdd(committed, reserved)
        return LLMBudgetStatus(
            period: period, limitMicro: policy.limitMicro, committedMicro: committed, reservedMicro: reserved,
            remainingMicro: max(0, policy.limitMicro - used), billedUnknownCount: state.billedUnknown, overrunCount: state.overruns,
        )
    }

    // MARK: Internals

    /// Settled spend of `period` plus every open hold (a hold from an earlier period still counts until settled).
    private func usedMicro(in period: String) -> Int64 {
        let open = state.open.values.reduce(Int64(0)) { Self.saturatingAdd($0, $1.estimateMicro) }
        return Self.saturatingAdd(state.committed[period, default: 0], open)
    }

    private func periodKey() -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let parts = calendar.dateComponents([.year, .month, .day], from: now())
        let year = parts.year ?? 0, month = parts.month ?? 0, day = parts.day ?? 0
        switch policy.period {
        case .lifetime: return "all"
        case .month: return String(format: "%04d-%02d", year, month)
        case .day: return String(format: "%04d-%02d-%02d", year, month, day)
        }
    }

    private func persist() throws {
        try store.write(try JSONEncoder().encode(state))
    }

    private static func saturatingAdd(_ a: Int64, _ b: Int64) -> Int64 {
        let (sum, overflow) = a.addingReportingOverflow(b)
        return overflow ? Int64.max : sum
    }
}
