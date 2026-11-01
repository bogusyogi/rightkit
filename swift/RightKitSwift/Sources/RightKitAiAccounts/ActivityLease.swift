import RightKitNotchCore
import RightKitNotch
import Foundation

/// Visibility lease for live session activity (#448). The session monitors poll every 2 s while a
/// session is busy and every 3 s otherwise; while someone is looking they poll every 1 s. Someone is
/// looking when the notch itself is expanded (`notchVisible`, set by the notch's view model) or when
/// another client (the hub's Activity page, the iOS live view) renewed a lease file in
/// `<state>/activity-lease/` within `ttl`. Expiry needs no message: the files age out.
/// The writer is engine/src/activity_lease.rs; keep `ttl` and the folder name in step.
/// The enum and `notchVisible` live in Sessions/ActivitySummary.swift (the shared face sets it).
extension ActivityLease {
    nonisolated static let ttl: TimeInterval = 10
    /// Test and injection hook: the folder to read; nil means the installed state folder.
    @MainActor static var folderOverride: URL?

    /// True while a visible client holds a lease. Reads one small folder per timer tick.
    @MainActor static func live(now: Date = Date()) -> Bool {
        if notchVisible { return true }
        if folderOverride == nil, Runtime.isUnderTest { return false }   // never the real folder under test
        let folder = folderOverride
            ?? StateRoot.installed("activity lease").appendingPathComponent("activity-lease", isDirectory: true)
        guard let names = try? FileManager.default.contentsOfDirectory(atPath: folder.path) else { return false }
        for name in names {
            let path = folder.appendingPathComponent(name).path
            if let modified = (try? FileManager.default.attributesOfItem(atPath: path))?[.modificationDate] as? Date,
               now.timeIntervalSince(modified) < ttl {
                return true
            }
        }
        return false
    }

    /// The timer period: 1 s while looked at, else `base`.
    @MainActor static func seconds(base: TimeInterval) -> TimeInterval {
        live() ? min(base, 1) : base
    }
}
