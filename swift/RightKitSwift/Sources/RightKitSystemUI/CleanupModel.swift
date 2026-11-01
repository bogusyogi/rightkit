#if os(macOS)
import RightKitEngineClient
import Foundation
import Observation
import RightKitHub

/// What a finished move or restore says, and how serious it is.
struct CleanupNote: Sendable, Equatable {
    enum Tone: Sendable, Equatable {
        case ok
        case warn
    }

    var tone: Tone
    var text: String
    var detail: String?
}

/// Which pane the section shows.
enum CleanupPane: String, Sendable, Hashable {
    case findings
    case history
}

/// The slice of the notch's state this section reads: Settings > General > Background >
/// "Scan when Storage or Cleanup opens".
private struct CleanupNotchState: Decodable {
    struct Settings: Decodable {
        let hubAutoScan: Bool?
    }

    let settings: Settings?
}

/// State of the Cleanup section: the scan's findings, the choice of what to move, the move and the
/// history with its restore. The scan, the move and the restore are single awaited engine calls, so
/// no engine event is needed here.
@MainActor
@Observable
final class CleanupModel {
    var report: CleanupAPI.Report?
    var history: [CleanupAPI.Activity] = []
    var historyWarnings: [String] = []
    /// Kept apart from `error`, so a rescan's clear does not hide a failed history load.
    var historyError: String?
    var selected: Set<String> = []
    /// The items a confirmation is waiting on.
    var pending: [CleanupAPI.Finding]?
    var confirmOpen = false
    var pane = CleanupPane.findings
    var busy = false
    var refreshing = false
    var error: String?
    var note: CleanupNote?
    var restoreIssue: [String: String] = [:]
    /// Ids of findings whose details are open, and the history entries whose paths are open.
    var expanded: Set<String> = []
    var heldOpen = false
    var aboutOpen = false

    private let client: CleanupClient
    private let notch: any NotchBridge
    @ObservationIgnored private var seeded = false
    /// One findings scan at a time; a request made while one runs asks for another after it.
    @ObservationIgnored private var scanning = false
    @ObservationIgnored private var again = false
    @ObservationIgnored private var started = false

    init(engine: any EngineClient, notch: any NotchBridge) {
        client = CleanupClient(engine: engine)
        self.notch = notch
    }

    // MARK: Wording

    static var trash: String { ACPlatform.trash }

    /// "≥ " for a lower bound, "up to " for an upper bound, then the size.
    static func total(_ findings: [CleanupAPI.Finding]) -> String {
        let sum = findings.reduce(Int64(0)) { $0 + $1.bytes }
        let prefix =
            findings.contains { $0.partial } ? "≥ " : (findings.contains { $0.isUpperBound } ? "up to " : "")
        return "\(prefix)\(StorageText.bytes(sum))"
    }

    /// Wording for a size the scan could not prove would be freed.
    static let estimate =
        "An estimate from allocated size on disk, not file size. Hard links and clones can make the real gain smaller."

    /// The drive's own free-space reading around a run: observed, and moved by anything else using the drive too.
    static func freeSpaceLine(_ volume: CleanupAPI.VolumeFree) -> String {
        let change = volume.afterAvailable - volume.beforeAvailable
        let amount: String
        if change == 0 {
            amount = "none"
        } else {
            amount = "\(change > 0 ? "+" : "-")\(StorageText.bytes(abs(change)))"
        }
        return "Free space on \(volume.volume) changed by \(amount) (observed)."
    }

    static var freeSpaceCaveat: String {
        "Items in the \(trash) stay on the drive, so space is not freed until the \(trash) is emptied. Other activity on the drive also moves this number."
    }

    /// The plain-words warning for one run, or nil when nothing needs saying.
    static func runWarning(_ activity: CleanupAPI.Activity) -> String? {
        var parts: [String] = []
        let unknown = activity.unknown ?? 0
        let skipped = activity.skipped ?? 0
        if unknown > 0 { parts.append("\(ACFormat.items(unknown)) could not be confirmed") }
        if skipped > 0 { parts.append("\(ACFormat.items(skipped)) \(skipped == 1 ? "was" : "were") skipped") }
        let detail = parts.joined(separator: " and ")
        if activity.recovered ?? false {
            let lead = "the app was interrupted during this cleanup"
            return detail.isEmpty ? "\(lead)." : "\(lead); \(detail)."
        }
        if detail.isEmpty { return nil }
        return "\(detail.prefix(1).uppercased())\(detail.dropFirst())."
    }

    // MARK: Derived

    var all: [CleanupAPI.Finding] { report?.findings ?? [] }
    var eligible: [CleanupAPI.Finding] { all.filter(\.movable) }
    var safe: [CleanupAPI.Finding] { eligible.filter { $0.risk == "safe" } }
    var review: [CleanupAPI.Finding] { eligible.filter { $0.risk == "review" } }
    var held: [CleanupAPI.Finding] { all.filter { !$0.movable } }
    /// What "Free this up" selects: the safe items and Chrome's signing copies (hub Cleanup.tsx `freeable`).
    var freeable: [CleanupAPI.Finding] { eligible.filter { $0.risk == "safe" || $0.ruleId == "chrome-signing-copies" } }
    /// Selects `freeable` for review; nothing moves until the move is confirmed (Cleanup.tsx `freeUp`).
    func freeUp() { selected = Set(freeable.map(\.id)) }
    var chosen: [CleanupAPI.Finding] { eligible.filter { selected.contains($0.id) } }

    var heading: String {
        guard report != nil else { return "Looking for what can go…" }
        let list = eligible
        return list.isEmpty ? "Nothing to clean up" : "\(Self.total(list)) can go"
    }

    var subheading: String? {
        guard let report else { return nil }
        let count = eligible.count
        var text = count > 0 ? "\(ACFormat.items(count)) · " : ""
        text += "Updated \(ACFormat.ago(report.scannedAt))"
        if refreshing { text += " · Refreshing…" }
        return text
    }

    // MARK: Load

    /// Saved findings show at once. A rescan follows only when there are none or they are old, and
    /// only when the setting allows it. `scan: false` loads only what is saved, for a picture.
    func load(scan: Bool = true) async {
        if started { return }
        started = true
        await loadHistory()
        let allowed = scan ? await autoScanAllowed() : false
        do {
            if let saved = try await client.cached() {
                show(saved)
                if allowed && Self.isStale(saved.scannedAt) { startRefresh() }
            } else if allowed {
                startRefresh()
            }
        } catch {
            if allowed { startRefresh() }
        }
    }

    /// "Scan when Storage or Cleanup opens": on unless the notch's state says it is off.
    private func autoScanAllowed() async -> Bool {
        guard let data = await notch.state(),
            let state = try? JSONDecoder().decode(CleanupNotchState.self, from: data)
        else { return true }
        return state.settings?.hubAutoScan != false
    }

    /// Saved results older than ten minutes refresh in the background when their view opens.
    private static func isStale(_ seconds: Int64) -> Bool {
        Date().timeIntervalSince1970 - Double(seconds) > 600
    }

    /// A failed history load stays on screen until a later load succeeds.
    func loadHistory() async {
        do {
            let next = try await client.history()
            history = next.actions
            historyWarnings = next.warnings
            historyError = nil
        } catch {
            historyError = ACFormat.describe(error)
        }
    }

    /// Preselected items are chosen once; later scans keep only what is still movable. A move
    /// waiting on confirmation keeps the items that are still offered.
    private func show(_ next: CleanupAPI.Report) {
        let first = !seeded
        seeded = true
        report = next
        let now = next.findings.filter(\.movable)
        if first {
            selected = Set(now.filter(\.preselected).map(\.id))
        } else {
            selected = Set(now.filter { selected.contains($0.id) }.map(\.id))
        }
        if let waiting = pending {
            let kept = now.filter { item in waiting.contains { $0.id == item.id } }
            pending = kept.isEmpty ? nil : kept
            if kept.isEmpty { confirmOpen = false }
        }
    }

    // MARK: Scan

    /// Starts a rescan behind the page, which stays usable while it runs: the saved findings can be
    /// selected and moved meanwhile, since every item is checked again when it moves.
    func startRefresh() {
        if scanning {
            again = true
            return
        }
        scanning = true
        refreshing = true
        error = nil
        Task { [weak self] in
            await self?.runScans()
        }
    }

    private func runScans() async {
        repeat {
            again = false
            do {
                show(try await client.scan())
            } catch {
                self.error = ACFormat.describe(error)
                again = false
            }
        } while again
        scanning = false
        refreshing = false
    }

    // MARK: Choosing

    func toggle(_ id: String) {
        if selected.contains(id) {
            selected.remove(id)
        } else {
            selected.insert(id)
        }
    }

    func toggleGroup(_ findings: [CleanupAPI.Finding], on: Bool) {
        for finding in findings {
            if on {
                selected.insert(finding.id)
            } else {
                selected.remove(finding.id)
            }
        }
    }

    func toggleExpanded(_ id: String) {
        if expanded.contains(id) {
            expanded.remove(id)
        } else {
            expanded.insert(id)
        }
    }

    /// Asks before moving what is chosen.
    func askToMove() {
        pending = chosen
        confirmOpen = true
    }

    var confirmTitle: String {
        "Move \(ACFormat.items(pending?.count ?? 0)) to the \(Self.trash)?"
    }

    var confirmMessage: String {
        let list = pending ?? []
        let estimate =
            list.contains { $0.isUpperBound || $0.partial }
            ? " (size is an estimate from allocated size, an upper bound: hard links and clones can make the real gain smaller)"
            : ""
        return
            "\(ACFormat.items(list.count)), \(Self.total(list)), will move to the \(Self.trash)\(estimate). Nothing is deleted, and nothing is freed until the \(Self.trash) is emptied: you can put them back from History here or from the \(Self.trash)."
    }

    // MARK: Move and restore

    private func run(_ work: () async throws -> Void) async {
        busy = true
        error = nil
        do {
            try await work()
        } catch {
            self.error = ACFormat.describe(error)
        }
        busy = false
    }

    /// Moves the picked findings to the Trash. The engine re-checks dev and ino for each.
    func move(_ picked: [CleanupAPI.Finding]) async {
        pending = nil
        confirmOpen = false
        await run {
            let sent = picked.filter(\.movable)
            let result = try await client.apply(sent)
            let warnings = result.warnings ?? []
            let skippedLines = result.skipped.map { "\(ACFormat.shortPath($0.path)): \($0.reason)" }
            let detail = (skippedLines + warnings).joined(separator: "\n")
            let moved = "\(StorageText.bytes(result.movedBytes)) (\(ACFormat.items(result.movedItems)))"
            let trash = Self.trash
            if !result.skipped.isEmpty {
                note = CleanupNote(
                    tone: .warn,
                    text: "Partly moved: \(moved) went to the \(trash), \(result.skipped.count) skipped.",
                    detail: detail
                )
            } else if !warnings.isEmpty {
                note = CleanupNote(
                    tone: .warn,
                    text: "Moved to the \(trash): \(moved), with a warning.",
                    detail: detail
                )
            } else {
                let free = (result.freeSpace ?? []).map(Self.freeSpaceLine).joined(separator: " ")
                note = CleanupNote(
                    tone: .ok,
                    text: "Moved to the \(trash): \(moved).",
                    detail: free.isEmpty ? nil : "\(free) \(Self.freeSpaceCaveat)"
                )
            }
            // What moved leaves the list at once; the rescan behind it confirms.
            let left = Set(result.skipped.map(\.path))
            let gone = Set(sent.filter { !left.contains($0.path) }.map(\.id))
            if let current = report {
                report = CleanupAPI.Report(
                    chromeSnapshots: current.chromeSnapshots,
                    findings: current.findings.filter { !gone.contains($0.id) },
                    safeBytes: current.safeBytes,
                    reviewBytes: current.reviewBytes,
                    scannedAt: current.scannedAt
                )
            }
            selected = selected.subtracting(gone)
            await loadHistory()
            startRefresh()
        }
    }

    /// Puts one run's items back. Items a crash left unknown are never offered and the engine refuses them.
    func restore(_ id: String) async {
        await run {
            restoreIssue[id] = nil
            let result = try await client.restore(id: id)
            let skippedLines = result.skipped.map { "\(ACFormat.shortPath($0.path)): \($0.reason)" }
            let detail = skippedLines.joined(separator: "\n")
            let warnings = result.warnings ?? []
            let restored = "\(StorageText.bytes(result.restoredBytes)) (\(ACFormat.items(result.restoredItems)))"
            let trash = Self.trash
            if !result.skipped.isEmpty { restoreIssue[id] = detail }
            if !result.skipped.isEmpty {
                note = CleanupNote(
                    tone: .warn,
                    text: "Partly restored: \(restored), \(result.skipped.count) stayed in the \(trash).",
                    detail: warnings.joined(separator: "\n")
                )
            } else if !warnings.isEmpty {
                note = CleanupNote(
                    tone: .warn,
                    text: "Restored: \(restored), with a warning.",
                    detail: warnings.joined(separator: "\n")
                )
            } else {
                note = CleanupNote(tone: .ok, text: "Restored: \(restored).", detail: nil)
            }
            await loadHistory()
            startRefresh()
        }
    }
}
#endif
