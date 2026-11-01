import Foundation

/// The Mac side of the Windows "drive filling" and "drive full" alerts (`windows/src/alerts.rs`:
/// `DRIVE_WARN` 0.7 and `DRIVE_CRITICAL` 0.9 of used space, `DriveWatch::rise`). It reads the
/// volumes the Disks gauge already shows, so the same drives are watched on both machines.
/// Fullness is measured against physically free space; space macOS could reclaim is never
/// counted as free, but the alert names it in a second sentence when the gauge shows it, so a
/// truly full disk can be told from one macOS can clear.
///
/// Once per crossing: a level is announced when a volume rises to it, and a volume that stays
/// at or above 70% is not announced again. Falling below 70% re-arms both alerts, so a drive
/// hovering around 90% does not repeat on every sample. A volume merely filling at launch only
/// records; one already almost full alerts once, as on Windows.
///
/// Deliberately different from Windows in one place: Windows re-arms "almost full" on any dip
/// below 90%, so a drive oscillating across 90% alerts each time it climbs back. Here the
/// re-arm point is the lower 70% line, which is the behaviour this watcher was asked for.
///
/// Read-only volumes, network volumes and disk images never reach here: the Disks provider
/// drops the first two and `DiskImageVolumes` marks the last. The watcher is a difference
/// engine like the others, so it records whether or not the alert is switched on; the caller
/// gates delivery on `announceDriveAlerts`.
@MainActor
final class DriveCapacityWatcher {
    static let warningFraction = 0.7
    static let criticalFraction = 0.9

    struct Alert: Equatable {
        /// True at 90% or more ("almost full"); false at 70% ("filling up").
        let critical: Bool
        /// The volume's name, as the gauge labels it.
        let name: String
        /// "X free of Y", the gauge's own detail line, then (when the gauge shows it) a second
        /// sentence "N more reclaimable by macOS."
        let detail: String
        let usedPercent: Int
    }

    /// Highest level announced per volume window id: 0 none, 1 filling, 2 almost full.
    private var announced: [String: Int] = [:]
    /// False until the first live Disks reading; that reading only records a filling volume.
    private var seeded = false
    private let deliver: (Alert) -> Void

    init(deliver: @escaping (Alert) -> Void = { _ in }) {
        self.deliver = deliver
    }

    /// The drive's free-space line plus, when the Disks provider gave one, its reclaimable line.
    private static func detail(of window: LimitWindow, in windows: [LimitWindow]) -> String {
        let free = window.detail ?? ""
        let reclaimRow = DisksProvider.reclaimRowID(for: window.id)
        guard let reclaim = windows.first(where: { $0.id == reclaimRow })?.detail else { return free }
        return free + ". " + reclaim + "."
    }

    func observe(_ snapshots: [ProviderSnapshot]) {
        guard let disks = snapshots.first(where: { $0.id == SystemProviders.disksID }),
              !disks.status.isStale
        else { return }

        var present = Set<String>()
        var raised: [Alert] = []
        for window in disks.windows where window.id.hasPrefix("disk:") {
            guard let fraction = window.usedFraction,
                  !DiskImageVolumes.isImage(id: window.id)
            else { continue }
            present.insert(window.id)

            let level = fraction >= Self.criticalFraction ? 2
                : fraction >= Self.warningFraction ? 1
                : 0
            if level == 0 {
                announced[window.id] = 0
                continue
            }
            let before = announced[window.id] ?? (seeded ? 0 : 1)
            announced[window.id] = max(before, level)
            if level > before {
                raised.append(Alert(
                    critical: level == 2,
                    name: window.label,
                    detail: Self.detail(of: window, in: disks.windows),
                    usedPercent: Int((fraction * 100).rounded())))
            }
        }
        // A volume that was ejected or unmounted starts again from fine when it returns.
        announced = announced.filter { present.contains($0.key) }
        seeded = true

        for alert in raised {
            deliver(alert)
        }
    }
}
