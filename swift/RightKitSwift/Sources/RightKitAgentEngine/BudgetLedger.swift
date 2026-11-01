import Foundation

/// The agent's durable spending cap (#408). The owner sets a daily and a monthly cap before the first call.
/// Before every call the worst-case cost is reserved and written to disk; the call is refused when that would
/// pass either cap. After the call the reservation becomes the real cost. A call whose outcome is unknown, and
/// any reservation still open when the app was killed, counts as fully spent.
///
/// Money is whole micro-dollars. A price of $P per million tokens is exactly P micro-dollars per token.
actor BudgetLedger {
    struct Pending: Codable, Sendable { var micro: Int; var day: String }

    struct State: Codable, Sendable {
        var version = 1
        var dailyCapCents: Int?
        var monthlyCapCents: Int?
        var spentMicro: [String: Int] = [:]
        var pending: [String: Pending] = [:]
    }

    /// What the settings screen shows.
    struct Snapshot: Sendable, Equatable {
        var dailyCapCents: Int?
        var monthlyCapCents: Int?
        var spentTodayMicro: Int
        var spentMonthMicro: Int
        var unreadable: Bool
    }

    static let microPerCent = 10_000

    private let url: URL
    private var state: State
    /// True when the file exists but could not be read: spending is unknown, so every call is refused.
    private var unreadable = false

    init(directory: URL) {
        url = directory.appendingPathComponent("agent-budget.json")
        var loaded = State()
        var broken = false
        if let data = try? Data(contentsOf: url) {
            if let decoded = try? JSONDecoder().decode(State.self, from: data) {
                loaded = decoded
            } else {
                broken = true
            }
        }
        // A reservation still open at launch means the app died mid-call: it counts as spent.
        for (_, pending) in loaded.pending { loaded.spentMicro[pending.day, default: 0] += pending.micro }
        let hadPending = !loaded.pending.isEmpty
        loaded.pending = [:]
        // Keep about two months of days; the monthly cap only needs the current month.
        if let cutoff = Calendar.current.date(byAdding: .day, value: -62, to: .now) {
            let oldest = Self.dayKey(cutoff)
            loaded.spentMicro = loaded.spentMicro.filter { $0.key >= oldest }
        }
        state = loaded
        unreadable = broken
        if hadPending && !broken { try? Self.write(loaded, to: url) }
    }

    static func dayKey(_ date: Date) -> String {
        let parts = Calendar.current.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    func snapshot(now: Date = .now) -> Snapshot {
        let day = Self.dayKey(now)
        return Snapshot(dailyCapCents: state.dailyCapCents, monthlyCapCents: state.monthlyCapCents,
                        spentTodayMicro: spent(day: day, month: String(day.prefix(7)), daily: true),
                        spentMonthMicro: spent(day: day, month: String(day.prefix(7)), daily: false),
                        unreadable: unreadable)
    }

    /// Sets both caps. The caller has already asked the owner (Face ID) when this raises a cap.
    func setCaps(dailyCents: Int, monthlyCents: Int) throws {
        guard dailyCents > 0, monthlyCents > 0, dailyCents <= monthlyCents else {
            throw AgentError.budget("Set a daily cap above zero and a monthly cap at least as large.")
        }
        guard !unreadable else { throw Self.unreadableError }
        var next = state
        next.dailyCapCents = dailyCents
        next.monthlyCapCents = monthlyCents
        try Self.write(next, to: url)
        state = next
    }

    /// Starts over after an unreadable file. The caller has asked the owner first.
    func resetUnreadable() throws {
        guard unreadable else { return }
        let fresh = State()
        try Self.write(fresh, to: url)
        state = fresh
        unreadable = false
    }

    /// Reserves `micro` before a call. Throws, and reserves nothing, when a cap is missing, the file cannot be
    /// written, or the call could pass either cap.
    func reserve(micro: Int, now: Date = .now) throws -> String {
        guard !unreadable else { throw Self.unreadableError }
        guard let daily = state.dailyCapCents, let monthly = state.monthlyCapCents else {
            throw AgentError.budget("Set a daily and a monthly spending cap before the agent makes a call.")
        }
        let day = Self.dayKey(now)
        let month = String(day.prefix(7))
        let dayAfter = spent(day: day, month: month, daily: true) + micro
        let monthAfter = spent(day: day, month: month, daily: false) + micro
        if dayAfter > daily * Self.microPerCent {
            throw AgentError.budget("This could pass today's cap of \(Self.dollars(daily * Self.microPerCent)). Nothing was sent. Raise the cap in Settings or try tomorrow.")
        }
        if monthAfter > monthly * Self.microPerCent {
            throw AgentError.budget("This could pass this month's cap of \(Self.dollars(monthly * Self.microPerCent)). Nothing was sent. Raise the cap in Settings or wait for next month.")
        }
        let id = UUID().uuidString
        var next = state
        next.pending[id] = Pending(micro: micro, day: day)
        // Written before the call: if this fails, the call is not made.
        do { try Self.write(next, to: url) } catch {
            throw AgentError.budget("The spending record could not be saved, so nothing was sent.")
        }
        state = next
        return id
    }

    /// The call finished: the reservation becomes the real cost.
    func settle(_ id: String, actualMicro: Int) {
        guard let pending = state.pending.removeValue(forKey: id) else { return }
        state.spentMicro[pending.day, default: 0] += max(actualMicro, 0)
        try? Self.write(state, to: url)
    }

    /// The call failed with the outcome unknown: the whole reservation counts as spent.
    func settleUnknown(_ id: String) {
        guard let pending = state.pending[id] else { return }
        settle(id, actualMicro: pending.micro)
    }

    /// The provider refused before doing any work: nothing was spent.
    func release(_ id: String) {
        guard state.pending.removeValue(forKey: id) != nil else { return }
        try? Self.write(state, to: url)
    }

    private func spent(day: String, month: String, daily: Bool) -> Int {
        var total = 0
        for (key, value) in state.spentMicro where daily ? key == day : key.hasPrefix(month) { total += value }
        for (_, pending) in state.pending where daily ? pending.day == day : pending.day.hasPrefix(month) {
            total += pending.micro
        }
        return total
    }

    private static let unreadableError = AgentError.budget("The spending record can't be read, so the agent is off to protect your cap. Reset it in Settings.")

    static func dollars(_ micro: Int) -> String {
        String(format: "$%.2f", Double(micro) / 1_000_000)
    }

    private static func write(_ state: State, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try JSONEncoder().encode(state)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
}
