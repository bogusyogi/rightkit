import Foundation
import ImageIO

// The clipboard folder on disk (docs/clipboard-spec.md 2.5): layout, permissions, atomic
// writes, load with repair, migration of the old version 0 index, and deleting. Everything
// here runs on the store's background queue, never on the main thread. No path, text or
// preview is ever logged.

/// What a load found. Plain values, so it crosses back to the main thread.
struct ClipLoaded: Sendable {
    var items: [ClipItem] = []
    var boards: [ClipBoard] = []
    var state: ClipLoadState = .ok
    var lockFD: Int32 = -1
    var thumbSizes: [String: Int] = [:]
    var appColours: [String: String] = [:]
}

/// 9: the in-memory copy of deleted items that makes the 6 s undo. Payload bytes sit here
/// (images over 5 MB sit in `clipboard/.undo/` instead).
final class ClipUndo: @unchecked Sendable {
    let items: [ClipItem]
    /// Per item, per board id: where the item stood in that board's order.
    let boardPositions: [String: [String: Int]]
    private let lock = NSLock()
    private var payloads: [String: [String: Data]] = [:]
    private var moved: [String: URL] = [:]
    private var finished = false

    init(items: [ClipItem], boardPositions: [String: [String: Int]]) {
        self.items = items
        self.boardPositions = boardPositions
    }

    var count: Int { items.count }

    /// False once restored or expired.
    var isActive: Bool {
        lock.lock(); defer { lock.unlock() }
        return !finished
    }

    /// Marks the undo used; false when it already was.
    func finish() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if finished { return false }
        finished = true
        return true
    }

    func store(id: String, ext: String, data: Data) {
        lock.lock(); defer { lock.unlock() }
        payloads[id, default: [:]][ext] = data
    }

    func store(id: String, movedTo url: URL) {
        lock.lock(); defer { lock.unlock() }
        moved[id] = url
    }

    func take(id: String) -> (payload: [String: Data], moved: URL?) {
        lock.lock(); defer { lock.unlock() }
        return (payloads[id] ?? [:], moved[id])
    }

    func movedFiles() -> [URL] {
        lock.lock(); defer { lock.unlock() }
        return Array(moved.values)
    }

    func drop() {
        lock.lock(); defer { lock.unlock() }
        payloads = [:]
        moved = [:]
    }
}

enum ClipDisk {
    static let payloadExtensions = ["txt", "png", "html", "rtf", "files.json"]
    static let undoDiskThreshold = 5 * 1024 * 1024

    // MARK: Paths

    static func itemsDir(_ dir: URL) -> URL { dir.appendingPathComponent("items", isDirectory: true) }
    static func thumbsDir(_ dir: URL) -> URL { dir.appendingPathComponent("thumbs", isDirectory: true) }
    static func undoDir(_ dir: URL) -> URL { dir.appendingPathComponent(".undo", isDirectory: true) }
    static func indexURL(_ dir: URL) -> URL { dir.appendingPathComponent("index.json") }
    static func boardsURL(_ dir: URL) -> URL { dir.appendingPathComponent("boards.json") }
    static func coloursURL(_ dir: URL) -> URL { dir.appendingPathComponent("appcolours.json") }

    static func payloadURL(_ dir: URL, _ id: String, _ ext: String) -> URL {
        itemsDir(dir).appendingPathComponent(id + "." + ext)
    }

    static func thumbURL(_ dir: URL, _ id: String) -> URL {
        thumbsDir(dir).appendingPathComponent(id + ".png")
    }

    /// The file a row cannot live without (2.5).
    static func requiredExtension(_ kind: ClipKind) -> String {
        switch kind {
        case .text, .link, .colour, .code, .rich: return "txt"
        case .image: return "png"
        case .file: return "files.json"
        }
    }

    // MARK: Writing

    static func makeDirectory(_ url: URL) {
        try? FileManager.default.createDirectory(
            at: url, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
    }

    static func ensureLayout(_ dir: URL) {
        makeDirectory(dir)
        makeDirectory(itemsDir(dir))
        makeDirectory(thumbsDir(dir))
    }

    /// Atomic (temp file, then rename), then 0600; sealed when encryption is on (ClipSeal, #152).
    static func write(_ data: Data, to url: URL) throws {
        try ClipSeal.write(data, to: url)
    }

    /// False when the index could not be written.
    @discardableResult
    static func writeIndex(_ items: [ClipItem], writer: String, dir: URL) -> Bool {
        (try? write(ClipIndexFile.encode(items, writer: writer), to: indexURL(dir))) != nil
    }

    static func writeBoards(_ boards: [ClipBoard], dir: URL) {
        try? write(ClipBoardsFile.encode(boards), to: boardsURL(dir))
    }

    static func writeColours(_ colours: [String: String], dir: URL) {
        var map: [String: ClipJSON] = [:]
        for (key, value) in colours { map[key] = .string(value) }
        try? write(Data(ClipJSONWriter.write(.object(map)).utf8), to: coloursURL(dir))
    }

    /// Writes a new item's files, payload first (2.5). Returns the thumbnail's size in bytes
    /// (0 when the kind has none), or nil when a file could not be written (nothing is left).
    static func writePayload(_ prepared: ClipPrepared, id: String, dir: URL) -> Int? {
        ensureLayout(dir)
        var written: [URL] = []
        for part in prepared.payload {
            let url = payloadURL(dir, id, part.ext)
            do {
                try write(part.data, to: url)
                written.append(url)
            } catch {
                for file in written { try? FileManager.default.removeItem(at: file) }
                return nil
            }
        }
        guard prepared.kind == .image,
              let png = prepared.payload.first(where: { $0.ext == "png" })?.data else { return 0 }
        return writeThumbnail(png: png, id: id, dir: dir) ?? 0
    }

    /// The 320 px thumbnail (3.2); its size, or nil when it could not be made.
    static func writeThumbnail(png: Data, id: String, dir: URL) -> Int? {
        guard let thumb = ClipImage.thumbnailPNG(from: png) else { return nil }
        makeDirectory(thumbsDir(dir))
        do {
            try write(thumb, to: thumbURL(dir, id))
            return thumb.count
        } catch {
            return nil
        }
    }

    // MARK: Deleting

    static func removePayload(id: String, dir: URL) {
        let manager = FileManager.default
        for ext in payloadExtensions { try? manager.removeItem(at: payloadURL(dir, id, ext)) }
        try? manager.removeItem(at: thumbURL(dir, id))
    }

    static func removeEverything(dir: URL) {
        let manager = FileManager.default
        for folder in [itemsDir(dir), thumbsDir(dir), undoDir(dir)] {
            for entry in (try? manager.contentsOfDirectory(atPath: folder.path)) ?? [] {
                try? manager.removeItem(at: folder.appendingPathComponent(entry))
            }
        }
    }

    /// Deletes the files of `ids` at once and keeps their bytes for the undo.
    static func stash(ids: [String], dir: URL, undo: ClipUndo) {
        let manager = FileManager.default
        for id in ids {
            for ext in payloadExtensions {
                let url = payloadURL(dir, id, ext)
                guard manager.fileExists(atPath: url.path) else { continue }
                let size = (try? manager.attributesOfItem(atPath: url.path)[.size] as? Int) ?? 0
                if ext == "png", size > undoDiskThreshold {
                    makeDirectory(undoDir(dir))
                    let target = undoDir(dir).appendingPathComponent(id + ".png")
                    try? manager.removeItem(at: target)
                    if (try? manager.moveItem(at: url, to: target)) != nil {
                        chmod(target.path, 0o600)
                        undo.store(id: id, movedTo: target)
                        continue
                    }
                }
                if let data = ClipSeal.read(url) { undo.store(id: id, ext: ext, data: data) }
                try? manager.removeItem(at: url)
            }
            try? manager.removeItem(at: thumbURL(dir, id))
        }
    }

    /// Puts the undo's files back (the thumbnails are rebuilt lazily).
    static func restore(_ undo: ClipUndo, dir: URL) {
        ensureLayout(dir)
        for item in undo.items {
            let kept = undo.take(id: item.id)
            for (ext, data) in kept.payload { try? write(data, to: payloadURL(dir, item.id, ext)) }
            if let file = kept.moved {
                let target = payloadURL(dir, item.id, "png")
                try? FileManager.default.removeItem(at: target)
                if (try? FileManager.default.moveItem(at: file, to: target)) != nil { chmod(target.path, 0o600) }
            }
        }
    }

    static func discard(_ undo: ClipUndo) {
        for file in undo.movedFiles() { try? FileManager.default.removeItem(at: file) }
        undo.drop()
    }

    // MARK: Loading

    /// 2.5 and 9: take the lock, read the index and boards, drop rows with no payload, delete
    /// orphans, repair the boards. Writes only when the folder is ours.
    static func load(dir: URL, writer: String) -> ClipLoaded {
        var loaded = ClipLoaded()
        ensureLayout(dir)
        let manager = FileManager.default

        // The lock lives as long as the process (2.5): the descriptor is never closed.
        var lockedElsewhere = false
        let fd = open(dir.appendingPathComponent(".lock").path, O_CREAT | O_RDWR, 0o600)
        if fd >= 0 {
            if flock(fd, LOCK_EX | LOCK_NB) == 0 {
                loaded.lockFD = fd
            } else {
                close(fd)
                lockedElsewhere = true
            }
        } else {
            // The lock file cannot be opened: read only, and never write.
            lockedElsewhere = true
        }

        // The key comes first: sealed files cannot be read without it (spec 9, #152). A process
        // that does not own the folder only reads the key.
        let prepared = ClipSeal.prepare(dir: dir, owner: !lockedElsewhere)
        if prepared == .unavailable && !lockedElsewhere {
            loaded.state = .unreadable
            return loaded
        }

        var items: [ClipItem] = []
        var rewriteIndex = false
        var state: ClipLoadState = .ok
        if let data = ClipSeal.read(indexURL(dir)) {
            switch ClipIndexFile.decode(data) {
            case .ok(let rows):
                items = rows
            case .newer:
                state = .newer
            case .unreadable:
                state = .unreadable
            }
        } else if manager.fileExists(atPath: indexURL(dir).path) {
            state = .unreadable
        }

        var boards: [ClipBoard] = []
        if state == .ok || state == .lockedElsewhere, let data = ClipSeal.read(boardsURL(dir)) {
            switch ClipBoardsFile.decode(data) {
            case .ok(let rows): boards = rows
            case .newer: state = .newer
            case .unreadable: state = .unreadable
            }
        }
        if state == .ok, lockedElsewhere { state = .lockedElsewhere }
        if state != .ok, state != .lockedElsewhere {
            // Nothing is shown and nothing is written: the index stays as it is.
            loaded.state = state
            return loaded
        }

        // Rows whose payload is gone are dropped.
        let before = items.count
        items = items.filter {
            manager.fileExists(atPath: payloadURL(dir, $0.id, requiredExtension($0.kind)).path)
        }
        if items.count != before { rewriteIndex = true }
        items.sort { $0.lastUsedAt > $1.lastUsedAt }

        let repaired = repair(boards: boards, items: items)
        if repaired.changed { rewriteIndex = rewriteIndex || repaired.itemsChanged }
        boards = repaired.boards
        items = repaired.items

        for item in items where item.kind == .image {
            let size = (try? manager.attributesOfItem(atPath: thumbURL(dir, item.id).path)[.size] as? Int) ?? 0
            if size > 0 { loaded.thumbSizes[item.id] = size }
        }
        if let data = ClipSeal.read(coloursURL(dir)), let map = ClipJSON.parse(data)?.objectValue {
            for (key, value) in map {
                if let hex = value.stringValue { loaded.appColours[key] = hex }
            }
        }

        if state == .ok {
            if rewriteIndex { writeIndex(items, writer: writer, dir: dir) }
            if repaired.changed { writeBoards(boards, dir: dir) }
            deleteOrphans(items: items, dir: dir)
            // An undo that outlived its launch is gone (9).
            for entry in (try? manager.contentsOfDirectory(atPath: undoDir(dir).path)) ?? [] {
                try? manager.removeItem(at: undoDir(dir).appendingPathComponent(entry))
            }
        }
        loaded.items = items
        loaded.boards = boards
        loaded.state = state
        return loaded
    }

    private static func deleteOrphans(items: [ClipItem], dir: URL) {
        let manager = FileManager.default
        let ids = Set(items.map { $0.id })
        for entry in (try? manager.contentsOfDirectory(atPath: itemsDir(dir).path)) ?? [] {
            guard !entry.hasPrefix("."), let dot = entry.firstIndex(of: ".") else { continue }
            if !ids.contains(String(entry[..<dot])) {
                try? manager.removeItem(at: itemsDir(dir).appendingPathComponent(entry))
            }
        }
        let images = Set(items.filter { $0.kind == .image }.map { $0.id })
        for entry in (try? manager.contentsOfDirectory(atPath: thumbsDir(dir).path)) ?? [] {
            guard !entry.hasPrefix("."), entry.hasSuffix(".png") else { continue }
            if !images.contains(String(entry.dropLast(4))) {
                try? manager.removeItem(at: thumbsDir(dir).appendingPathComponent(entry))
            }
        }
    }

    /// 6: membership comes from the items; `itemOrder` is only the order. Unknown ids are
    /// dropped, members missing from the order follow it by `lastUsedAt`, orders are 0...n.
    static func repair(boards: [ClipBoard], items: [ClipItem])
        -> (boards: [ClipBoard], items: [ClipItem], changed: Bool, itemsChanged: Bool) {
        var changed = false
        var itemsChanged = false
        var sorted = boards.sorted { $0.order < $1.order }
        for index in sorted.indices where sorted[index].order != index {
            sorted[index].order = index
            changed = true
        }
        let boardIDs = Set(sorted.map { $0.id })
        var fixed = items
        for index in fixed.indices {
            let kept = fixed[index].boards.filter { boardIDs.contains($0) }
            if kept != fixed[index].boards {
                fixed[index].boards = kept
                itemsChanged = true
                changed = true
            }
        }
        let byID = Dictionary(fixed.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        for index in sorted.indices {
            let boardID = sorted[index].id
            var order: [String] = []
            var seen = Set<String>()
            for id in sorted[index].itemOrder {
                if let item = byID[id], item.boards.contains(boardID), seen.insert(id).inserted { order.append(id) }
            }
            let missing = fixed.filter { $0.boards.contains(boardID) && !seen.contains($0.id) }
                .sorted { $0.lastUsedAt > $1.lastUsedAt }
            order += missing.map { $0.id }
            if order != sorted[index].itemOrder {
                sorted[index].itemOrder = order
                changed = true
            }
        }
        return (sorted, fixed, changed, itemsChanged)
    }
}

// MARK: - Images

/// ImageIO helpers: they read sizes and make thumbnails without decoding a whole picture
/// where the format allows it, and are safe off the main thread.
enum ClipImage {
    static func dimensions(of data: Data) -> (width: Int, height: Int)? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0,
              let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = props[kCGImagePropertyPixelWidth] as? Int,
              let height = props[kCGImagePropertyPixelHeight] as? Int else { return nil }
        return (width, height)
    }

    /// Any image format as PNG bytes.
    static func png(fromImageData data: Data) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { return nil }
        return encode(image)
    }

    /// A PNG whose longest side is `maxSide` pixels.
    static func thumbnailPNG(from data: Data, maxSide: Int = 320) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maxSide,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return encode(image)
    }

    private static func encode(_ image: CGImage) -> Data? {
        let out = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(out, "public.png" as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return out as Data
    }
}
