#if os(macOS)
import Foundation
import Observation
import RightKitHub

/// State of the Apps list: the inventory, update checks, icons, the filter and the open app.
///
/// The Tauri page reacts to engine events (`apps-row`, `apps-inventory`, `apps-icon`,
/// `apps-update-row`, `apps-updates-done`, `apps-update-job`); so does this model, through
/// `Events` (a bounded long-poll until RightKit publishes `events.subscribe`). The section's
/// `.task` runs `run()`. A `pulse.resync` event means events may have been missed: the saved
/// results are read again and a job that never reported is not claimed as done.
@MainActor
@Observable
final class AppsModel {
    static let day: Int64 = 86_400
    /// "Unused" means not opened for this many days.
    static let unusedDays: Int64 = 90

    let startup: StartupModel

    var apps: [AppsAPI.Entry] = []
    var loaded = false
    var refreshing = false
    var updates: [String: AppsAPI.Update] = [:]
    var updatesAt: Int64?
    var checking = false
    var icons: [String: AppIcon] = [:]
    var jobs: [String: AppsUpdateJob] = [:]
    var query = ""
    var filter: AppsFilter = .all
    /// The app whose detail is open, or nil for the list.
    var open: AppDetailModel?
    var error: String?
    var aboutOpen = false

    private let client: AppsClient

    /// Every topic this model handles.
    static let topics = [
        "apps-row", "apps-inventory", "apps-icon",
        "apps-update-row", "apps-updates-done", "apps-update-job",
    ]
    @ObservationIgnored private var details: [String: AppDetailModel] = [:]
    @ObservationIgnored private var iconsAsked: Set<String> = []
    @ObservationIgnored private var started = false
    /// When the running refresh and update check began (epoch seconds), to tell a saved result that
    /// is newer than they are when events were missed.
    @ObservationIgnored private var refreshBegan: Int64 = 0
    @ObservationIgnored private var checkBegan: Int64 = 0
    /// The version an app had when its Homebrew update started: the proof the update finished, if
    /// its event was missed.
    @ObservationIgnored private var versionBefore: [String: String] = [:]
    /// False for the off-screen renderer: it shows the saved data and starts no background job.
    @ObservationIgnored private var live = true

    init(engine: any EngineClient) {
        client = AppsClient(engine: engine)
        startup = StartupModel(client: AppsClient(engine: engine))
    }

    // MARK: Load

    /// Saved data first, then the background refreshes. `live: false` loads only what is saved
    /// (and the icons already cached), for a picture.
    func load(live: Bool = true) async {
        if started { return }
        started = true
        self.live = live
        if let saved = try? await client.cached(), !saved.apps.isEmpty {
            apps = saved.apps
        }
        loaded = true
        if live { startRefresh() }
        if let saved = try? await client.updatesCached() {
            updates = Self.byPath(saved.apps)
            updatesAt = saved.checkedAt
        }
        if live { startUpdateCheck(force: false) }
        await requestIcons(waiting: !live)
    }

    private static func byPath(_ rows: [AppsAPI.Update]) -> [String: AppsAPI.Update] {
        var out: [String: AppsAPI.Update] = [:]
        for row in rows { out[row.path] = row }
        return out
    }

    private static func now() -> Int64 { Int64(Date().timeIntervalSince1970) }

    // MARK: Inventory

    /// Starts a refresh behind the page.
    func startRefresh() {
        if refreshing { return }
        refreshing = true
        error = nil
        Task { [weak self] in
            await self?.runRefresh()
        }
    }

    private func runRefresh() async {
        refreshBegan = Self.now()
        do {
            try await client.refresh()
        } catch {
            refreshing = false
            self.error = ACFormat.describe(error)
        }
        // The engine runs the refresh behind the call: `apps-row` per app, then `apps-inventory`,
        // which ends `refreshing` (see `handle`).
    }

    // MARK: Updates

    /// Checks every app for an update behind the page. `force` ignores the engine's six-hour cache.
    func startUpdateCheck(force: Bool) {
        if checking { return }
        checking = true
        Task { [weak self] in
            await self?.runUpdateCheck(force: force)
        }
    }

    private func runUpdateCheck(force: Bool) async {
        checkBegan = Self.now()
        do {
            try await client.updatesRefresh(force: force)
        } catch {
            checking = false
            self.error = ACFormat.describe(error)
        }
        // `apps-update-row` per app, then `apps-updates-done`, which ends `checking` (see `handle`).
    }

    /// Starts the update of one app. Homebrew runs in the background; Sparkle and App Store apps open.
    func startUpdate(path: String) {
        jobs[path] = AppsUpdateJob(state: .running, message: "Starting…")
        versionBefore[path] = apps.first { $0.path == path }?.version ?? updates[path]?.installedVersion
        Task { [weak self] in
            await self?.runUpdate(path: path)
        }
    }

    private func runUpdate(path: String) async {
        let outcome: String
        do {
            outcome = try await client.update(path: path)
        } catch {
            jobs[path] = AppsUpdateJob(state: .failed, message: ACFormat.describe(error))
            versionBefore[path] = nil
            return
        }
        if outcome != "running" {
            versionBefore[path] = nil
            let message = outcome == "store" ? "Opened in the App Store." : "Opened. Use the app's own updater."
            jobs[path] = AppsUpdateJob(state: .done, message: message)
            return
        }
        // Homebrew runs behind the call and reports as `apps-update-job` (running, then done or
        // failed); `handle` sets the job. Nothing is set here: the event may already have come.
    }

    // MARK: Icons

    /// Asks once per app. Cached icons come back now; the engine renders the rest in the background
    /// and sends each as an `apps-icon` event.
    func requestIcons(waiting: Bool) async {
        let wanted = apps.map(\.path).filter { !iconsAsked.contains($0) }
        if wanted.isEmpty { return }
        iconsAsked.formUnion(wanted)
        if waiting {
            await fetchIcons(wanted)
        } else {
            Task { [weak self] in
                await self?.fetchIcons(wanted)
            }
        }
    }

    private func fetchIcons(_ paths: [String]) async {
        // The engine reads at most 500 paths per call.
        var start = 0
        while start < paths.count {
            let batch = Array(paths[start..<min(start + 200, paths.count)])
            start += 200
            guard let ready = try? await client.icons(paths: batch) else {
                // Asked again at the next inventory or resync.
                iconsAsked.subtract(batch)
                continue
            }
            for path in batch {
                if let url = ready[path], let icon = AppIcon.decode(dataURL: url) {
                    icons[path] = icon
                }
            }
        }
    }

    // MARK: Events

    /// Follows the engine's events until the section's task ends. The first `pulse.resync` says the
    /// stream is live: only then does the model read its state and start the background jobs, so no
    /// event of theirs is missed. If the stream is not live within a few seconds the model loads
    /// anyway, so a slow engine shows its error instead of an empty page.
    func run() async {
        let fallback = Task { [weak self] in
            try? await Task.sleep(for: .seconds(4))
            if Task.isCancelled { return }
            await self?.load()
        }
        defer { fallback.cancel() }
        for await event in Events.stream(engine: client.engine, topics: Self.topics) {
            if event.topic == Events.resyncTopic {
                await resync()
            } else {
                await handle(event)
            }
        }
    }

    private func resync() async {
        if started {
            await reread()
        } else {
            await load()
        }
    }

    /// Events may have been missed: read the saved results again, as the page's events would have
    /// delivered them, and settle what was waiting for one.
    private func reread() async {
        if let saved = try? await client.cached() {
            if !saved.apps.isEmpty { apps = saved.apps }
            if refreshing {
                // A refresh that did not save since it began is lost (the engine restarted) or
                // still running; asking again joins it when it is running.
                refreshing = false
                if !(saved.savedAt.map { $0 >= refreshBegan } ?? false) { startRefresh() }
            }
        }
        if let saved = try? await client.updatesCached() {
            updates = Self.byPath(saved.apps)
            updatesAt = saved.checkedAt
            if checking {
                checking = false
                if !(saved.checkedAt.map { $0 >= checkBegan } ?? false) { startUpdateCheck(force: false) }
            }
        }
        for (path, job) in jobs where job.state == .running {
            if let before = versionBefore[path],
                let current = try? await client.summary(path: path).version,
                current != before
            {
                finishJob(path, AppsUpdateJob(state: .done, message: "Updated with Homebrew."))
            } else {
                jobs[path] = AppsUpdateJob(
                    state: .unconfirmed,
                    message: "Lost track of the update. Check updates to see the result."
                )
            }
        }
        // Icons whose event was missed are asked for again; cached ones come back now.
        iconsAsked = iconsAsked.filter { icons[$0] != nil }
        await requestIcons(waiting: false)
    }

    private struct IconEvent: Decodable {
        let path: String
        let dataUrl: String?

        enum CodingKeys: String, CodingKey {
            case path
            case dataUrl = "data_url"
        }
    }

    private struct JobEvent: Decodable {
        let path: String
        let state: String
        let message: String
    }

    /// One engine event, handled as the page's listener for it is.
    private func handle(_ event: EngineEvent) async {
        let decoder = JSON.decoder
        switch event.topic {
        case "apps-row":
            guard let row = try? decoder.decode(AppsAPI.Entry.self, from: event.payload) else { return }
            if let index = apps.firstIndex(where: { $0.path == row.path }) {
                apps[index] = row
            } else {
                apps.append(row)
            }
        case "apps-inventory":
            guard let list = try? decoder.decode([AppsAPI.Entry].self, from: event.payload) else { return }
            apps = list
            refreshing = false
            loaded = true
            await requestIcons(waiting: false)
        case "apps-update-row":
            guard let row = try? decoder.decode(AppsAPI.Update.self, from: event.payload) else { return }
            updates[row.path] = row
        case "apps-updates-done":
            guard let report = try? decoder.decode(AppsAPI.UpdateReport.self, from: event.payload) else { return }
            updates = Self.byPath(report.apps)
            updatesAt = report.checkedAt
            checking = false
        case "apps-icon":
            // No data URL: the app has no usable icon and keeps the neutral one.
            guard let icon = try? decoder.decode(IconEvent.self, from: event.payload),
                let url = icon.dataUrl, let decoded = AppIcon.decode(dataURL: url)
            else { return }
            icons[icon.path] = decoded
        case "apps-update-job":
            guard let job = try? decoder.decode(JobEvent.self, from: event.payload) else { return }
            let state: AppsUpdateJob.State = job.state == "done" ? .done : job.state == "failed" ? .failed : .running
            if state == .done {
                finishJob(job.path, AppsUpdateJob(state: .done, message: job.message))
            } else {
                jobs[job.path] = AppsUpdateJob(state: state, message: job.message)
            }
        default:
            break
        }
    }

    /// Homebrew changed an app: check again so its row reflects the new version (the page checks
    /// updates; the list also reads the new version from a refresh).
    private func finishJob(_ path: String, _ job: AppsUpdateJob) {
        jobs[path] = job
        versionBefore[path] = nil
        startUpdateCheck(force: false)
        startRefresh()
    }

    // MARK: List

    /// The apps the filter and search show, largest first.
    var shown: [AppsAPI.Entry] {
        let now = Self.now()
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return apps
            .filter { app in
                let matches = needle.isEmpty
                    || app.name.lowercased().contains(needle)
                    || (app.bundleId ?? "").lowercased().contains(needle)
                // Unknown last-used is never treated as unused.
                let unused = filter != .unused
                    || (app.lastUsed.map { now - $0 >= Self.unusedDays * Self.day } ?? false)
                let withUpdate = filter != .updates || updates[app.path]?.state == "available"
                return matches && unused && withUpdate
            }
            .sorted { ($1.sizeBytes, $0.path) < ($0.sizeBytes, $1.path) }
    }

    /// How many apps have an update waiting.
    var updateCount: Int {
        apps.filter { updates[$0.path]?.state == "available" }.count
    }

    var isFiltered: Bool {
        !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || filter != .all
    }

    func clearFilters() {
        query = ""
        filter = .all
    }

    // MARK: Detail

    /// Opens an app's detail. Its picks survive Back and reopening, so the model is kept per app.
    func openDetail(_ app: AppsAPI.Entry) {
        let model = details[app.path] ?? AppDetailModel(app: app, client: client)
        details[app.path] = model
        model.prepareToOpen()
        open = model
        Task { [weak model] in
            await model?.load()
        }
    }

    /// Opens the app at `path` straight to its detail (the page's `--app <path>` start).
    func openDetail(path: String) async {
        do {
            let entry = try await client.summary(path: path)
            openDetail(entry)
        } catch {
            self.error = ACFormat.describe(error)
        }
    }

    func closeDetail() {
        open = nil
        if live { startRefresh() }
    }

    func reveal(_ path: String) async {
        do {
            try await client.reveal(path: path)
        } catch {
            self.error = ACFormat.describe(error)
        }
    }

    // MARK: Wording

    /// "Today", "Yesterday", "12 days ago", "4 months ago", "Unknown".
    static func lastUsedText(_ epoch: Int64?) -> String {
        guard let epoch else { return "Unknown" }
        let days = Int((now() - epoch) / day)
        if days <= 0 { return "Today" }
        if days == 1 { return "Yesterday" }
        if days < 60 { return "\(days) days ago" }
        return "\(days / 30) months ago"
    }
}
#endif
