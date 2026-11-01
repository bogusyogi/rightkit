#if os(macOS)
import RightKitNotchCore
import RightKitNotch
import AppKit
import Combine
import Foundation
import RightKitWatch

/// Watches one profile's `sessions` directory — `~/.claude/sessions` by
/// default — and publishes the Claude Code sessions that are actually running.
/// One monitor per `ClaudeProfile`; see `AppDelegate`.
///
/// The directory is watched rather than polled, because a session file appears
/// and disappears the moment a session starts and stops. The timer alongside it
/// covers the two things no file event will report: a process that died without
/// touching the directory, and — for sessions the Claude desktop app hosts —
/// what the session is *doing*, which is not in the directory at all and has to
/// be read from the transcript. See `ClaudeTranscript` for why.
@MainActor
final class ClaudeSessionMonitor: ObservableObject, AgentActivityMonitor {
    @Published private(set) var sessions: [AgentSession] = []
    var sessionsPublisher: AnyPublisher<[AgentSession], Never> { $sessions.eraseToAnyPublisher() }

    private let directory: URL
    private let livenessInterval: TimeInterval
    /// Nil leaves the monitor reading nothing but the registry, which is what
    /// the tests that only care about the registry want.
    private let transcripts: ClaudeTranscriptReader?

    /// Sessions to leave out because Codenotch started them, not the user.
    ///
    /// The `/usage` probe runs the Claude CLI, and the CLI registers a
    /// session file for the second or so it is alive, exactly like any other
    /// session. Without this the notch grows a seventh row that nobody asked
    /// for, and — worse — `isBusy` can read it as work in progress and start
    /// polling usage hard on the strength of it.
    ///
    /// The pid alone is enough: checked on this machine, the file the CLI
    /// writes carries the pid of the process Codenotch spawned, with no fork in
    /// between.
    var ignoredPIDs: () -> Set<Int32> = { [] }

    /// Working directories whose sessions are Codenotch's own, matched as a
    /// second net under the pids: the `/usage` probe runs in
    /// `ClaudeUsageCLI.scratchDirectory`, and a session filed from there is
    /// never the user's, whichever process wrote it.
    var ignoredWorkingDirectories: Set<String> = []

    /// Who owns a session the Claude desktop app hosts, when there is more than
    /// one Claude account on the machine. Nil leaves every record with the
    /// directory it was found in, which is what a single-profile machine and
    /// every test that predates this want. See `ClaudeSessionOwnership`.
    ///
    /// Only this profile's own directory is *watched*; a session adopted from
    /// another profile's directory arrives on the next liveness tick instead,
    /// two seconds later. Deliberate: a second descriptor per profile per
    /// profile is a lot of machinery to save a delay nobody can see, and the
    /// timer is already what covers everything a file event cannot report.
    var ownership: ClaudeSessionOwnership?

    private var watcher: RightKitWatcher?
    private var livenessTimer: Timer?
    private var timerSeconds: TimeInterval = 0
    private var debounce: DispatchWorkItem?
    private var wakeObserver: NSObjectProtocol?

    /// `livenessInterval` doubles as how often a running session's transcript
    /// is looked at, so it matches the Codex monitor's two seconds rather than
    /// the five it used when the registry was the only thing being read. The
    /// cost of a tick is a `stat` per live session; the tail is read only when
    /// the file has grown.
    init(
        directory: URL = URL(fileURLWithPath: Runtime.realHomePath("~/.claude/sessions"))
            .appendingPathComponent(".claude/sessions"),
        projects: URL? = nil,
        livenessInterval: TimeInterval = 2
    ) {
        self.directory = directory
        self.livenessInterval = livenessInterval
        self.transcripts = projects.map { ClaudeTranscriptReader(projects: $0) }
    }

    func start() {
        // Not idempotent by construction: a second descriptor and a second
        // timer would stack on the first pair, so stop whatever is running.
        // (`AntigravityActivityMonitor` does the same.)
        stop()
        rescan(remembering: true)
        watchDirectory()
        startLivenessTimer()
    }

    /// Rate: every `livenessInterval` (2 s) while a session is busy or waiting, 3 s while sessions are alive but idle. With none,
    /// no registry file and the directory watched, the timer is dropped: a new session
    /// writes its registry file, which the watcher turns into a read, and that
    /// read starts the timer again. It also stays on for profiles that adopt
    /// sessions from another directory (`ownership`), which is not watched.
    private func startLivenessTimer(active: Bool = false) {
        // Active (a session busy or waiting): `livenessInterval`, never faster. Otherwise 1.5x
        // that (3 s by default), since nothing live needs a fast look (#448).
        // While a visible client holds the activity lease (expanded notch, hub Activity page, iOS
        // live view), 1 s; it lapses 10 s after the last renew and the next tick restores 2 s/3 s.
        let seconds = ActivityLease.seconds(base: active ? livenessInterval : livenessInterval * 1.5)
        if livenessTimer != nil, timerSeconds == seconds { return }
        livenessTimer?.invalidate()
        timerSeconds = seconds
        let timer = Timer(timeInterval: seconds, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.rescan() }
        }
        RunLoop.main.add(timer, forMode: .common)
        livenessTimer = timer
    }

    func stop() {
        // A read still in flight finishes, but its result is dropped.
        generation += 1
        pending = false
        livenessTimer?.invalidate()
        livenessTimer = nil
        debounce?.cancel()
        watcher?.stop()
        watcher = nil
    }

    private func watchDirectory() {
        // No directory yet; the timer still covers us.
        guard FileManager.default.fileExists(atPath: directory.path) else { return }
        watcher = try? RightKitWatcher(RightKitWatchConfiguration(paths: [directory.path]), queue: .main) {
            [weak self] _ in
            MainActor.assumeIsolated { self?.scheduleRescan() }
        }
    }

    /// A single state change can produce several file events; coalesce them.
    private func scheduleRescan() {
        debounce?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.rescan(remembering: true) }
        }
        debounce = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: work)
    }

    /// Threading: the registry listing, the transcript tails and the liveness
    /// checks stall on a slow or swapping disk, so none of them runs on the main
    /// thread. They run on this one serial queue, shared by every profile's
    /// monitor, and the transcript readers and the desktop session index (both
    /// shared between monitors through `ownership`) are confined to it: only
    /// `read` touches them, one read at a time, so they need no lock. What
    /// crosses threads is value types, the ignore sets and a snapshot of
    /// `ownership`; `sessions` and the closures stay on the main actor.
    private static let readQueue = DispatchQueue(label: "dev.orthic.pulse.claude-read", qos: .utility)
    /// A read is in flight; a timer tick is skipped rather than queued behind it.
    private var reading = false
    /// A file event arrived during a read: exactly one more read follows it, so
    /// a burst never queues more than one and the final state is never dropped.
    private var pending = false
    /// Bumped by `start` and `stop`, so a result from before either is dropped.
    private var generation = 0

    /// `remembering` is for file events and `start`: if a read is running, one
    /// more runs after it. The timer leaves it off and simply skips the tick.
    private func rescan(remembering: Bool = false) {
        guard !reading else {
            if remembering { pending = true }
            return
        }
        reading = true
        let issued = generation
        let directory = directory, transcripts = transcripts
        // Evaluated here, on the main actor: both read state the main actor owns.
        let ignoring = ignoredPIDs()
        let ignoringDirectories = ignoredWorkingDirectories
        let ownership = ownership?.snapshot()
        Self.readQueue.async { [weak self] in
            let read = Self.read(directory: directory, transcripts: transcripts,
                                 ignoring: ignoring,
                                 ignoringDirectories: ignoringDirectories,
                                 ownership: ownership)
            // Any registry file at all (even one not alive yet, or half written)
            // keeps the liveness timer on: a file filling in raises no directory event.
            let registered = ((try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? [])
                .contains { $0.hasSuffix(".json") }
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.apply(read, issued: issued, registered: registered) }
            }
        }
    }

    private func apply(_ found: [AgentSession], issued: Int, registered: Bool = true) {
        reading = false
        defer {
            if pending {
                pending = false
                rescan()
            }
        }
        guard issued == generation else { return }
        if found.isEmpty, !registered, ownership == nil, watcher != nil {
            livenessTimer?.invalidate()
            livenessTimer = nil
        } else {
            startLivenessTimer(active: found.contains { $0.state == .busy || $0.state == .waiting })
        }
        guard found != sessions else { return }   // don't churn SwiftUI for nothing
        // Only on a change, so this is a handful of lines an hour rather than a
        // firehose. It is the one way to see what the notch thinks is running
        // without hovering over it:
        //
        //     log stream --predicate 'category == "sessions"' --level debug
        let summary = found.map { "\($0.name)=\($0.state)" }.joined(separator: " ")
        Log.sessions.debug("\(ClaudeProfile.tilde(self.directory.path), privacy: .public): \(summary, privacy: .public)")
        sessions = found
    }

    /// Every session this profile should draw.
    ///
    /// With no `ownership` this is exactly what it always was: the records in
    /// `directory`, nothing else. With one, the registry files of *every*
    /// profile are read and each desktop-hosted record goes to the profile
    /// whose account actually hosts it — which is not the profile whose
    /// directory it sits in, because the desktop app files them all under the
    /// default. A record whose account cannot be established is left exactly
    /// where it was found, so an unreadable index costs nothing but the
    /// old behaviour.
    nonisolated static func read(directory: URL,
                     transcripts: ClaudeTranscriptReader? = nil,
                     ignoring: Set<Int32> = [],
                     ignoringDirectories: Set<String> = [],
                     ownership: ClaudeSessionOwnership? = nil) -> [AgentSession] {
        let sources = ownership?.directories ?? [directory]

        var live: [ClaudeSessionRecord] = []
        /// Which directory each record came from: an adopted session's
        /// transcript stays where the session writes it, not where its ring is.
        var origin: [Int32: URL] = [:]

        for source in sources {
            for record in records(in: source, ignoring: ignoring,
                                  ignoringDirectories: ignoringDirectories) {
                if let ownership, !ownership.claims(record, foundIn: source) { continue }
                live.append(record)
                origin[record.pid] = source
            }
        }

        return deduplicated(live)
            .map { record in
                let reader = origin[record.pid].flatMap { ownership?.reader(for: $0) }
                    ?? transcripts
                return state(of: record, transcripts: reader)
            }
            // The id breaks ties so the order cannot flicker between two ticks
            // that read the same thing.
            .sorted { $0.since == $1.since ? $0.id < $1.id : $0.since > $1.since }
    }

    /// The live records in one profile's `sessions` directory.
    ///
    /// `ignoringDirectories` is the second net under the pids: a session filed
    /// from Codenotch's own `/usage` scratch directory is never the user's,
    /// whichever profile's directory it turns up in.
    nonisolated static func records(in directory: URL, ignoring: Set<Int32> = [],
                        ignoringDirectories: Set<String> = []) -> [ClaudeSessionRecord] {
        let ignoredDirectories = Set(ignoringDirectories.map {
            URL(fileURLWithPath: $0).standardizedFileURL.path
        })
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        return names
            .filter { $0.hasSuffix(".json") }
            .compactMap { name -> ClaudeSessionRecord? in
                let url = directory.appendingPathComponent(name)
                guard let data = try? Data(contentsOf: url),
                      let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let record = ClaudeSessionRecord(json: json),
                      !ignoring.contains(record.pid),
                      !ignoredDirectories.contains(URL(fileURLWithPath: record.cwd).standardizedFileURL.path),
                      ProcessLiveness.isAlive(pid: record.pid, startedAt: record.startedAt)
                else { return nil }
                return record
            }
    }

    /// The record's own answer where it has one, the transcript's where it does
    /// not. Desktop sessions always take the second path; terminal ones never
    /// do, which is what keeps their `waiting` state — the one thing only the
    /// terminal interface knows — exactly as it was.
    nonisolated static func state(of record: ClaudeSessionRecord,
                      transcripts: ClaudeTranscriptReader?) -> AgentSession {
        guard !record.reportsStatus,
              let sessionID = record.sessionID,
              let activity = transcripts?.activity(sessionID: sessionID, cwd: record.cwd)
        else { return record.session }
        return record.session(state: activity.turn == .inFlight ? .busy : .idle,
                              since: activity.since)
    }

    /// One session can hold two registry records at once: resuming after a
    /// crash registers a new pid while the old process is still winding down,
    /// and for a moment both files pass the liveness check. Claude Code settles
    /// this by keeping the most recently started record, so the notch does the
    /// same rather than drawing one session twice.
    nonisolated static func deduplicated(_ records: [ClaudeSessionRecord]) -> [ClaudeSessionRecord] {
        var newest: [String: ClaudeSessionRecord] = [:]
        var unidentified: [ClaudeSessionRecord] = []
        for record in records {
            guard let id = record.sessionID else { unidentified.append(record); continue }
            let candidate = record.startedAt ?? .distantPast
            if let held = newest[id], (held.startedAt ?? .distantPast) >= candidate { continue }
            newest[id] = record
        }
        return Array(newest.values) + unidentified
    }
}
#endif
