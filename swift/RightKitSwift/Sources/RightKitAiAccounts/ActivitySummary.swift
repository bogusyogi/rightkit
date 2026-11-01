import RightKitNotchCore
import RightKitNotch
import SwiftUI

/// What the activity cell shows: the state of every live session, reduced to
/// the one thing worth knowing at a glance.
public struct ActivitySummary: Equatable {
    public enum State: Equatable {
        case working
        case waiting
        case success
        case idle
    }

    public let state: State
    public let sessions: [AgentSession]
    /// Requests lined up behind the one running — a local runtime's queue.
    /// Zero for every cloud agent, which has no such line to report.
    public let queued: Int
    /// What the tooltip's header says while this is going on, where a local
    /// runtime names the phase. Nil leaves the header to the session's name.
    public let note: String?

    /// Nil when nothing is running — the cell disappears rather than sitting
    /// there saying nothing.
    public init?(sessions: [AgentSession], queued: Int = 0, note: String? = nil) {
        guard !sessions.isEmpty else { return nil }
        self.sessions = sessions
        self.queued = max(0, queued)
        self.note = note
        // Anything blocked on you outranks anything merely busy: it is the only
        // state where the notch is asking for something.
        if sessions.contains(where: { $0.state == .waiting }) {
            state = .waiting
        } else if sessions.contains(where: { $0.state == .busy }) {
            state = .working
        } else if sessions.contains(where: { $0.state == .success }) {
            state = .success
        } else {
            state = .idle
        }
    }

    /// One short word, for the tooltip.
    public var label: String {
        switch state {
        case .working: return L10n.t("working")
        case .waiting: return L10n.t("waiting")
        case .success: return L10n.t("complete")
        case .idle:    return L10n.t("idle")
        }
    }

    /// White for working, deliberately: the indicator sits inside a ring whose
    /// colour already means "how much of your limit is gone", and a neutral
    /// tone cannot be misread as part of that scale. Waiting gets amber because
    /// it is the one state that wants something from you.
    public var color: Color {
        switch state {
        case .working: return Palette.textPrimary
        case .waiting: return Palette.watch
        case .success: return Palette.ample
        case .idle:    return Palette.ringTrack
        }
    }

}

/// The notch's expanded view is showing (set by `NotchViewModel`). The polling side of the lease
/// (`live`, `seconds`, the lease folder) is Mac-only and extends this enum in Sessions/ActivityLease.swift.
enum ActivityLease {
    /// Main-actor state, read on the main actor.
    @MainActor static var notchVisible = false
}
