#if os(macOS)
import Foundation
import Observation
import RightKitHub

// Overview's state: port of the data half of `hub/src/views/Overview.tsx`. Every number is either
// live (status and history every 3 s) or read from what is already saved. Nothing here starts a scan.

// MARK: - Engine results

/// `volumes` (one row per drive). Sizes are nil for a network volume that did not answer.
struct OvVolume: Decodable, Sendable {
    let name: String
    let mountPoint: String
    let totalBytes: Double?
    /// Physically free: what the file system would let a write use now.
    let availableBytes: Double?
    /// macOS only: more space the system could reclaim; never added to the free figure.
    let reclaimableBytes: Double?
    let diskImage: Bool?
    /// "local" or "network".
    let kind: String?

    /// Share of the drive in use, 0...1.
    var used: Double {
        guard let totalBytes, let availableBytes, totalBytes > 0 else { return 0 }
        return 1 - availableBytes / totalBytes
    }

    /// A local drive with known sizes: network shares are listed in Storage only.
    var isLocalDrive: Bool {
        diskImage != true && kind != "network" && totalBytes != nil && availableBytes != nil
    }

    var reclaimableNote: String? {
        MonFormat.reclaimableNote(total: totalBytes, reclaimable: reclaimableBytes)
    }
}

/// The part of `cleanup_cached` the Overview reads. The command answers null when nothing is saved.
struct OvCleanup: Decodable, Sendable {
    let safeBytes: Double
    let reviewBytes: Double
    /// Unix seconds.
    let scannedAt: Double
}

/// The part of `apps_cached` the Overview reads.
struct OvCachedApps: Decodable, Sendable {
    struct App: Decodable, Sendable {
        let sizeBytes: Double?
    }

    /// Unix seconds when the saved list was written; nil when there is none.
    let savedAt: Double?
    let apps: [App]
}

/// The part of `apps_updates_cached` the Overview reads.
struct OvUpdateReport: Decodable, Sendable {
    struct App: Decodable, Sendable {
        let state: String
    }

    /// Unix seconds of the newest check; nil before any check.
    let checkedAt: Double?
    let apps: [App]
}

/// A saved result: still being read, read and empty (the command answered null), or read.
enum OvSaved<Value: Sendable>: Sendable {
    case reading
    case missing
    case value(Value)

    var isReading: Bool {
        if case .reading = self { return true }
        return false
    }

    var value: Value? {
        if case .value(let value) = self { return value }
        return nil
    }
}

// MARK: - Headline

struct OvAction: Sendable {
    let label: String
    let section: String
}

/// The one sentence for the whole page: the worst of memory, CPU, a full drive and swap.
struct OvHeadline: Sendable {
    let level: MonLevel
    let title: String
    let reason: String
    let actions: [OvAction]
}

struct OvLimitRow: Sendable {
    let label: String
    let fraction: Double
}

enum OvLogic {
    static let hour = 3600.0
    static let day = 86_400.0

    /// Picks the five-hour and weekly windows; falls back to the first two the provider sent.
    static func pickLimits(_ limits: [MonLimit]) -> [OvLimitRow] {
        let five = limits.first { limit in
            guard let seconds = limit.seconds else { return false }
            return abs(seconds - 5 * hour) < 60
        }
        let week = limits.first { limit in
            guard let seconds = limit.seconds else { return false }
            return seconds >= 6 * day && seconds <= 8 * day
        }
        var rows: [OvLimitRow] = []
        if let five { rows.append(OvLimitRow(label: "5-hour", fraction: five.usedFraction)) }
        if let week { rows.append(OvLimitRow(label: "Weekly", fraction: week.usedFraction)) }
        if !rows.isEmpty { return rows }
        return limits.prefix(2).map { OvLimitRow(label: $0.label, fraction: $0.usedFraction) }
    }
}

struct OvHeadlineInput {
    let hasStatus: Bool
    let cpu: Double?
    let memUsed: Double?
    let memTotal: Double?
    let memFraction: Double
    let memLevel: MonLevel
    let cpuLevel: MonLevel
    let fullest: OvVolume?
    let diskLevel: MonLevel
    let swapBusy: Bool
    let swapUsed: Double?
    let canGo: Double
    let cleanupScanned: Bool
    let available: Int
}

extension OvHeadline {
    /// Ties keep the order memory, CPU, drive, swap. When all are fine, the saved cleanup and
    /// updates decide the actions.
    static func choose(_ i: OvHeadlineInput) -> OvHeadline {
        let clear: [OvAction] = i.canGo > 0 ? [OvAction(label: "Clear \(MonFormat.bytes(i.canGo))", section: "cleanup")] : []
        let memText = i.memUsed == nil
            ? ""
            : "\(MonFormat.bytes(i.memUsed)) of \(MonFormat.bytes(i.memTotal)) memory in use (\(MonFormat.percent(i.memFraction)))"
        let cpuText = i.cpu.map { "CPU at \(Int($0.rounded()))%" } ?? ""
        let rest = "Open Monitor to see what is using it."
        let machine = MonPlatform.machine

        if !i.hasStatus {
            return OvHeadline(
                level: .ok, title: "Reading your \(machine)…", reason: "Live readings appear in a moment.", actions: [])
        }

        func twoActions(_ first: OvAction) -> [OvAction] { Array(([first] + clear).prefix(2)) }

        var worst: (level: MonLevel, make: () -> OvHeadline)?
        let issues: [(level: MonLevel, make: () -> OvHeadline)] = [
            (i.memLevel, {
                OvHeadline(
                    level: i.memLevel,
                    title: i.memLevel == .bad ? "Memory is critically high" : "Memory is getting busy",
                    reason: "\(memText). \(rest)",
                    actions: twoActions(OvAction(label: "Open Monitor", section: "monitor")))
            }),
            (i.cpuLevel, {
                OvHeadline(
                    level: i.cpuLevel,
                    title: i.cpuLevel == .bad ? "CPU is critically high" : "CPU is getting busy",
                    reason: "\(cpuText). \(rest)",
                    actions: twoActions(OvAction(label: "Open Monitor", section: "monitor")))
            }),
            (i.diskLevel, {
                let note = i.fullest?.reclaimableNote.map { " \($0)." } ?? ""
                return OvHeadline(
                    level: i.diskLevel,
                    title: i.diskLevel == .bad ? "A drive is almost full" : "A drive is filling up",
                    reason: "\(i.fullest?.name ?? "") has \(MonFormat.bytes(i.fullest?.availableBytes)) free of \(MonFormat.bytes(i.fullest?.totalBytes)).\(note)",
                    actions: twoActions(OvAction(label: "Open Storage", section: "storage")))
            }),
            (i.swapBusy ? MonLevel.warn : MonLevel.ok, {
                OvHeadline(
                    level: .warn,
                    title: "Swap is heavily used",
                    reason: "\(MonFormat.bytes(i.swapUsed)) of swap is in use, so memory has been tight. \(rest)",
                    actions: twoActions(OvAction(label: "Open Monitor", section: "monitor")))
            }),
        ]
        for issue in issues where issue.level != .ok {
            if worst == nil || issue.level > worst!.level { worst = issue }
        }
        if let worst { return worst.make() }

        let parts = [cpuText, memText].filter { !$0.isEmpty }.joined(separator: " and ")
        var extras: [String] = []
        if i.canGo > 0 { extras.append("\(MonFormat.bytes(i.canGo)) can be cleared") }
        if i.available > 0 { extras.append("\(i.available) app update\(i.available == 1 ? "" : "s") available") }
        var actions: [OvAction] = []
        if !i.cleanupScanned {
            actions.append(OvAction(label: "Scan for clutter", section: "cleanup"))
        } else if i.canGo > 0 {
            actions.append(OvAction(label: "Review \(MonFormat.bytes(i.canGo)) to clear", section: "cleanup"))
        }
        if i.available > 0 { actions.append(OvAction(label: "Update apps", section: "apps")) }
        let tail = extras.isEmpty ? "" : " \(extras.joined(separator: " · "))."
        return OvHeadline(
            level: .ok,
            title: "Your \(machine) is running well",
            reason: "\(parts): both comfortable. Storage is fine.\(tail)",
            actions: Array(actions.prefix(2)))
    }
}

// MARK: - Model

@MainActor
@Observable
final class OverviewModel {
    private let engine: any EngineClient
    private let notch: any NotchBridge
    private let history = MonMetricsStore()

    private(set) var status: MonStatus?
    /// Samples from the last 10 minutes, for the sparklines.
    private(set) var recent: [MonSample] = []
    /// nil while reading; empty when the read failed.
    private(set) var volumes: [OvVolume]?
    private(set) var cleanup: OvSaved<OvCleanup> = .reading
    private(set) var apps: OvSaved<OvCachedApps> = .reading
    private(set) var updates: OvSaved<OvUpdateReport> = .reading
    private(set) var notchState: MonNotchState?
    /// True once the notch state was read once; a nil `notchState` after that means the notch is not running.
    private(set) var notchRead = false
    /// The last live-reading failure, cleared by the next success.
    private(set) var liveError: String?

    init(engine: any EngineClient, notch: any NotchBridge) {
        self.engine = engine
        self.notch = notch
    }

    // MARK: Loading

    /// The first read of everything: live numbers, drives, saved results and the notch state.
    func load() async {
        async let live: Void = refreshLive()
        async let drives: Void = refreshVolumes()
        async let saved: Void = readSaved()
        async let notchTask: Void = readNotch()
        _ = await (live, drives, saved, notchTask)
    }

    /// Keeps the live numbers fresh while the page is open: status and history every 3 s, drive
    /// sizes every 30 s. `initialLoad` is false when `load()` already ran (the prepared view).
    func run(initialLoad: Bool) async {
        if initialLoad { await load() }
        var round = 0
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3))
            if Task.isCancelled { break }
            round += 1
            await refreshLive()
            if round % 10 == 0 { await refreshVolumes() }
        }
    }

    /// Follows the notch's published state while the page is open.
    func followNotch() async {
        await readNotch()
        for await data in notch.stateUpdates() {
            if Task.isCancelled { break }
            notchState = MonNotchState.parse(data)
            notchRead = true
        }
    }

    func readNotch() async {
        notchState = MonNotchState.parse(await notch.state())
        notchRead = true
    }

    func refreshLive() async {
        do {
            status = try await MonEngine.call(engine, "status", as: MonStatus.self, timeout: .seconds(10))
            liveError = nil
        } catch {
            liveError = MonEngine.text(error)
        }
        do {
            try await history.poll(engine)
            recent = history.lastMinutes(10)
        } catch {
            // The graphs keep what they have; the status read above reports a broken engine.
            if liveError == nil { liveError = MonEngine.text(error) }
        }
    }

    func refreshVolumes() async {
        do {
            volumes = try await MonEngine.call(engine, "volumes", as: [OvVolume].self, timeout: .seconds(20))
        } catch {
            volumes = []
        }
    }

    /// Saved results only: these commands never scan.
    private func readSaved() async {
        async let cleanupRead: Void = readCleanup()
        async let appsRead: Void = readApps()
        async let updatesRead: Void = readUpdates()
        _ = await (cleanupRead, appsRead, updatesRead)
    }

    private func readCleanup() async {
        do {
            let report = try await MonEngine.call(engine, "cleanup_cached", as: OvCleanup?.self, timeout: .seconds(10))
            cleanup = report.map { OvSaved<OvCleanup>.value($0) } ?? OvSaved<OvCleanup>.missing
        } catch {
            cleanup = .missing
        }
    }

    private func readApps() async {
        do {
            let cached = try await MonEngine.call(engine, "apps_cached", as: OvCachedApps.self, timeout: .seconds(10))
            apps = .value(cached)
        } catch {
            apps = .missing
        }
    }

    private func readUpdates() async {
        do {
            let report = try await MonEngine.call(engine, "apps_updates_cached", as: OvUpdateReport.self, timeout: .seconds(10))
            updates = .value(report)
        } catch {
            updates = .missing
        }
    }

    // MARK: Derived numbers

    var cpu: Double? { status?.cpu }
    var memUsed: Double? { status?.memUsed }
    var memTotal: Double? { status?.memTotal }
    var swapUsed: Double? { status?.swapUsed }
    var swapTotal: Double? { status?.swapTotal }
    var pressure: String? { status?.pressure }

    var memFraction: Double {
        guard let memUsed, let memTotal, memTotal > 0 else { return 0 }
        return memUsed / memTotal
    }

    /// Local drives with known sizes.
    var drives: [OvVolume] { (volumes ?? []).filter { $0.isLocalDrive } }

    var fullest: OvVolume? {
        var best: OvVolume?
        for drive in drives where drive.used > (best?.used ?? -1) { best = drive }
        return best
    }

    var memLevel: MonLevel {
        max(MonLevel.usage(memFraction), pressure.map { MonLevel.pressure($0) } ?? .ok)
    }

    var cpuLevel: MonLevel { MonLevel.usage((cpu ?? 0) / 100) }

    var diskLevel: MonLevel { fullest.map { MonLevel.usage($0.used) } ?? .ok }

    /// Swap has no fixed limit: it is busy once it passes half of the memory.
    var swapBusy: Bool {
        guard let swapUsed, let memTotal, memTotal > 0 else { return false }
        return swapUsed / memTotal >= 0.5
    }

    /// Bytes the saved cleanup scan says can go: the safe and the review totals.
    var canGo: Double {
        guard let report = cleanup.value else { return 0 }
        return report.safeBytes + report.reviewBytes
    }

    var installedCount: Int { apps.value?.apps.count ?? 0 }
    var installedBytes: Double { (apps.value?.apps ?? []).reduce(0) { $0 + ($1.sizeBytes ?? 0) } }

    /// How many installed apps have an update available.
    var updatesAvailable: Int { updates.value?.apps.filter { $0.state == "available" }.count ?? 0 }

    /// Permissions the notch requires that are not granted.
    var permissionsMissing: Int {
        (notchState?.permissions ?? []).filter { $0.required == true && $0.status != "granted" }.count
    }

    var notchDown: Bool { notchRead && notchState == nil }

    var connectedAccounts: [MonAccount] { (notchState?.accounts ?? []).filter { $0.connected == true } }

    /// Network now: the notch's reading, else the hub's own history (no interface name there).
    var network: (down: Double, up: Double, kind: String?)? {
        if let reading = notchState?.system?.network {
            return (reading.down, reading.up, reading.kind)
        }
        if let sample = recent.last(where: { $0.netDown != nil && $0.netUp != nil }),
           let down = sample.netDown, let up = sample.netUp {
            return (down, up, nil)
        }
        return nil
    }

    var headline: OvHeadline {
        OvHeadline.choose(
            OvHeadlineInput(
                hasStatus: status != nil,
                cpu: cpu,
                memUsed: memUsed,
                memTotal: memTotal,
                memFraction: memFraction,
                memLevel: memLevel,
                cpuLevel: cpuLevel,
                fullest: fullest,
                diskLevel: diskLevel,
                swapBusy: swapBusy,
                swapUsed: swapUsed,
                canGo: canGo,
                cleanupScanned: cleanup.value != nil,
                available: updatesAvailable))
    }

    // MARK: Series for the sparklines

    func series(_ pick: (MonSample) -> Double?) -> HubSeries {
        HubSeries(
            capacity: max(2, recent.count),
            samples: recent.map { HubSample(time: $0.time, value: pick($0)) })
    }
}
#endif
