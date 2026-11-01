#if os(macOS)
import RightKitEngineClient
import Foundation
import Observation
import RightKitHub

// Monitor's state: port of the data half of `hub/src/views/Monitor.tsx`. Status and the process list
// are read every 3 s, the history every 2 s, only while the page is open.

// MARK: - Engine results

/// A process identity: the pid plus its start time, so a reused pid is a different process.
struct MonIdentity: Decodable, Sendable {
    let pid: Int
    let startTime: Int
}

struct MonMember: Decodable, Sendable {
    let identity: MonIdentity
    let name: String
    let cpuUsagePercent: Double
    let memoryBytes: Double
    let threads: Double?
    /// Unix seconds; nil when unreadable.
    let startedAt: Double?
}

/// Bytes one group read and wrote since the previous refresh, and over its members' lifetimes (#324 item 10). Absent from older engines.
struct MonDiskIo: Decodable, Sendable {
    let readBytes: Double
    let writtenBytes: Double
    let totalReadBytes: Double
    let totalWrittenBytes: Double
}

/// One row of `process_rows`: an app or process group, led by one process.
struct MonProcess: Decodable, Sendable, Identifiable {
    let key: String
    let name: String
    let bundleId: String?
    let appPath: String?
    let lead: MonIdentity
    let cpuUsagePercent: Double
    let memoryBytes: Double
    /// Resident size (in RAM now) when it differs from `memoryBytes`; absent when unknown.
    let residentBytes: Double?
    /// Disk bytes since the previous refresh, summed over the group; nil from an older engine.
    let diskIo: MonDiskIo?
    /// Nil when any member's count is unreadable.
    let threads: Double?
    /// Unix seconds the lead started; nil when unreadable.
    let startedAt: Double?
    /// The app bundle or the executable; nil when unknown.
    let executable: String?
    let members: [MonMember]
    let canAct: Bool
    let refusal: String?

    var id: String { key }

    /// Identity of this process for its history: a reused pid starts with none.
    var historyKey: String { "\(lead.pid):\(lead.startTime)" }
}

struct MonPort: Decodable, Sendable {
    /// "tcp" or "udp".
    let proto: String
    let family: String
    let address: String
    let port: Int

    private enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case family, address, port
    }
}

/// One established connection of a process: the remote end it talks to, as an address (never looked up).
struct MonConnection: Decodable, Sendable {
    /// "tcp" or "udp".
    let proto: String
    let family: String
    let remoteAddress: String
    let remotePort: Int

    private enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case family, remoteAddress, remotePort
    }
}

/// `process_inspect`: a nil list could not be read, and a note says why.
struct MonInspection: Decodable, Sendable {
    let listening: [MonPort]?
    /// Absent in answers from older engines; nil when the connections could not be read.
    let connections: [MonConnection]?
    let openFiles: [String]?
    let notes: [String]?
}

struct MonProcessParams: Encodable, Sendable {
    let key: String
    let pid: Int
    let startTime: Int
}

struct MonInspectParams: Encodable, Sendable {
    let pid: Int
    let startTime: Int
}

struct MonRevealParams: Encodable, Sendable {
    let path: String
}

/// The Inspect panel: one process, read on request. `result` is nil until the first read ends.
struct MonInspectState: Sendable {
    let row: MonProcess
    var result: MonInspection?
    var error: String?
    var loading: Bool
}

/// One poll of one app: CPU percent, memory bytes, and disk bytes per second (nil when unknown).
/// `total` is the lifetime read plus written bytes the rate comes from; `members` names the processes
/// it covers, so a changed member set drops the rate; `at` is when the poll was read.
struct MonProcSample: Sendable {
    let cpu: Double
    let memory: Double
    let disk: Double?
    let total: Double?
    let members: String
    let at: Date
}

// MARK: - Sorting

enum MonSortKey: String, CaseIterable, Identifiable, Sendable {
    case memory, cpu, name, threads, started

    var id: String { rawValue }

    var label: String {
        switch self {
        case .memory: return "Memory"
        case .cpu: return "CPU"
        case .name: return "Name"
        case .threads: return "Threads"
        case .started: return "Started"
        }
    }

    var defaultDir: MonSortDir { self == .name ? .asc : .desc }

    /// Words for the caption, e.g. "sorted by cpu, highest first".
    var words: String {
        switch self {
        case .memory: return "memory"
        case .cpu: return "cpu"
        case .name: return "name"
        case .threads: return "threads"
        case .started: return "start time"
        }
    }
}

enum MonSortDir: Sendable {
    case asc, desc

    var flipped: MonSortDir { self == .asc ? .desc : .asc }
}

enum MonRules {
    /// Protected processes (macOS, and Windows' kernel-level ones that cannot be ended). They never
    /// get Quit or Force Quit from here.
    static let protectedNames: Set<String> = [
        "kernel_task", "windowserver", "launchd", "loginwindow",
        "secure system", "system", "system idle process", "system interrupts", "registry", "memory compression",
    ]

    /// Pulse itself. Quitting it from its own window would close the hub.
    static let pulseNames: Set<String> = ["pulse", "pulse-hub", "pulse hub"]

    /// System and critical rows: refused by the engine, not quittable, or protected by name.
    static func isSystem(_ p: MonProcess) -> Bool {
        let name = p.name.lowercased()
        return !p.canAct || p.refusal != nil || protectedNames.contains(name) || pulseNames.contains(name)
    }

    static func matches(_ p: MonProcess, query: String) -> Bool {
        if p.name.lowercased().contains(query) { return true }
        if (p.bundleId ?? "").lowercased().contains(query) { return true }
        return p.members.contains { $0.name.lowercased().contains(query) }
    }

    /// "3 h", "12 min", "2 d": the age of a start time; "unknown" when unreadable.
    static func age(startedAt: Double?, now: Double) -> String {
        guard let startedAt else { return "unknown" }
        let seconds = max(0, now - startedAt)
        if seconds < 60 { return "just now" }
        if seconds < 3600 { return "\(Int(seconds / 60)) min" }
        if seconds < 86400 { return "\(Int(seconds / 3600)) h" }
        return "\(Int(seconds / 86400)) d"
    }

    static func threadsText(_ threads: Double?) -> String {
        threads.map { String(Int($0)) } ?? "unknown"
    }

    /// A long path with its middle cut, so both ends stay readable.
    static func middleTruncate(_ text: String, max: Int = 72) -> String {
        if text.count <= max { return text }
        let keep = max - 1
        let head = (keep + 1) / 2
        let tail = keep / 2
        return String(text.prefix(head)) + "…" + String(text.suffix(tail))
    }

    static func batteryText(_ b: MonSystemReadings.Battery) -> String {
        var parts = ["\(Int(b.percent.rounded()))%"]
        if b.charging == true { parts.append("charging") }
        if let cycles = b.cycles { parts.append("\(Int(cycles)) cycles") }
        if let health = b.health { parts.append("\(Int((health * 100).rounded()))% health") }
        if let watts = b.watts { parts.append("\(watts < 0 ? "-" : "+")\(String(format: "%.1f", abs(watts))) W") }
        if let celsius = b.celsius { parts.append("\(String(format: "%.1f", celsius)) °C") }
        if let minutes = b.minutes {
            let total = Int(minutes)
            let span = total >= 60 ? "\(total / 60) h \(total % 60) min" : "\(total) min"
            parts.append("\(span) to \(b.toFull == true ? "full" : "empty")")
        }
        return parts.joined(separator: " · ")
    }
}

// MARK: - Model

@MainActor
@Observable
final class MonitorModel {
    private let engine: any EngineClient
    private let notch: any NotchBridge
    private let history = MonMetricsStore()

    private(set) var status: MonStatus?
    private(set) var procs: [MonProcess] = []
    /// The newest history sample. Reading it in a view makes the view follow new samples.
    private(set) var latest: MonSample?
    private(set) var notchState: MonNotchState?
    private(set) var notchRead = false
    /// The last read failure, cleared by the next success.
    private(set) var loadError: String?

    var query = ""
    var sort: MonSortKey = .memory
    var dir: MonSortDir = .desc
    var minutes = 15
    var sensorsOpen = false
    /// Show every process row; the list starts with the first `rowLimit`.
    var showAll = false
    /// The key of the group whose members are shown.
    var expanded: String?
    private(set) var notes: [String: String] = [:]
    private(set) var stuck: Set<String> = []
    private(set) var busy: String?
    private(set) var inspect: MonInspectState?
    /// The Force Quit dialog. `pendingForce` outlives the dialog's own dismissal so the confirm
    /// action still knows the row.
    var confirmForce = false
    private(set) var pendingForce: MonProcess?
    private(set) var procHistory: [String: [MonProcSample]] = [:]

    /// Samples kept per process: 120 polls, about six minutes at the 3 s poll.
    static let historySamples = 120
    /// Processes tracked at once; the oldest entries are dropped first.
    static let historyProcesses = 2000
    /// Rows drawn before "Show all": the list is a plain stack, so a very long list is cut.
    static let rowLimit = 150

    init(engine: any EngineClient, notch: any NotchBridge) {
        self.engine = engine
        self.notch = notch
    }

    // MARK: Loading

    func load() async {
        async let procsTask: Void = refreshProcesses()
        async let metricsTask: Void = refreshMetrics()
        async let notchTask: Void = readNotch()
        _ = await (procsTask, metricsTask, notchTask)
    }

    /// Status and the process list every 3 s while the page is open.
    func runProcesses(initialLoad: Bool) async {
        if initialLoad { await refreshProcesses() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3))
            if Task.isCancelled { break }
            await refreshProcesses()
        }
    }

    /// History every 2 s while the page is open.
    func runMetrics(initialLoad: Bool) async {
        if initialLoad { await refreshMetrics() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(2))
            if Task.isCancelled { break }
            await refreshMetrics()
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

    /// The Windows notch reads the GPU temperature only while someone looks at the readings, so
    /// this asks for it every 8 s while the page is open. Best effort: a refused command only
    /// means there is no GPU reading, and the page already says "Not available on this PC".
    func watchSensors() async {
        guard MonPlatform.isWindows else { return }
        let payload = Data(#"{"command":"watchSensors"}"#.utf8)
        while !Task.isCancelled {
            try? await notch.command(payload)
            try? await Task.sleep(for: .seconds(8))
        }
    }

    func readNotch() async {
        notchState = MonNotchState.parse(await notch.state())
        notchRead = true
    }

    /// One read of status and the process rows. A read that finds the previous one still running
    /// never happens: the polling loop awaits each read before the next sleep.
    func refreshProcesses() async {
        do {
            async let statusTask = MonEngine.call(engine, "status", as: MonStatus.self, timeout: .seconds(10))
            async let rowsTask = MonEngine.call(engine, "process_rows", as: [MonProcess].self, timeout: .seconds(20))
            let (newStatus, rows) = try await (statusTask, rowsTask)
            status = newStatus
            procs = rows
            record(rows, at: Date())
            loadError = nil
        } catch {
            loadError = MonEngine.text(error)
        }
    }

    func refreshMetrics() async {
        do {
            try await history.poll(engine)
            latest = history.latest
        } catch {
            if loadError == nil { loadError = MonEngine.text(error) }
        }
    }

    /// Adds one poll to each app's history, drops apps that are gone, and caps the map. The disk rate is
    /// the change in the app's lifetime total since its previous poll, over the seconds between this
    /// page's own polls. It is nil when there is no previous reading, the total went backwards (a pid
    /// reused or a member gone), the member set changed, the gap is over 10 s, or disk_io is absent.
    private func record(_ rows: [MonProcess], at now: Date) {
        var map = procHistory
        var live = Set<String>()
        for row in rows {
            let id = row.historyKey
            live.insert(id)
            var samples = map[id] ?? []
            let total = row.diskIo.map { $0.totalReadBytes + $0.totalWrittenBytes }
            let members = row.members
                .map { "\($0.identity.pid):\($0.identity.startTime)" }
                .joined(separator: ",")
            let prev = samples.last
            let seconds = prev.map { now.timeIntervalSince($0.at) } ?? 0
            var disk: Double?
            if let prev, let prevTotal = prev.total, let total, prev.members == members,
               total >= prevTotal, seconds > 0, seconds <= 10 {
                disk = (total - prevTotal) / seconds
            }
            samples.append(
                MonProcSample(
                    cpu: row.cpuUsagePercent, memory: row.memoryBytes, disk: disk, total: total,
                    members: members, at: now))
            if samples.count > Self.historySamples { samples.removeFirst(samples.count - Self.historySamples) }
            map[id] = samples
        }
        for id in Array(map.keys) where !live.contains(id) { map[id] = nil }
        for id in Array(map.keys) {
            if map.count <= Self.historyProcesses { break }
            map[id] = nil
        }
        procHistory = map
    }

    // MARK: Derived

    var pressureText: String? { (status?.pressure ?? latest?.pressure).map { MonFormat.capitalized($0) } }

    var notchDown: Bool { notchRead && notchState == nil }

    var system: MonSystemReadings? { notchState?.system }

    var sensorCount: Int {
        // Only fans with an rpm or percent reading are rendered, so only those are counted.
        (system?.fans?.filter { $0.rpm != nil || $0.percent != nil }.count ?? 0)
            + (system?.temperatures?.count ?? 0) + (system?.battery != nil ? 1 : 0)
    }

    /// Top 3 by memory are flagged regardless of the current sort.
    var topMemory: Set<String> {
        Set(procs.sorted { $0.memoryBytes > $1.memoryBytes }.prefix(3).map { $0.key })
    }

    var trimmedQuery: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The rows after search and sort. A stable sort: equal rows keep the engine's order.
    var visible: [MonProcess] {
        let q = trimmedQuery.lowercased()
        let rows = q.isEmpty ? procs : procs.filter { MonRules.matches($0, query: q) }
        let order = comparator(sort, dir)
        return rows.enumerated().sorted { a, b in
            let result = order(a.element, b.element)
            return result == 0 ? a.offset < b.offset : result < 0
        }.map { $0.element }
    }

    /// The process leading by memory (or CPU when sorted by CPU), for the notice line.
    var leader: MonProcess? {
        let key: MonSortKey = sort == .cpu ? .cpu : .memory
        let order = comparator(key, .desc)
        return procs.enumerated().min { a, b in
            let result = order(a.element, b.element)
            return result == 0 ? a.offset < b.offset : result < 0
        }?.element
    }

    var orderText: String {
        switch sort {
        case .name: return dir == .asc ? "A to Z" : "Z to A"
        case .started: return dir == .desc ? "newest first" : "oldest first"
        default: return dir == .desc ? "highest first" : "lowest first"
        }
    }

    /// Negative when `a` comes first. Unknown values sort last in both directions.
    private func comparator(_ key: MonSortKey, _ dir: MonSortDir) -> (MonProcess, MonProcess) -> Int {
        { a, b in
            let order: Int
            switch key {
            case .name:
                let x = a.name.lowercased()
                let y = b.name.lowercased()
                order = x == y ? 0 : (x < y ? -1 : 1)
            case .cpu:
                order = Self.compare(a.cpuUsagePercent, b.cpuUsagePercent)
            case .memory:
                order = Self.compare(a.memoryBytes, b.memoryBytes)
            case .threads, .started:
                let x = key == .threads ? a.threads : a.startedAt
                let y = key == .threads ? b.threads : b.startedAt
                if x == nil || y == nil { return x == nil && y == nil ? 0 : (x == nil ? 1 : -1) }
                order = Self.compare(x ?? 0, y ?? 0)
            }
            return dir == .asc ? order : -order
        }
    }

    private static func compare(_ x: Double, _ y: Double) -> Int { x == y ? 0 : (x < y ? -1 : 1) }

    // MARK: Actions

    /// A header click on the sorted column flips the direction; any other choice takes that
    /// column's default.
    func chooseSort(_ key: MonSortKey, fromHeader: Bool) {
        if fromHeader && key == sort {
            dir = dir.flipped
            return
        }
        sort = key
        dir = key.defaultDir
    }

    private func note(_ key: String, _ text: String) {
        notes[key] = text
    }

    func reveal(_ row: MonProcess) async {
        guard let path = row.executable else { return }
        do {
            _ = try await MonEngine.call(
                engine, "reveal", MonRevealParams(path: path), as: EngineEmpty?.self, timeout: .seconds(10))
        } catch {
            note(row.key, MonEngine.text(error))
        }
    }

    func quit(_ row: MonProcess) async {
        busy = row.key
        note(row.key, "Asking to quit…")
        do {
            let outcome = try await MonEngine.call(
                engine, "process_quit", MonProcessParams(key: row.key, pid: row.lead.pid, startTime: row.lead.startTime),
                as: String.self, timeout: .seconds(30))
            if outcome == "quit" {
                note(row.key, "Quit.")
            } else {
                note(row.key, "Still running. Force Quit is available.")
                stuck.insert(row.key)
            }
        } catch {
            note(row.key, MonEngine.text(error))
        }
        busy = nil
    }

    /// Asks to confirm a Force Quit.
    func askForce(_ row: MonProcess) {
        pendingForce = row
        confirmForce = true
    }

    /// Force Quit after the confirmation, for the row asked about.
    func forceConfirmed() async {
        guard let row = pendingForce else { return }
        pendingForce = nil
        busy = row.key
        do {
            let outcome = try await MonEngine.call(
                engine, "process_force_quit", MonProcessParams(key: row.key, pid: row.lead.pid, startTime: row.lead.startTime),
                as: String.self, timeout: .seconds(30))
            note(row.key, outcome == "quit" ? "Force quit." : "Still running.")
        } catch {
            note(row.key, MonEngine.text(error))
        }
        busy = nil
    }

    func toggleInspect(_ row: MonProcess) {
        if inspect?.row.key == row.key {
            inspect = nil
        } else {
            inspect = MonInspectState(row: row, result: nil, error: nil, loading: true)
            Task { await readInspect(row) }
        }
    }

    func refreshInspect() {
        guard let current = inspect else { return }
        inspect = MonInspectState(row: current.row, result: current.result, error: nil, loading: true)
        Task { await readInspect(current.row) }
    }

    func closeInspect() { inspect = nil }

    /// One read of one process. A result for a row that was closed meanwhile is dropped.
    private func readInspect(_ row: MonProcess) async {
        do {
            let result = try await MonEngine.call(
                engine, "process_inspect", MonInspectParams(pid: row.lead.pid, startTime: row.lead.startTime),
                as: MonInspection.self, timeout: .seconds(30))
            if let current = inspect, current.row.key == row.key {
                inspect = MonInspectState(row: current.row, result: result, error: nil, loading: false)
            }
        } catch {
            if let current = inspect, current.row.key == row.key {
                inspect = MonInspectState(row: current.row, result: current.result, error: MonEngine.text(error), loading: false)
            }
        }
    }

    // MARK: Series for the charts

    /// The samples the charts draw for the last `minutes`; nil while fewer than two are held. Reading
    /// `latest` makes the caller follow new samples.
    func chartWindow(minutes: Int) -> MonWindow? {
        _ = latest
        return history.window(minutes: minutes)
    }

    /// `minutes` of history as bars of 10 s each (180 slots for 30 minutes). Reading `latest` makes
    /// the caller follow new samples.
    func series(minutes: Int, pick: (MonSample) -> Double?) -> HubSeries {
        _ = latest
        return history.series(minutes: minutes, pick: pick)
    }
}
#endif
