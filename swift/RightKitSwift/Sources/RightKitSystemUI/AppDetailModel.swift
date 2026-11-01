import Foundation
import Observation
import RightKitHub

/// State of one app's detail: its leftovers, the choice of what to move, and the uninstall.
///
/// The Tauri page streams `app_leftovers` as `apps-leftovers` events. Those events are not available
/// yet (`events.subscribe`), so this reads `app_detail`, which answers with the same bundle row,
/// leftovers, background items and receipts in one reply.
@MainActor
@Observable
final class AppDetailModel {
    enum Status {
        case loading
        case done
        case failed
    }

    /// The groups the items are shown in, in this order.
    static let locations = [
        "Application",
        "User Library",
        "System Library",
        "Your app data",
        "Shared app data",
        "Installer receipt",
    ]

    /// How a match was made, in plain words.
    static let confidence: [String: String] = [
        "exact": "Exact id",
        "helper": "Helper id",
        "group": "App group",
        "prefix": "Id prefix",
        "team": "Team id",
        "name": "Name",
        "receipt": "Receipt",
        "install": "Registered app",
        "publisher": "Publisher and product",
    ]

    let app: AppsAPI.Entry

    var items: [AppsAPI.Related]
    var background: [AppsAPI.Background] = []
    var receipts: [String] = []
    var picked: Set<String>
    var status = Status.loading
    var leftoverError: String?
    var confirmOpen = false
    var busy = false
    var result: AppsAPI.UninstallResult?
    var error: String?
    var aboutOpen = false

    private let client: AppsClient
    /// Paths the user has changed: leftovers arriving later never overrule them.
    @ObservationIgnored private var touched: Set<String> = []

    init(app: AppsAPI.Entry, client: AppsClient) {
        self.app = app
        self.client = client
        items = [Self.bundleRow(app)]
        picked = app.protectedReason == nil ? [app.path] : []
    }

    /// The bundle row from the list's facts, shown before the real one arrives.
    static func bundleRow(_ app: AppsAPI.Entry) -> AppsAPI.Related {
        AppsAPI.Related(
            path: app.path,
            label: "Application",
            location: "Application",
            exact: true,
            confidence: "exact",
            reason: "The application bundle",
            admin: false,
            sizeBytes: app.sizeBytes,
            preselected: app.protectedReason == nil
        )
    }

    /// Clears what belongs to one visit (notices, an open dialog); the picks stay.
    func prepareToOpen() {
        result = nil
        error = nil
        leftoverError = nil
        confirmOpen = false
        busy = false
        status = .loading
    }

    func load() async {
        status = .loading
        leftoverError = nil
        do {
            let detail = try await client.detail(path: app.path)
            merge(detail)
            status = .done
        } catch {
            leftoverError = ACFormat.describe(error)
            status = .failed
        }
    }

    private func merge(_ detail: AppsAPI.Detail) {
        var next = detail.items
        if !next.contains(where: { $0.path == app.path }) {
            next.insert(Self.bundleRow(app), at: 0)
        }
        items = next
        // Preselected items are chosen unless the user already decided about them.
        let auto = next.filter { $0.preselected && !touched.contains($0.path) }.map(\.path)
        picked.formUnion(auto)
        var seen = Set<String>()
        background = detail.background.filter { seen.insert("\($0.kind):\($0.label)").inserted }
        var known = Set<String>()
        receipts = detail.receipts.filter { known.insert($0).inserted }
    }

    // MARK: What is shown

    /// Installer files under the app or a Library item are already listed there.
    var visible: [AppsAPI.Related] {
        let taken =
            [app.path]
            + items.filter { $0.location == "User Library" || $0.location == "System Library" }.map(\.path)
        return items.filter { item in
            item.location != "Installer receipt"
                || !taken.contains { item.path == $0 || item.path.hasPrefix("\($0)/") }
        }
    }

    var pickedVisible: [AppsAPI.Related] {
        visible.filter { picked.contains($0.path) }
    }

    var totalBytes: Int64 {
        pickedVisible.reduce(0) { $0 + $1.sizeBytes }
    }

    /// How many of the picked items need an administrator password.
    var adminPicked: Int {
        pickedVisible.filter(\.admin).count
    }

    func rows(in location: String) -> [AppsAPI.Related] {
        visible.filter { $0.location == location }
    }

    // MARK: Choosing

    func set(_ paths: [String], on: Bool) {
        for path in paths { touched.insert(path) }
        if on {
            picked.formUnion(paths)
        } else {
            picked.subtract(paths)
        }
    }

    func toggle(_ path: String) {
        set([path], on: !picked.contains(path))
    }

    // MARK: Uninstall

    var confirmTitle: String {
        if ACPlatform.isWindows { return "Uninstall \(app.name)?" }
        let count = pickedVisible.count
        return "Move \(count) item\(count == 1 ? "" : "s") to Trash?"
    }

    var confirmMessage: String {
        if ACPlatform.isWindows {
            return "\(app.name)'s own uninstaller opens and may ask for administrator rights. Once it has finished, any leftover folders you selected go to the Recycle Bin. If the uninstaller is cancelled, none are touched."
        }
        var text = "\(app.name) and the selected files, \(AppsFormat.bytes(totalBytes)) in all. Everything goes to the Trash, so it can be put back."
        if app.running { text += " The app will be asked to quit first." }
        if adminPicked > 0 { text += " Root-owned items make Finder ask for your password." }
        return text
    }

    var confirmLabel: String {
        ACPlatform.isWindows ? "Uninstall" : "Move to Trash"
    }

    /// Moves the picked items. The engine re-checks everything again before it moves anything.
    func uninstall() async {
        confirmOpen = false
        busy = true
        error = nil
        defer { busy = false }
        do {
            let outcome = try await client.uninstall(
                path: app.path,
                bundleId: app.bundleId,
                items: pickedVisible.map(\.path)
            )
            result = outcome
            let gone = Set(outcome.moved.map(\.path))
            items.removeAll { gone.contains($0.path) }
            picked = []
        } catch {
            self.error = ACFormat.describe(error)
        }
    }

    func reveal(_ path: String) async {
        do {
            try await client.reveal(path: path)
        } catch {
            self.error = ACFormat.describe(error)
        }
    }
}

/// Login and background items that start by themselves. Read only; loaded when opened, not on a timer.
@MainActor
@Observable
final class StartupModel {
    var report: AppsAPI.StartupReport?
    var loading = false
    var error: String?
    var isOpen = false

    private let client: AppsClient

    init(client: AppsClient) {
        self.client = client
    }

    /// Opens or closes the section; the first opening reads the items.
    func toggle() {
        isOpen.toggle()
        if isOpen && report == nil && !loading {
            Task { [weak self] in
                await self?.load()
            }
        }
    }

    func load() async {
        loading = true
        error = nil
        do {
            report = try await client.startupItems()
        } catch {
            self.error = ACFormat.describe(error)
        }
        loading = false
    }

    func reveal(_ path: String) async {
        do {
            try await client.reveal(path: path)
        } catch {
            self.error = ACFormat.describe(error)
        }
    }

    /// One group of rows: the scope the engine reports and the heading it gets.
    struct Scope: Identifiable, Sendable {
        let id: String
        let label: String
    }

    /// The scopes the rows are grouped in.
    static let scopes: [Scope] = [
        Scope(id: "user", label: "Your account"),
        Scope(id: "all users", label: "All users"),
        Scope(id: "system", label: "System"),
    ]
}
