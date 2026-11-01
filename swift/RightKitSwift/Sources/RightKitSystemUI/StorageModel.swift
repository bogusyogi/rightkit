#if os(macOS)
import Foundation
import Observation
import RightKitHub
import SwiftUI

/// State and actions of the Storage section, ported from `hub/src/views/Storage.tsx`.
///
/// One workspace at a time: Findings, Folders, Changes or Duplicates. Every destructive action goes
/// through a confirmation, and every single-item action reads the item's identity (dev and ino)
/// when the person asks; the engine checks it again right before anything moves.
@MainActor
@Observable
final class StorageModel {
    enum Pane: String, CaseIterable, Sendable {
        case findings, folders, changes, duplicates

        var title: String {
            switch self {
            case .findings: return "Findings"
            case .folders: return "Folders"
            case .changes: return "Changes"
            case .duplicates: return "Duplicates"
            }
        }
    }

    /// The filter whose choices are open under the filter row.
    enum FilterMenu: Sendable { case kind, size, changed }

    struct PendingTrash { let label: String; let items: [StorageFinding] }
    struct ItemTrash { let row: StorageRow; let id: StorageFileIdentity }
    struct ItemMove { let row: StorageRow; let id: StorageFileIdentity; let destination: String; let target: String }
    /// A move waiting for its destination folder, typed in the inspector (only where the platform has no
    /// folder chooser).
    struct MoveDraft { let row: StorageRow; let id: StorageFileIdentity; var destination: String }
    struct Undo { let id: String; let text: String }
    struct Inspection { let path: String; let data: StorageInspect?; let error: String? }
    /// The picture the inspector shows for an image file: the bytes the engine read (`storage_preview`),
    /// decoded by the platform services when the view draws them.
    struct Preview: Sendable {
        enum Phase: Sendable { case reading, ready(Data), failed(String) }
        let path: String
        let phase: Phase
    }

    /// The rows on screen and what the filters keep of them.
    struct Listing {
        var allRows: [StorageRow] = []
        var rows: [StorageRow] = []
        var undated = 0
        var filtersOn = false
        var total: Int64 = 1
        var largest: Int64 = 1
    }

    // MARK: State

    @ObservationIgnored let ctx: HubContext
    @ObservationIgnored let notch: any NotchBridge
    let duplicates: StorageDuplicatesModel

    var volumes: [StorageVolume] = []
    var active = "/"
    var folder: StorageFolder?
    /// The file access permission itself; nil until the first answer.
    var access: String?
    var results: [StorageRow]?
    var query = ""
    var growth: StorageGrowth?
    var report: StorageCleanupReport?
    var findingsError: String?
    var showAll = false
    var showNotes = false
    var pane: Pane = .findings
    var health: StorageHealthReport?
    var healthError: String?
    /// The volume whose drive-health panel is open, by mount point.
    var healthOpen: String?
    var openGroups: Set<String> = []
    var pending: PendingTrash?
    var itemTrash: ItemTrash?
    var itemMove: ItemMove?
    var moveDraft: MoveDraft?
    var undo: Undo?
    var busy = false
    var scanning = false
    var scanSeconds = 0
    /// True until the saved view has been asked for, so the page never shows an empty "Scanning…" first.
    var opening = true
    var findingsBusy = false
    var error: String?
    /// The row the inspector shows (by path), and what the engine read about it.
    var selected: String?
    var inspect: Inspection?
    /// The inspector's picture of the selected image; nil for every other selection.
    var preview: Preview?
    var kindFilter: StorageKindFilter = .all
    /// Bytes an item must exceed; 0 means any size.
    var minSize: Int64 = 0
    var changedFilter: StorageChangedFilter = .any
    var filterMenu: FilterMenu?
    /// True while the volume choices are open under the volume line (the page's Volume select).
    var volumeMenu = false
    /// The path whose "Actions for <name>" menu is open under its row.
    var rowMenu: String?
    /// Search matches the date range left out for having no date.
    var searchUndated = 0
    var ejecting: String?
    var ejectError: String?
    /// "Scan when Storage or Cleanup opens" (Settings > General > Background), on by default.
    var autoScan = true
    /// "Compress images and videos" (Settings > General > Background), off by default; read when a row is chosen.
    var compressOn = false
    /// The last compression result, shown until dismissed.
    var compressNote: String?

    @ObservationIgnored private var scanToken = 0
    @ObservationIgnored private var pickedPane = false
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var inspectTask: Task<Void, Never>?
    @ObservationIgnored private var previewTask: Task<Void, Never>?
    @ObservationIgnored private var lastDestination: String?
    private static var cachedReport: StorageCleanupReport?

    init(ctx: HubContext, notch: any NotchBridge) {
        self.ctx = ctx
        self.notch = notch
        self.duplicates = StorageDuplicatesModel(ctx: ctx)
        self.report = Self.cachedReport
    }

    // MARK: Engine

    private func ask<P: Encodable & Sendable, R: Decodable & Sendable>(
        _ method: String, _ params: P, as type: R.Type, timeout: Duration? = nil
    ) async throws -> R {
        try await ctx.engine.call(method, params, as: type, timeout: timeout)
    }

    private func askNothing<R: Decodable & Sendable>(_ method: String, as type: R.Type, timeout: Duration? = nil) async throws -> R {
        try await ctx.engine.call(method, EngineEmpty(), as: type, timeout: timeout)
    }

    /// A command whose answer is `null`.
    private func send<P: Encodable & Sendable>(_ method: String, _ params: P, timeout: Duration? = nil) async throws {
        let body = try JSONEncoder().encode(params)
        _ = try await ctx.engine.call(method, params: body, timeout: timeout)
    }

    private func run(_ work: @MainActor () async throws -> Void) async {
        busy = true
        error = nil
        do {
            try await work()
        } catch {
            self.error = StorageText.describe(error)
        }
        busy = false
    }

    /// Starts an action from a button.
    func start(_ work: @escaping @MainActor () async throws -> Void) {
        Task { [self] in await run(work) }
    }

    // MARK: Opening

    private struct NotchEnvelope: Decodable {
        struct Settings: Decodable { let hubAutoScan: Bool?; let mediaCompression: Bool? }
        let settings: Settings?
    }

    private func readCompression() async {
        guard let data = await notch.state(), let envelope = try? JSONDecoder().decode(NotchEnvelope.self, from: data) else {
            compressOn = false
            return
        }
        compressOn = envelope.settings?.mediaCompression == true
    }

    /// Writes a separate, smaller copy in a "Pulse Compressed" folder beside the file (a video can take
    /// minutes); the original is never changed.
    func compress(_ row: StorageRow) {
        compressNote = nil
        start { [self] in
            let done = try await ask("media_compress", StoragePathParams(path: row.path), as: StorageCompressOutcome.self, timeout: .seconds(7_500))
            compressNote = "Saved \(StorageText.bytes(done.saved_bytes)): \(StorageText.shortPath(done.output)) is \(StorageText.bytes(done.output_bytes)), the original is unchanged."
        }
    }

    /// Whether "Compress a copy" is offered for this row: the switch is on and it is a JPEG, PNG, MP4 or MOV file.
    func canCompress(_ row: StorageRow) -> Bool {
        guard compressOn, !row.is_dir else { return false }
        return ["jpg", "jpeg", "png", "mp4", "m4v", "mov"].contains((row.path as NSString).pathExtension.lowercased())
    }

    private func readAutoScan() async {
        guard let data = await notch.state(), let envelope = try? JSONDecoder().decode(NotchEnvelope.self, from: data) else {
            autoScan = true
            return
        }
        autoScan = envelope.settings?.hubAutoScan != false
    }

    /// Opens on the saved view at once. A rescan runs behind it only when the saved one is old; with no
    /// saved view at all, the first scan starts now. A scan started before the section was reopened
    /// keeps running; it is followed.
    func load() async {
        await readAutoScan()
        volumes = (try? await askNothing("volumes", as: [StorageVolume].self)) ?? []
        let status = try? await askNothing("scan_status", as: StorageScanStatus.self)
        if let status, status.running {
            let external = volumes.first { !$0.isInternal && $0.mount_point == status.running_root }
            active = external?.mount_point ?? volumes.first { $0.isInternal }?.mount_point ?? "/"
            opening = false
            await refreshHealth()
            await follow()
            return
        }
        let saved = (try? await askNothing("last_scan", as: StorageFolder?.self)) ?? nil
        opening = false
        if let saved {
            applyFolder(saved)
            // Without a live refresh a walk of the whole folder is long and nothing keeps the view
            // current, so a recent saved scan waits for Rescan.
            let old = status?.live_refresh == false ? StorageText.isOld(saved.scanned_at) : StorageText.isStale(saved.scanned_at)
            // A saved network scan is never refreshed behind the page; "Scan anyway" refreshes it.
            if old && autoScan && !onNetwork(saved.root) {
                let mount = mountFor(saved.root)
                Task { [self] in await scanVolume(mount) }
            }
            await loadCompanions(for: saved)
        } else if autoScan {
            Task { [self] in await scanVolume("/") }
        }
        await refreshHealth()
    }

    /// Drive health for every drive card. The engine samples smartctl itself at most every ten
    /// minutes, so polling here only reads the saved view.
    func healthLoop() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(60))
            if Task.isCancelled { return }
            await refreshHealth()
        }
    }

    func refreshHealth() async {
        let mounts = volumes.filter { !$0.disk_image && $0.kind == "local" }.map(\.mount_point)
        if mounts.isEmpty { return }
        do {
            health = try await ask("drive_health", StorageMountsParams(mounts: mounts), as: StorageHealthReport.self, timeout: .seconds(120))
            healthError = nil
        } catch {
            healthError = StorageText.describe(error)
        }
    }

    func refreshVolumes() async {
        if let list = try? await askNothing("volumes", as: [StorageVolume].self) { volumes = list }
    }

    func refreshAccess() async {
        if let value = try? await askNothing("fda_status", as: String.self) {
            access = value
        } else {
            access = "unknown"
        }
    }

    // MARK: Volumes and scans

    /// The volume a saved scan belongs to: its external drive, else the startup disk.
    func mountFor(_ root: String) -> String {
        volumes.first { !$0.isInternal && $0.mount_point == root }?.mount_point ?? "/"
    }

    /// True for a path on a network volume: such a volume is never scanned without the person asking.
    func onNetwork(_ path: String) -> Bool {
        volumes.contains { volume in
            guard volume.isNetwork else { return false }
            var root = volume.mount_point
            while root.hasSuffix("/") || root.hasSuffix("\\") { root.removeLast() }
            return path == root || path.hasPrefix(root + "/") || path.hasPrefix(root + "\\")
        }
    }

    private func isInternal(_ mount: String) -> Bool {
        guard let volume = volumes.first(where: { $0.mount_point == mount }) else { return true }
        return volume.isInternal
    }

    var activeVolume: StorageVolume? {
        volumes.first { !$0.disk_image && $0.mount_point == active }
    }

    var internalActive: Bool {
        volumes.first { $0.mount_point == active }?.isInternal ?? (active == "/")
    }

    var volumeList: [StorageVolume] { volumes.filter { !$0.disk_image } }
    var installers: [StorageVolume] { volumes.filter(\.disk_image) }

    /// Shows data that is already scanned: picks the matching drive card.
    private func applyFolder(_ f: StorageFolder) {
        let external = volumes.first { !$0.isInternal && $0.mount_point == f.root }
        active = external?.mount_point ?? volumes.first { $0.isInternal }?.mount_point ?? "/"
        setFolder(f)
    }

    /// Selection belongs to one folder listing: moving to another folder or rescanning clears it.
    private func setFolder(_ f: StorageFolder) {
        folder = f
        selected = nil
        inspect = nil
        clearPreview()
        moveDraft = nil
        inspectTask?.cancel()
        scheduleSearch()
    }

    /// The growth line (home only), the cleanup findings (once) and the access state.
    private func loadCompanions(for f: StorageFolder) async {
        let external = volumes.contains { !$0.isInternal && $0.mount_point == f.root }
        if external {
            growth = nil
        } else {
            growth = try? await askNothing("growth", as: StorageGrowth.self)
            await loadFindings()
        }
        await refreshAccess()
    }

    /// The startup disk is scanned from the home folder; other volumes from their root. Starting a
    /// scan cancels the one running; there is never more than one.
    func scanVolume(_ mount: String) async {
        scanToken += 1
        let token = scanToken
        active = mount
        query = ""
        scheduleSearch()
        error = nil
        scanning = true
        startTicker(token)
        do {
            let scanned = try await ask(
                "scan", StorageScanParams(path: isInternal(mount) ? nil : mount), as: StorageFolder.self, timeout: .seconds(3600)
            )
            guard token == scanToken else { return }
            applyFolder(scanned)
            scanning = false
            await loadCompanions(for: scanned)
        } catch {
            guard token == scanToken else { return }
            scanning = false
            let text = StorageText.describe(error)
            if !text.contains("cancelled") { self.error = text }
        }
    }

    func rescan() {
        let mount = active
        Task { [self] in await scanVolume(mount) }
    }

    /// Choosing a network volume only shows it; its scan waits for "Scan anyway".
    func chooseVolume(_ mount: String) {
        volumeMenu = false
        if volumes.first(where: { $0.mount_point == mount })?.isNetwork != true {
            Task { [self] in await scanVolume(mount) }
            return
        }
        scanToken += 1
        scanning = false
        active = mount
        folder = nil
        results = nil
        selected = nil
        inspect = nil
        clearPreview()
        query = ""
        searchTask?.cancel()
        searchUndated = 0
        error = nil
    }

    /// A scan started before this view was reopened keeps running; wait for it.
    private func follow() async {
        scanToken += 1
        let token = scanToken
        scanning = true
        startTicker(token)
        while true {
            try? await Task.sleep(for: .milliseconds(1500))
            if Task.isCancelled || token != scanToken { return }
            let status = try? await askNothing("scan_status", as: StorageScanStatus.self)
            if let status, !status.running { break }
        }
        let last = (try? await askNothing("last_scan", as: StorageFolder?.self)) ?? nil
        guard token == scanToken else { return }
        if let last {
            applyFolder(last)
            scanning = false
            await loadCompanions(for: last)
        } else {
            scanning = false
        }
    }

    /// The engine reports no scan progress, so the page shows how long the scan has run.
    private func startTicker(_ token: Int) {
        scanSeconds = 0
        Task { [self] in
            while scanning && token == scanToken {
                try? await Task.sleep(for: .seconds(1))
                if scanning && token == scanToken { scanSeconds += 1 }
            }
        }
    }

    // MARK: Folders and search

    func open(_ path: String) {
        start { [self] in
            setFolder(try await ask("children", StoragePathParams(path: path), as: StorageFolder.self))
        }
    }

    func openFromChange(_ path: String) {
        pane = .folders
        open(path)
    }

    func setQuery(_ value: String) {
        query = value
        scheduleSearch()
    }

    func setKindFilter(_ value: StorageKindFilter) {
        kindFilter = value
        filterMenu = nil
    }

    func setMinSize(_ value: Int64) {
        minSize = value
        filterMenu = nil
        scheduleSearch()
    }

    func setChangedFilter(_ value: StorageChangedFilter) {
        changedFilter = value
        filterMenu = nil
        scheduleSearch()
    }

    func clearFilters() {
        kindFilter = .all
        minSize = 0
        changedFilter = .any
        filterMenu = nil
        scheduleSearch()
    }

    var hasDates: Bool { folder?.has_dates == true }

    /// A date choice made on an earlier scan does not apply once the scan has no dates.
    var effectiveChanged: StorageChangedFilter { hasDates ? changedFilter : .any }

    private func scheduleSearch() {
        searchTask?.cancel()
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard folder != nil, text.count >= 2 else {
            results = nil
            searchUndated = 0
            return
        }
        let size = minSize
        let range = hasDates ? effectiveChanged.range(now: Date().timeIntervalSince1970.rounded(.down)) : nil
        searchTask = Task { [self] in
            try? await Task.sleep(for: .milliseconds(200))
            if Task.isCancelled { return }
            do {
                // The size bound goes to the engine so the search's result limit fills with matches.
                let found = try await ask(
                    "search", StorageSearchParams(query: text, minBytes: size > 0 ? size : nil, changed: range), as: StorageSearchResult.self
                )
                if Task.isCancelled { return }
                results = found.rows
                searchUndated = found.undated
            } catch {
                if Task.isCancelled { return }
                self.error = StorageText.describe(error)
            }
        }
    }

    /// Every row of the listing, then the rows the filters keep. Shares are of the whole listing.
    var listing: Listing {
        var out = Listing()
        out.allRows = results ?? folder?.rows ?? []
        let changed = effectiveChanged
        out.filtersOn = kindFilter != .all || minSize > 0 || changed != .any
        // Search results were cut to the date range by the engine; a folder listing is cut here.
        let listRange = results == nil ? changed.range(now: Date().timeIntervalSince1970.rounded(.down)) : nil
        var listUndated = 0
        if out.filtersOn {
            out.rows = out.allRows.filter { row in
                if row.summary == true || (kindFilter != .all && StorageKindFilter.of(row) != kindFilter) || row.bytes <= minSize {
                    return false
                }
                guard let range = listRange else { return true }
                let modified = row.modified ?? 0
                if modified == 0 {
                    listUndated += 1
                    return false
                }
                if let since = range.since, modified < since { return false }
                if let before = range.before, modified >= before { return false }
                return true
            }
        } else {
            out.rows = out.allRows
        }
        out.undated = changed == .any ? 0 : (results != nil ? searchUndated : listUndated)
        out.largest = max(1, out.rows.map(\.bytes).max() ?? 1)
        out.total = max(1, out.allRows.reduce(0) { $0 + $1.bytes })
        return out
    }

    /// The path of the open folder, one button per level.
    var crumbs: [(name: String, path: String)] {
        guard let folder else { return [] }
        var parts: [(name: String, path: String)] = []
        var path = folder.path
        while !path.isEmpty && path.count >= folder.root.count {
            let leaf = path.split(separator: "/").last.map(String.init) ?? path
            parts.insert((name: path == folder.root ? folder.root_label : leaf, path: path), at: 0)
            if path == folder.root { break }
            if let slash = path.lastIndex(of: "/") {
                let parent = String(path[..<slash])
                path = parent.isEmpty ? "/" : parent
            } else {
                break
            }
        }
        return parts
    }

    // MARK: Selection and inspector

    func select(_ path: String?) {
        selected = path
        inspect = nil
        clearPreview()
        moveDraft = nil
        inspectTask?.cancel()
        guard let path else { return }
        startPreview(path)
        Task { [self] in await readCompression() }
        inspectTask = Task { [self] in
            do {
                let data = try await ask("storage_inspect", StoragePathParams(path: path), as: StorageInspect.self)
                if !Task.isCancelled, selected == path { inspect = Inspection(path: path, data: data, error: nil) }
            } catch {
                if !Task.isCancelled, selected == path {
                    inspect = Inspection(path: path, data: nil, error: StorageText.describe(error))
                }
            }
        }
    }

    /// The extensions the inspector previews. HEIC, BMP and TIFF are not (the page's list).
    static let previewExtensions: Set<String> = ["png", "jpg", "jpeg", "gif", "webp"]
    /// The longest side the preview is decoded to: the inspector's 256 pt width at 2x.
    static let previewPixels = 512

    /// Whether the inspector previews this file: an image of a kind in `previewExtensions`.
    func isPreviewable(_ row: StorageRow) -> Bool {
        guard !row.is_dir, let dot = row.path.lastIndex(of: ".") else { return false }
        return Self.previewExtensions.contains(String(row.path[row.path.index(after: dot)...]).lowercased())
    }

    private func clearPreview() {
        previewTask?.cancel()
        previewTask = nil
        preview = nil
    }

    /// Reads and decodes the picture of a selected image. Nothing is read for other kinds, and nothing
    /// on a network volume: the file is never touched there.
    private func startPreview(_ path: String) {
        guard let row = listing.allRows.first(where: { $0.path == path }), isPreviewable(row), !onNetwork(path) else { return }
        preview = Preview(path: path, phase: .reading)
        previewTask = Task { [self] in
            @MainActor func finish(_ phase: Preview.Phase) {
                if !Task.isCancelled, selected == path { preview = Preview(path: path, phase: phase) }
            }
            do {
                let reply = try await ask("storage_preview", StoragePathParams(path: path), as: StoragePreviewReply.self, timeout: .seconds(60))
                guard !Task.isCancelled, selected == path else { return }
                guard let bytes = Data(base64Encoded: reply.base64), !bytes.isEmpty else {
                    finish(.failed("The preview could not be read."))
                    return
                }
                // Decoded now, so the view draws the picture the moment it shows it.
                if await HubImages.preload(bytes, maxPixel: Self.previewPixels, services: ctx.services) {
                    finish(.ready(bytes))
                } else {
                    finish(.failed("This image cannot be previewed here."))
                }
            } catch {
                finish(.failed(StorageText.describe(error)))
            }
        }
    }

    /// Opens a folder and waits for it: how the off-screen renderer shows a folder other than the saved one.
    func openAndWait(_ path: String) async {
        await run { [self] in
            setFolder(try await ask("children", StoragePathParams(path: path), as: StorageFolder.self))
        }
    }

    /// Waits for the inspector's reads and the preview, for the off-screen renderer.
    func settle() async {
        await inspectTask?.value
        await previewTask?.value
    }

    // MARK: Single-item actions

    func showInFileManager(_ path: String) {
        Task { [self] in
            do {
                try await send("finder_open", StoragePathParams(path: path))
            } catch {
                self.error = StorageText.describe(error)
            }
        }
    }

    func copyText(_ text: String) {
        Task { [self] in
            do {
                try await ctx.services.writeClipboardText(text)
            } catch {
                self.error = "Could not copy: \(StorageText.describe(error))"
            }
        }
    }

    /// Drops a moved or trashed item from what is on screen. The saved scan index is not rewritten
    /// here, so a rescan (or re-drilling) shows it until then.
    private func dropRow(_ path: String) {
        if selected == path {
            selected = nil
            inspect = nil
            clearPreview()
        }
        if let current = results {
            results = current.filter { $0.path != path }
        } else if var f = folder {
            f.rows = f.rows.filter { $0.path != path }
            folder = f
        }
    }

    func askTrash(_ row: StorageRow) {
        start { [self] in
            let id = try await ask("file_identity", StoragePathParams(path: row.path), as: StorageFileIdentity.self)
            itemTrash = ItemTrash(row: row, id: id)
        }
    }

    func confirmItemTrash(_ item: ItemTrash) {
        itemTrash = nil
        start { [self] in
            do {
                try await send("file_trash", StorageIdentityParams(path: item.row.path, dev: item.id.dev, ino: item.id.ino))
                dropRow(item.row.path)
            } catch {
                await refreshVolumes()
                throw error
            }
            await refreshVolumes()
        }
    }

    private func finishMove(_ row: StorageRow, _ id: StorageFileIdentity, _ destination: String, copyAcrossVolumes: Bool) async throws {
        do {
            _ = try await ask(
                "file_move",
                StorageMoveParams(path: row.path, dev: id.dev, ino: id.ino, destination: destination, copyAcrossVolumes: copyAcrossVolumes),
                as: StorageMoveResult.self,
                timeout: .seconds(3600)
            )
            dropRow(row.path)
        } catch {
            await refreshVolumes()
            throw error
        }
        await refreshVolumes()
    }

    /// "Move to…": reads the identity now, then asks for the destination folder with the platform's folder
    /// chooser, and moves through `planMove` as a typed destination would. Cancelling the chooser changes
    /// nothing. Where the platform has no chooser, the destination is typed in the inspector instead.
    func startMove(_ row: StorageRow) {
        start { [self] in
            let id = try await ask("file_identity", StoragePathParams(path: row.path), as: StorageFileIdentity.self)
            var destination = lastDestination ?? ""
            if destination.isEmpty { destination = (try? await askNothing("home_path", as: String.self)) ?? "" }
            let draft = MoveDraft(row: row, id: id, destination: destination)
            let chosen: String?
            do {
                chosen = try await ctx.services.chooseFolder(
                    prompt: "Move \(row.name) into which folder?",
                    startingAt: destination.isEmpty ? nil : destination
                )
            } catch let failure as HubPlatformError {
                // No folder chooser on this platform: fall back to the typed destination.
                guard case .unsupported = failure else { throw failure }
                moveDraft = draft
                return
            }
            guard let chosen else { return }
            try await planMove(draft, destination: chosen)
        }
    }

    func setMoveDestination(_ value: String) {
        moveDraft?.destination = value
    }

    func cancelMove() { moveDraft = nil }

    /// Within one drive the item moves at once; across drives a copy is confirmed first.
    func planMove() {
        guard let draft = moveDraft else { return }
        let destination = draft.destination.trimmingCharacters(in: .whitespacesAndNewlines)
        if destination.isEmpty { return }
        start { [self] in try await planMove(draft, destination: destination) }
    }

    /// Asks the engine where the item would go, with the identity read when the person asked.
    private func planMove(_ draft: MoveDraft, destination: String) async throws {
        let plan = try await ask(
            "file_move_plan",
            StorageMovePlanParams(path: draft.row.path, dev: draft.id.dev, ino: draft.id.ino, destination: destination),
            as: StorageMovePlan.self
        )
        lastDestination = destination
        if plan.same_volume {
            try await finishMove(draft.row, draft.id, destination, copyAcrossVolumes: false)
            moveDraft = nil
        } else {
            moveDraft = nil
            itemMove = ItemMove(row: draft.row, id: draft.id, destination: destination, target: plan.target)
        }
    }

    func confirmItemMove(_ item: ItemMove) {
        itemMove = nil
        start { [self] in
            try await finishMove(item.row, item.id, item.destination, copyAcrossVolumes: true)
        }
    }

    // MARK: Findings

    /// Findings show at once from the saved copy. A background rescan follows when there is none,
    /// when it is old, or when a move has just changed the list.
    func loadFindings(force: Bool = false) async {
        if let cached = Self.cachedReport {
            report = cached
            settlePane()
            if force || (autoScan && StorageText.isStale(cached.scanned_at)) { await refreshFindings() }
            return
        }
        do {
            let saved = try await askNothing("cleanup_cached", as: StorageCleanupReport?.self)
            if let saved, Self.cachedReport == nil {
                Self.cachedReport = saved
                report = saved
                settlePane()
            }
            if force || (autoScan && (saved == nil || StorageText.isStale(saved?.scanned_at ?? 0))) { await refreshFindings() }
        } catch {
            if autoScan || force { await refreshFindings() }
        }
    }

    func refreshFindings() async {
        findingsBusy = true
        do {
            let fresh = try await askNothing("cleanup_scan", as: StorageCleanupReport.self, timeout: .seconds(900))
            Self.cachedReport = fresh
            report = fresh
            findingsError = nil
            settlePane()
        } catch {
            findingsError = StorageText.describe(error)
        }
        findingsBusy = false
    }

    /// With nothing to clear, opens on the folders instead of an empty list (once, so a pane the
    /// person picked is never taken away).
    private func settlePane() {
        if !pickedPane, report != nil, eligible.isEmpty { pane = .folders }
    }

    func choosePane(_ value: Pane) {
        pickedPane = true
        pane = value
    }

    var eligible: [StorageFinding] {
        (report?.findings ?? []).filter(\.eligible).sorted { $0.bytes > $1.bytes }
    }

    var others: [StorageFinding] {
        (report?.findings ?? []).filter { !$0.eligible && $0.bytes > 0 }.sorted { $0.bytes > $1.bytes }
    }

    var safeItems: [StorageFinding] { eligible.filter { $0.risk == "safe" } }

    var freeable: Int64 { report.map { $0.safe_bytes + $0.review_bytes } ?? 0 }

    var groups: [StorageFindingGroup] { StorageFindingGroup.group(eligible) }

    func toggleGroup(_ key: String) {
        if openGroups.contains(key) { openGroups.remove(key) } else { openGroups.insert(key) }
    }

    func confirmMove(_ request: PendingTrash) {
        pending = nil
        start { [self] in
            let items = request.items.map { StorageCleanupItem(rule_id: $0.rule_id, path: $0.path, dev: $0.dev, ino: $0.ino) }
            let result = try await ask(
                "cleanup_apply", StorageCleanupApplyParams(items: items), as: StorageCleanupApplyResult.self, timeout: .seconds(900)
            )
            if let activity = result.activity_id {
                let skipped = result.skipped.isEmpty ? "" : "; \(result.skipped.count) skipped"
                undo = Undo(
                    id: activity,
                    text: "Moved to the \(StorageText.trash): \(StorageText.upTo(result.moved_bytes)) on disk\(skipped). \(StorageText.trashFreesNothing)"
                )
            } else if !result.skipped.isEmpty {
                error = result.skipped.map { "\(StorageText.shortPath($0.path)): \($0.reason)" }.joined(separator: "\n")
            }
            await refreshVolumes()
            await loadFindings(force: true)
        }
    }

    func undoMove() {
        guard let current = undo else { return }
        start { [self] in
            _ = try await ask("cleanup_restore", StorageIdParams(id: current.id), as: StorageAny.self, timeout: .seconds(900))
            undo = nil
            await loadFindings(force: true)
        }
    }

    // MARK: Volumes

    func eject(_ volume: StorageVolume) {
        ejecting = volume.mount_point
        ejectError = nil
        Task { [self] in
            do {
                try await send("eject", StorageMountParams(mount: volume.mount_point), timeout: .seconds(120))
            } catch {
                ejectError = "\(volume.name): \(StorageText.describe(error))"
            }
            ejecting = nil
            await refreshVolumes()
        }
    }

    func openFullDiskAccess() {
        Task { [self] in
            do {
                try await send("fda_request", EngineEmpty())
            } catch {
                self.error = StorageText.describe(error)
            }
        }
    }

    // MARK: Text

    var scanText: String {
        if let folder, scanning { return "Updated \(StorageText.ago(folder.scanned_at)) · Refreshing…" }
        if scanning { return "Scanning… \(scanSeconds) s" }
        if let folder { return "Updated \(StorageText.ago(folder.scanned_at))\(folder.from_snapshot == true ? " (saved)" : "")" }
        return ""
    }

    var headline: String {
        guard report != nil else { return "Looking for what is safe to clear…" }
        return freeable > 0 ? "\(StorageText.upTo(freeable)) eligible to move" : "Nothing obvious to clear"
    }
}

/// Any JSON value: for commands whose answer the page ignores.
struct StorageAny: Decodable, Sendable {
    init(from decoder: any Decoder) throws {}
}
#endif
