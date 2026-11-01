import Foundation

/// The limit alerts already raised, keyed by window and reset, so a restart does
/// not raise the same card again (#134). The app keeps it in UserDefaults; tests
/// pass nothing and keep it in memory.
final class UsageLimitLedger {
    private static let defaultsKey = "usageLimitRaised"
    /// Two reset times this close are the same reset: the CLI reads a reset to the
    /// minute, the endpoint to the second, and a poll that falls back between them
    /// must not announce the same limit twice.
    private static let sameResetSeconds: TimeInterval = 120

    private let defaults: UserDefaults?
    private var raised: [String: TimeInterval]

    init(defaults: UserDefaults? = nil) {
        self.defaults = defaults
        raised = (defaults?.dictionary(forKey: Self.defaultsKey) as? [String: TimeInterval]) ?? [:]
    }

    /// Whether `key` has already raised its card for the reset at `resetsAt`.
    func isRaised(_ key: String, resetsAt: Date?) -> Bool {
        guard let resetsAt, let stored = raised[key] else { return false }
        return abs(stored - resetsAt.timeIntervalSince1970) <= Self.sameResetSeconds
    }

    /// Records that `key` raised its card for the reset at `resetsAt`. A window with
    /// no reset time cannot be told apart from the next one, so it is not recorded.
    func record(_ key: String, resetsAt: Date?) {
        guard let resetsAt else { return }
        raised[key] = resetsAt.timeIntervalSince1970
        defaults?.set(raised, forKey: Self.defaultsKey)
    }
}

/// Watches store snapshots and detects when a provider's limit windows reach 100%
/// (exhausted) or the account is blocked.
///
/// Each window raises its own card, once per reset (#134): the session (the
/// headline) as a session limit, and any weekly window (the all-models one, or a
/// model's such as a team plan's Fable) as a weekly limit, named after its own
/// window. A window is judged by its own share; the account's pause counts only
/// for the session.
@MainActor
final class UsageLimitWatcher {
    private struct TrackedLimit {
        /// False until this window has been read once. The first reading of a
        /// window only records: a limit already spent when Codenotch starts is
        /// not news. Kept per window rather than per provider because the
        /// windows do not arrive together: the store's first publication can
        /// be a placeholder with no window at all, and the weekly window can
        /// appear a fetch after the session one. Counting the placeholder as
        /// the baseline is what announced "limit reached" at launch.
        var seeded = false
        var isExhausted: Bool = false
        var resetsAt: Date?
        var fraction: Double = 0
    }

    /// Per provider, per window id.
    private var states: [String: [String: TrackedLimit]] = [:]
    private let isMuted: (String) -> Bool
    private let deliver: (UsageAlertEvent) -> Void
    private let now: () -> Date
    private let ledger: UsageLimitLedger

    init(
        isMuted: @escaping (String) -> Bool = { _ in false },
        deliver: @escaping (UsageAlertEvent) -> Void = { _ in },
        now: @escaping () -> Date = Date.init,
        ledger: UsageLimitLedger = UsageLimitLedger()
    ) {
        self.isMuted = isMuted
        self.deliver = deliver
        self.now = now
        self.ledger = ledger
    }

    /// A later reset timestamp alone is not a new window: APIs which report a
    /// relative countdown move that timestamp by a few seconds on every
    /// refresh, and re-arming on that announced "limit reached" again on every
    /// fetch while the limit stayed spent. The tracked window must have
    /// actually elapsed, the same rule the reset watcher applies.
    private func rolledOver(from previous: Date?, to current: Date?) -> Bool {
        guard let previous, let current else { return false }
        return previous <= now() && current > previous
    }

    /// A window that runs about a week: the all-models weekly, a model's weekly, or
    /// Codex's secondary. Found by its length, never the session.
    private func isWeekly(_ window: LimitWindow) -> Bool {
        window.duration.map { WeeklyHeadline.week.contains($0) } ?? false
    }

    func observe(_ snapshots: [ProviderSnapshot]) {
        for snapshot in snapshots {
            observe(snapshot)
        }
    }

    private func observe(_ snapshot: ProviderSnapshot) {
        // Same rule as the reset watcher: an archived (stale) reading is not a
        // baseline, so the first live reading after one only records.
        guard !snapshot.status.isStale else {
            states.removeValue(forKey: snapshot.id)
            return
        }
        var windows = states[snapshot.id] ?? [:]
        let headlineID = snapshot.headline?.id

        for window in snapshot.windows where window.group == nil {
            guard let fraction = window.usedFraction else { continue }
            let isSession = window.id == headlineID
            guard isSession || isWeekly(window) else { continue }

            // The account's pause is the session's; a model's weekly is spent only
            // when its own share is.
            let isExhausted = fraction >= 1.0 || (isSession && snapshot.block != nil)
            var tracked = windows[window.id] ?? TrackedLimit()

            if rolledOver(from: tracked.resetsAt, to: window.resetsAt)
                || (window.resetsAt == nil && fraction < 0.95) {
                tracked.isExhausted = false
            }

            if !tracked.seeded {
                tracked.seeded = true
                tracked.isExhausted = isExhausted
            } else if isExhausted && !tracked.isExhausted && !isMuted(snapshot.id) {
                tracked.isExhausted = true
                let key = "\(snapshot.id)|\(window.id)"
                if !ledger.isRaised(key, resetsAt: window.resetsAt) {
                    ledger.record(key, resetsAt: window.resetsAt)
                    var event = UsageAlertEvent(
                        kind: isSession ? .sessionLimitReached : .weeklyLimitReached,
                        providerID: snapshot.id,
                        providerName: snapshot.displayName,
                        windowLabel: window.label,
                        glyph: snapshot.glyph,
                        previousFraction: tracked.fraction,
                        currentFraction: fraction,
                        resetsAt: window.resetsAt
                    )
                    event.noticeTitle = limitTitle(snapshot, window, isSession: isSession)
                    deliver(event)
                }
            }

            tracked.fraction = fraction
            tracked.resetsAt = window.resetsAt
            windows[window.id] = tracked
        }

        states[snapshot.id] = windows
    }

    /// The card's title, naming the window it is about: "Claude 5-hour limit
    /// reached", "Claude weekly limit reached", "Claude Fable (weekly) limit reached".
    private func limitTitle(_ snapshot: ProviderSnapshot, _ window: LimitWindow, isSession: Bool) -> String {
        let name: String
        if isSession {
            name = window.duration == 5 * 3600 ? L10n.t("5-hour") : L10n.t("session")
        } else if window.id == "weekly_all" || window.id == "secondary" {
            name = L10n.t("weekly")
        } else {
            name = window.label
        }
        return L10n.t("\(snapshot.displayName) \(name) limit reached")
    }
}
