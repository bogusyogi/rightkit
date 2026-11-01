#if os(macOS)
import RightKitNotchCore
import AppKit
import Foundation

/// The clipboard history store (docs/clipboard-spec.md): one per process, read by the strip
/// and by the launcher's clip mode. Capture is section 3 (this file and ClipboardCapture),
/// the disk is 2.5 (ClipboardDisk), retention is 4 (ClipboardRetention), search is 5
/// (ClipboardSearch), pinboards are 6 (ClipboardBoards), and writing back to the pasteboard
/// is 7.11.2 (ClipboardPasteboard). Off until `settings.enabled`: then no timer runs and the
/// pasteboard is never read, and never under test. The main thread reads the pasteboard and
/// keeps the lists; hashing, detection, thumbnails and every file write run on `io`.
/// Nothing here logs an item's text, preview or path (9).
@MainActor
public final class LauncherClipboard {
    public static let ownType = NSPasteboard.PasteboardType("com.pulse.clipboard.own")
    public static let sourceType = NSPasteboard.PasteboardType("org.nspasteboard.source")
    private static let skippedTypes: Set<String> = [
        "org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType",
        "de.petermaurer.TransientPasteboardType", "com.typeit4me.clipping",
        "Pasteboard generator type", "com.agilebits.onepassword",
    ]
    private static let screenshotSources: Set<String> = [
        "com.apple.screencaptureui", "com.apple.screenshot.launcher",
    ]
    private static let pollInterval: TimeInterval = 0.4
    /// With no keyboard, mouse or trackpad input for `idleAfter` seconds nothing can have been
    /// copied by a person, so the pasteboard's change count is read every 2 s, not every 0.4 s;
    /// the first input brings the fast beat back within one slow tick (#148).
    private static let idlePollInterval: TimeInterval = 2
    private static let idleAfter: TimeInterval = 20
    private var pollIsIdle = false
    private static let undoSeconds: TimeInterval = 6

    /// "mac 0.9.0" for the index's `writer` key.
    private static let writer: String = {
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0"
        return "mac " + version
    }()

    /// Pulse itself: the source of its own screenshots (4).
    public static var pulseApp: ClipSourceApp {
        ClipSourceApp(id: Bundle.main.bundleIdentifier ?? "dev.orthic.pulse", name: "Pulse")
    }

    /// The history, so Pulse's other features can tell it about their own writes.
    private(set) static weak var current: LauncherClipboard?

    public init() {
        Self.current = self
    }

    // MARK: - State

    /// The switches (8.1). `enabled` starts and stops watching; the caps rerun retention.
    public var settings = ClipSettings() {
        didSet {
            ClipSeal.setEnabled(settings.encrypt)
            settingsDidChange(from: oldValue)
        }
    }

    /// Newest `lastUsedAt` first.
    private(set) var items: [ClipItem] = []
    /// By `order`.
    private(set) var boards: [ClipBoard] = []
    private(set) var loadState: ClipLoadState = .ok
    /// Non-nil when pinned and board items alone use more than the byte cap: the strip's
    /// footer says "Pinned items use N MB" (4).
    private(set) var protectedOverCapBytes: Int?

    /// Called on the main thread after any change to the items, boards, thumbnails or app
    /// colours. Changes that come together give one call.
    public var onChange: (() -> Void)?

    public var count: Int { items.count }

    /// Payloads plus thumbnails.
    public var totalBytes: Int {
        items.reduce(thumbSizes.values.reduce(0, +)) { $0 + $1.size }
    }

    // Store-internal state, shared with the extensions in the Clipboard*.swift files.
    public let io = DispatchQueue(label: "dev.orthic.pulse.clipboard", qos: .utility)
    public var persistent = !Runtime.isUnderTest
    public var writable = false
    private(set) var isLoaded = false
    private var loadStarted = false
    private var lockFD: Int32 = -1
    public var thumbSizes: [String: Int] = [:]
    public var thumbPending: Set<String> = []
    public let thumbCache: NSCache<NSString, NSImage> = {
        let cache = NSCache<NSString, NSImage>()
        cache.countLimit = 64
        return cache
    }()
    public var appColours: [String: String] = [:]
    public var appColourPending: Set<String> = []
    private let pendingIndex = ClipPending<[ClipItem]>()
    private let pendingBoards = ClipPending<[ClipBoard]>()
    private var timer: Timer?
    private var retentionTimer: Timer?
    private var lastChangeCount = -1
    private var changePending = false
    private var epoch = 0
    private var inflight: Set<String> = []
    private var lastDigest = ""
    private var lastDigestAt = 0.0
    private var sourceNames: [String: String] = [:]
    private var retentionRunning = false
    private var retentionAgain = false

    public var directory: URL { LauncherStorage.clipboardDirectory }

    /// Whether the lists may change: the folder is ours, or the store is in memory only
    /// (under test, or holding fixtures).
    public var canMutate: Bool { writable || !persistent }

    // MARK: - Settings, watching

    private func settingsDidChange(from old: ClipSettings) {
        if !settings.enabled || !settings.copyBurst { ClipBurst.shared.end(leaveNewest: false) }
        if settings.enabled != old.enabled {
            if settings.enabled { start() } else { stop() }
            return
        }
        guard settings.enabled else { return }
        if settings.maxItems != old.maxItems || settings.maxMB != old.maxMB
            || settings.maxAgeDays != old.maxAgeDays || settings.screenshotHours != old.screenshotHours {
            retentionPass()
        }
    }

    private func start() {
        // The pasteboard is real user data: clipboard history never starts under test.
        if Runtime.isUnderTest { return }
        kickLoad()
        lastChangeCount = NSPasteboard.general.changeCount
        schedulePoll(idle: false)
        retentionTimer?.invalidate()
        let hourly = Timer(timeInterval: 3600, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.retentionPass() }
        }
        RunLoop.main.add(hourly, forMode: .common)
        retentionTimer = hourly
        if isLoaded { retentionPass() }
    }

    private func stop() {
        timer?.invalidate()
        timer = nil
        retentionTimer?.invalidate()
        retentionTimer = nil
    }

    /// Call after Pulse writes the pasteboard itself, so its own copy is not stored as new
    /// history (3.1).
    public func noteOwnWrite() {
        if Runtime.isUnderTest { return }
        lastChangeCount = NSPasteboard.general.changeCount
    }

    // MARK: - Loading

    /// Reads the folder on the background queue (2.5); the lists fill in when it is done.
    private func kickLoad() {
        guard !loadStarted else { return }
        loadStarted = true
        if Runtime.isUnderTest {
            isLoaded = true
            return
        }
        let dir = directory
        let writer = Self.writer
        io.async {
            let result = ClipDisk.load(dir: dir, writer: writer)
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.applyLoad(result) }
            }
        }
    }

    private func applyLoad(_ result: ClipLoaded) {
        items = result.items
        boards = result.boards
        loadState = result.state
        lockFD = result.lockFD
        thumbSizes = result.thumbSizes
        appColours = result.appColours
        writable = result.state == .ok
        isLoaded = true
        updateProtectedFigure()
        scheduleChange()
        if settings.enabled, writable { retentionPass() }
    }

    /// Fills the store without the disk (the picture-rendering run and the strip's fixtures):
    /// nothing is read or written afterwards.
    public func installFixture(items: [ClipItem], boards: [ClipBoard], loadState: ClipLoadState = .ok) {
        persistent = false
        writable = false
        loadStarted = true
        isLoaded = true
        self.items = items.sorted { $0.lastUsedAt > $1.lastUsedAt }
        self.boards = boards
        self.loadState = loadState
        scheduleChange()
    }

    // MARK: - Persisting

    public func persistIndex() {
        guard persistent, writable else { return }
        pendingIndex.set(items)
        let box = pendingIndex
        let dir = directory
        let writer = Self.writer
        io.async {
            if let snapshot = box.take() { ClipDisk.writeIndex(snapshot, writer: writer, dir: dir) }
        }
    }

    public func persistBoards() {
        guard persistent, writable else { return }
        pendingBoards.set(boards)
        let box = pendingBoards
        let dir = directory
        io.async {
            if let snapshot = box.take() { ClipDisk.writeBoards(snapshot, dir: dir) }
        }
    }

    public func persistColours() {
        guard persistent, writable else { return }
        let snapshot = appColours
        let dir = directory
        io.async { ClipDisk.writeColours(snapshot, dir: dir) }
    }

    /// Replaces the lists, writes what changed, tells the strip.
    public func commit(items newItems: [ClipItem]? = nil, boards newBoards: [ClipBoard]? = nil) {
        guard canMutate else { return }
        if let newItems {
            items = newItems
            persistIndex()
            updateProtectedFigure()
        }
        if let newBoards {
            boards = newBoards
            persistBoards()
        }
        scheduleChange()
    }

    /// One `onChange` call for any number of changes in the same turn.
    public func scheduleChange() {
        guard !changePending else { return }
        changePending = true
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.changePending = false
                self.onChange?()
            }
        }
    }

    private func updateProtectedFigure() {
        let cap = max(1, settings.maxMB) * 1024 * 1024
        let used = items.filter { $0.isProtected }.reduce(0) { $0 + $1.size + (thumbSizes[$1.id] ?? 0) }
        protectedOverCapBytes = used > cap ? used : nil
    }

    // MARK: - Capture

    private func schedulePoll(idle: Bool) {
        timer?.invalidate()
        pollIsIdle = idle
        let interval = idle ? Self.idlePollInterval : Self.pollInterval
        let poll = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        poll.tolerance = idle ? 0.5 : 0.05
        RunLoop.main.add(poll, forMode: .common)
        timer = poll
    }

    /// Seconds since the last keyboard, mouse or trackpad event in this login session.
    private static var secondsSinceInput: TimeInterval {
        CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: CGEventType(rawValue: ~0)!)
    }

    private func poll() {
        if timer != nil {
            let idle = Self.secondsSinceInput > Self.idleAfter
            if idle != pollIsIdle { schedulePoll(idle: idle) }
        }
        guard isLoaded, settings.enabled, !Runtime.isUnderTest else { return }
        let pasteboard = NSPasteboard.general
        let changed = pasteboard.changeCount
        guard changed != lastChangeCount else { return }
        lastChangeCount = changed
        // Another Pulse holds the folder, or the index is newer or unreadable: capture refused.
        guard writable, loadState == .ok else { return }
        capture(from: pasteboard)
    }

    /// 3.5 steps 1 to 5 from the pasteboard's type list alone: nil when the copy is skipped (Pulse's
    /// own write, concealed, transient, auto-generated, other-device, an ignored type, a kind
    /// switched off, nothing readable), else the kind to capture. Pure, so the QA journey asserts it.
    public nonisolated static func captureKind(types: Set<String>, settings: ClipSettings) -> ClipKind? {
        guard !types.contains(ownType.rawValue) else { return nil }
        var blocked = skippedTypes
        if settings.skipAutoGenerated { blocked.insert("org.nspasteboard.AutoGeneratedType") }
        if settings.skipRemote { blocked.insert("com.apple.is-remote-clipboard") }
        for type in settings.ignoredTypes { blocked.insert(type) }
        guard types.isDisjoint(with: blocked) else { return nil }

        // The kind is decided from the type list alone (3.2, 3.5 step 5).
        let hasFile = types.contains("public.file-url")
        let hasImage = types.contains("public.png") || types.contains("public.tiff")
        let hasText = types.contains("public.utf8-plain-text") || types.contains("NSStringPboardType")
        let hasHTML = types.contains("public.html")
        let hasRTF = types.contains("public.rtf")
        var kind: ClipKind
        if hasFile {
            kind = .file
        } else if hasImage {
            kind = hasText && (hasHTML || hasRTF) ? .rich : .image
        } else if hasText {
            kind = hasHTML || hasRTF ? .rich : .text
        } else {
            return nil
        }
        switch kind {
        case .file: guard settings.captureFiles else { return nil }
        case .image: guard settings.captureImages else { return nil }
        case .rich: if !settings.captureRich { kind = .text }
        default: break
        }
        return kind
    }

    /// 3.5, cheapest check first; the payload is read only after the type checks.
    private func capture(from pasteboard: NSPasteboard) {
        let types = Set((pasteboard.types ?? []).map { $0.rawValue })
        guard let kind = Self.captureKind(types: types, settings: settings) else { return }
        let hasHTML = types.contains("public.html")
        let hasRTF = types.contains("public.rtf")

        let source = sourceApp(of: pasteboard, types: types)
        if let source, settings.excludedApps.contains(source.id) { return }

        var input = ClipCaptureInput(kind: kind, at: Date().timeIntervalSince1970)
        input.source = source
        input.secretFilter = settings.secretFilter
        switch kind {
        case .file:
            let urls = pasteboard.readObjects(
                forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
            input.paths = urls.map { $0.path }
            guard !input.paths.isEmpty else { return }
        case .image:
            if let png = pasteboard.data(forType: .png) {
                input.image = png
            } else if let tiff = pasteboard.data(forType: .tiff) {
                input.image = tiff
                input.imageIsTIFF = true
            }
            guard input.image != nil else { return }
        case .rich:
            input.text = pasteboard.string(forType: .string)
            if hasHTML { input.html = pasteboard.data(forType: .html) }
            if hasRTF { input.rtf = pasteboard.data(forType: .rtf) }
        default:
            input.text = pasteboard.string(forType: .string)
        }
        dispatch(input)
    }

    /// Pulse's own Snip, Screen and Window screenshots (4): the history gets the picture even
    /// though Pulse's own pasteboard writes are skipped. The source is Pulse.
    public func noteScreenshot(_ image: NSImage) {
        guard settings.enabled, !Runtime.isUnderTest, isLoaded, writable, loadState == .ok,
              settings.captureImages else { return }
        let pasteboard = NSPasteboard.general
        // The poll must not take the same picture again.
        lastChangeCount = pasteboard.changeCount
        var input = ClipCaptureInput(kind: .image, at: Date().timeIntervalSince1970)
        input.source = Self.pulseApp
        input.forceScreenshot = true
        if let png = pasteboard.data(forType: .png) {
            input.image = png
        } else if let tiff = pasteboard.data(forType: .tiff) ?? image.tiffRepresentation {
            input.image = tiff
            input.imageIsTIFF = true
        }
        guard input.image != nil else { return }
        dispatch(input)
    }

    private func dispatch(_ input: ClipCaptureInput) {
        io.async {
            let prepared = ClipCapture.prepare(input)
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.finishPrepare(prepared) }
            }
        }
    }

    /// Back on the main thread with a prepared copy: the rapid-repeat guard, dedupe (3.4), the
    /// lane and stack (4); then the files are written off the main thread before the row is added.
    private func finishPrepare(_ prepared: ClipPrepared?) {
        guard let prepared, writable, loadState == .ok, settings.enabled, !Runtime.isUnderTest else { return }
        // 3.5 step 9: an identical digest within 300 ms is ignored (some apps write twice).
        if !prepared.forceScreenshot, prepared.digest == lastDigest, prepared.at - lastDigestAt < 0.3 { return }
        lastDigest = prepared.digest
        lastDigestAt = prepared.at

        if let index = items.firstIndex(where: { $0.digest == prepared.digest && $0.kind == prepared.kind }) {
            var item = items.remove(at: index)
            item.lastUsedAt = prepared.at
            if let source = prepared.source { item.sourceApp = source }
            if prepared.forceScreenshot, item.kind == .image, item.lane == .main, !item.isProtected,
               item.meta.laneLocked != true {
                item.lane = .screenshot
                item.meta.screenshot = true
                item.meta.stackId = stackID(at: item.createdAt, excluding: item.id)
            }
            items.insert(item, at: 0)
            persistIndex()
            scheduleChange()
            ClipBurst.shared.noteCopy(id: item.id, at: prepared.at)
            return
        }

        let key = prepared.kind.rawValue + "|" + prepared.digest
        guard inflight.insert(key).inserted else { return }
        var meta = prepared.meta
        var lane = ClipLane.main
        let id = UUID().uuidString.lowercased()
        if prepared.kind == .image {
            if isScreenshot(prepared) {
                lane = .screenshot
                meta.screenshot = true
                meta.stackId = stackID(at: prepared.at, excluding: id)
            } else {
                meta.screenshot = false
            }
        }
        let item = ClipItem(
            id: id, kind: prepared.kind, preview: prepared.preview, title: nil,
            sourceApp: prepared.source, createdAt: prepared.at, lastUsedAt: prepared.at,
            useCount: 0, pinned: false, boards: [], size: prepared.size, digest: prepared.digest,
            lane: lane, meta: meta, concealed: false)
        let startEpoch = epoch
        let dir = directory
        io.async {
            let thumb = ClipDisk.writePayload(prepared, id: id, dir: dir)
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    self?.finishWrite(item, key: key, thumb: thumb, startEpoch: startEpoch)
                }
            }
        }
    }

    private func finishWrite(_ item: ClipItem, key: String, thumb: Int?, startEpoch: Int) {
        inflight.remove(key)
        // nil: a file could not be written and nothing was left behind.
        guard let thumb else { return }
        guard startEpoch == epoch, writable, loadState == .ok else {
            let dir = directory
            let id = item.id
            io.async { ClipDisk.removePayload(id: id, dir: dir) }
            return
        }
        if thumb > 0 { thumbSizes[item.id] = thumb }
        items.insert(item, at: 0)
        persistIndex()
        updateProtectedFigure()
        scheduleChange()
        retentionPass()
        ClipBurst.shared.noteCopy(id: item.id, at: item.createdAt)
    }

    /// 4: a screenshot comes from a screenshot tool, is the size of a display, or was taken by
    /// Pulse's own tools.
    private func isScreenshot(_ prepared: ClipPrepared) -> Bool {
        if prepared.forceScreenshot { return true }
        if let id = prepared.source?.id,
           Self.screenshotSources.contains(id) || id == Self.pulseApp.id { return true }
        guard let width = prepared.meta.width, let height = prepared.meta.height else { return false }
        return NSScreen.screens.contains { screen in
            let pixels = screen.convertRectToBacking(screen.frame).size
            return abs(Int(pixels.width.rounded()) - width) <= 2 && abs(Int(pixels.height.rounded()) - height) <= 2
        }
    }

    /// 4: screenshots within 60 s of the one before share a `stackId`.
    private func stackID(at time: Double, excluding id: String) -> String {
        var latest: Int?
        for (index, item) in items.enumerated()
        where item.kind == .image && item.lane == .screenshot && item.id != id {
            if latest == nil || item.createdAt > items[latest ?? 0].createdAt { latest = index }
        }
        if let latest, time - items[latest].createdAt <= 60 {
            if let existing = items[latest].meta.stackId { return existing }
            let fresh = UUID().uuidString.lowercased()
            items[latest].meta.stackId = fresh
            return fresh
        }
        return UUID().uuidString.lowercased()
    }

    // MARK: - Source app (3.6)

    private func sourceApp(of pasteboard: NSPasteboard, types: Set<String>) -> ClipSourceApp? {
        let own = Bundle.main.bundleIdentifier
        var identifier: String?
        var name: String?
        if types.contains(Self.sourceType.rawValue) {
            // An empty string means unknown.
            let marked = pasteboard.string(forType: Self.sourceType) ?? ""
            identifier = marked.isEmpty ? nil : marked
        } else if let front = NSWorkspace.shared.frontmostApplication {
            identifier = front.bundleIdentifier
            name = front.localizedName
        }
        guard let identifier, !identifier.isEmpty, identifier != own else { return nil }
        return ClipSourceApp(id: identifier, name: name ?? displayName(for: identifier))
    }

    private func displayName(for identifier: String) -> String {
        if let cached = sourceNames[identifier] { return cached }
        var name = ""
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: identifier) {
            name = FileManager.default.displayName(atPath: url.path)
            if name.hasSuffix(".app") { name = String(name.dropLast(4)) }
        }
        if name.isEmpty { name = identifier.split(separator: ".").last.map(String.init) ?? identifier }
        sourceNames[identifier] = name
        return name
    }

    // MARK: - Using and editing items

    public func item(id: String) -> ClipItem? {
        items.first { $0.id == id }
    }

    /// 7.11.2 step 7: the item was pasted or copied from the strip or launcher.
    public func markUsed(_ id: String) {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        var next = items
        var item = next.remove(at: index)
        item.lastUsedAt = Date().timeIntervalSince1970
        item.useCount += 1
        next.insert(item, at: 0)
        commit(items: next)
    }

    public func setPinned(_ id: String, _ pinned: Bool) {
        guard let index = items.firstIndex(where: { $0.id == id }), items[index].pinned != pinned else { return }
        var next = items
        next[index].pinned = pinned
        commit(items: next)
        if !pinned { retentionPass() }
    }

    /// 4: the user moved an image between lanes; the lane sticks.
    public func setLane(_ id: String, screenshot: Bool) {
        guard let index = items.firstIndex(where: { $0.id == id }), items[index].kind == .image else { return }
        var next = items
        next[index].lane = screenshot ? .screenshot : .main
        next[index].meta.screenshot = screenshot
        next[index].meta.laneLocked = true
        commit(items: next)
        retentionPass()
    }

    /// 3.2: for a file item, refreshes `meta.anyMissing`. Cheap; writes the index only when the
    /// answer changed and never calls `onChange`.
    public func refreshMissing(for item: ClipItem) {
        guard item.kind == .file, let index = items.firstIndex(where: { $0.id == item.id }) else { return }
        let missing = files(for: item).contains { !FileManager.default.fileExists(atPath: $0) }
        guard items[index].meta.anyMissing != missing else { return }
        items[index].meta.anyMissing = missing
        persistIndex()
    }

    // MARK: - Deleting (9)

    /// Deletes at once and returns the 6 s undo, or nil when nothing was deleted.
    public func delete(_ ids: [String]) -> ClipUndo? {
        guard canMutate else { return nil }
        let chosen = Set(ids)
        let removed = items.filter { chosen.contains($0.id) }
        guard !removed.isEmpty else { return nil }
        var positions: [String: [String: Int]] = [:]
        var nextBoards = boards
        for board in nextBoards.indices {
            for item in removed {
                if let place = nextBoards[board].itemOrder.firstIndex(of: item.id) {
                    positions[item.id, default: [:]][nextBoards[board].id] = place
                }
            }
            nextBoards[board].itemOrder.removeAll { chosen.contains($0) }
        }
        let undo = ClipUndo(items: removed, boardPositions: positions)
        for item in removed { thumbSizes[item.id] = nil; thumbCache.removeObject(forKey: item.id as NSString) }
        commit(items: items.filter { !chosen.contains($0.id) }, boards: nextBoards)
        if persistent {
            let dir = directory
            let removedIDs = removed.map { $0.id }
            io.async { ClipDisk.stash(ids: removedIDs, dir: dir, undo: undo) }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.undoSeconds) { [weak self] in
            MainActor.assumeIsolated { self?.expire(undo) }
        }
        return undo
    }

    /// Puts the deleted items back, if their 6 s have not run out.
    public func undoDelete(_ undo: ClipUndo) {
        guard canMutate, undo.finish() else { return }
        let startEpoch = epoch
        let restoreOnMain: @Sendable () -> Void = { [weak self] in
            MainActor.assumeIsolated { self?.finishUndo(undo, startEpoch: startEpoch) }
        }
        if persistent {
            let dir = directory
            io.async {
                ClipDisk.restore(undo, dir: dir)
                DispatchQueue.main.async(execute: restoreOnMain)
            }
        } else {
            restoreOnMain()
        }
    }

    private func finishUndo(_ undo: ClipUndo, startEpoch: Int) {
        guard startEpoch == epoch, canMutate else {
            discardUndo(undo)
            return
        }
        var nextBoards = boards
        var restored: [ClipItem] = []
        for var item in undo.items where !items.contains(where: { $0.id == item.id }) {
            item.boards = item.boards.filter { id in nextBoards.contains { $0.id == id } }
            for board in nextBoards.indices where item.boards.contains(nextBoards[board].id) {
                let place = undo.boardPositions[item.id]?[nextBoards[board].id] ?? 0
                nextBoards[board].itemOrder.insert(item.id, at: min(place, nextBoards[board].itemOrder.count))
            }
            restored.append(item)
        }
        commit(items: (items + restored).sorted { $0.lastUsedAt > $1.lastUsedAt }, boards: nextBoards)
        undo.drop()
    }

    private func expire(_ undo: ClipUndo) {
        guard undo.finish() else { return }
        discardUndo(undo)
    }

    private func discardUndo(_ undo: ClipUndo) {
        if persistent {
            io.async { ClipDisk.discard(undo) }
        } else {
            undo.drop()
        }
    }

    /// 9: every payload, thumbnail and the index. Boards stay, emptied.
    public func clearAll() {
        guard canMutate else { return }
        epoch += 1
        thumbSizes = [:]
        thumbCache.removeAllObjects()
        var emptied = boards
        for board in emptied.indices { emptied[board].itemOrder = [] }
        commit(items: [], boards: emptied)
        if persistent {
            let dir = directory
            io.async { ClipDisk.removeEverything(dir: dir) }
        }
    }

    /// The unprotected items a window clears: copied at or after `since` when `newer`, else before it.
    private func windowIDs(since: Double, newer: Bool) -> [String] {
        items
            .filter { !$0.isProtected && (newer ? $0.createdAt >= since : $0.createdAt < since) }
            .map { $0.id }
    }

    /// How many items a clear of this scope removes now (the confirmation row says so).
    public func clearCount(_ scope: ClipClearScope, now: Double = Date().timeIntervalSince1970) -> Int {
        switch scope {
        case .all: return items.count
        case .lastHour: return windowIDs(since: now - 3600, newer: true).count
        case .olderThanDay: return windowIDs(since: now - 86_400, newer: false).count
        }
    }

    /// Runs a clear (docs/clipboard-spec.md 9). `.all` is `clearAll`. A window removes its unprotected
    /// items through `delete`, so their files go with them; pinned and board items stay, as the
    /// retention pass keeps them. Returns the number of items removed.
    @discardableResult
    public func clearScope(_ scope: ClipClearScope, now: Double = Date().timeIntervalSince1970) -> Int {
        switch scope {
        case .all:
            let count = items.count
            clearAll()
            return count
        case .lastHour, .olderThanDay:
            let ids = scope == .lastHour
                ? windowIDs(since: now - 3600, newer: true)
                : windowIDs(since: now - 86_400, newer: false)
            guard !ids.isEmpty else { return 0 }
            _ = delete(ids)
            return ids.count
        }
    }

    // MARK: - Retention (4)

    /// Plans on the background queue, applies on the main thread. Runs at launch, after each
    /// capture, hourly and when a cap changes.
    public func retentionPass() {
        guard settings.enabled, writable, !Runtime.isUnderTest else { return }
        guard !retentionRunning else {
            retentionAgain = true
            return
        }
        retentionRunning = true
        let snapshot = items
        let thumbs = thumbSizes
        let current = settings
        let now = Date().timeIntervalSince1970
        io.async {
            let plan = ClipRetention.plan(items: snapshot, thumbs: thumbs, settings: current, now: now)
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.applyRetention(plan) }
            }
        }
    }

    private func applyRetention(_ plan: ClipRetention.Plan) {
        retentionRunning = false
        // An item pinned or put on a board since the plan was made stays.
        let gone = items.filter { plan.evict.contains($0.id) && !$0.isProtected }.map { $0.id }
        if !gone.isEmpty {
            let drop = Set(gone)
            for id in gone { thumbSizes[id] = nil; thumbCache.removeObject(forKey: id as NSString) }
            commit(items: items.filter { !drop.contains($0.id) })
            let dir = directory
            io.async { for id in gone { ClipDisk.removePayload(id: id, dir: dir) } }
        }
        protectedOverCapBytes = plan.protectedOverCap
        if gone.isEmpty { scheduleChange() }
        if retentionAgain {
            retentionAgain = false
            retentionPass()
        }
    }
}

/// The newest value waiting to be written: later writes replace earlier ones that have not
/// run yet, so a burst of changes costs one file write.
final class ClipPending<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value?

    func set(_ next: Value) {
        lock.lock(); defer { lock.unlock() }
        value = next
    }

    func take() -> Value? {
        lock.lock(); defer { lock.unlock() }
        let found = value
        value = nil
        return found
    }
}
#endif
