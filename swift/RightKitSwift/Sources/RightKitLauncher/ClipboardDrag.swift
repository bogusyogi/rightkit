#if os(macOS)
import AppKit
import UniformTypeIdentifiers

// docs/clipboard-spec.md 7.11.1: dragging a card out of the strip into another app. Text, link,
// colour and code go as text (a link also as a URL); rich as HTML plus plain text; an image as
// a PNG file promise (`Clipboard <date> <time>.png`) plus image data; files as their file URLs.
// Every item also carries Pulse's own type with its id, so a board tab can take it.

extension NSPasteboard.PasteboardType {
    /// The id of the dragged item, for the strip's own drop targets.
    static let clipItem = NSPasteboard.PasteboardType("com.pulse.clipboard.item")
}

/// The promise for an image's file: written to wherever the drop asks, from bytes held in memory.
final class ClipFilePromise: NSObject, NSFilePromiseProviderDelegate, @unchecked Sendable {
    private let data: Data
    private let name: String

    init(data: Data, date: Date) {
        self.data = data
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd 'at' HH.mm.ss"
        self.name = "Clipboard \(formatter.string(from: date)).png"
        super.init()
    }

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        name
    }

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, writePromiseTo url: URL,
                             completionHandler: @escaping (Error?) -> Void) {
        do {
            try data.write(to: url, options: .atomic)
            completionHandler(nil)
        } catch {
            completionHandler(error)
        }
    }
}

/// A file promise that also offers the PNG data and the strip's own type.
final class ClipImageDragWriter: NSFilePromiseProvider {
    var png = Data()
    var itemID = ""

    override func writableTypes(for pasteboard: NSPasteboard) -> [NSPasteboard.PasteboardType] {
        super.writableTypes(for: pasteboard) + [.png, .clipItem]
    }

    override func pasteboardPropertyList(forType type: NSPasteboard.PasteboardType) -> Any? {
        switch type {
        case .png: return png
        case .clipItem: return Data(itemID.utf8)
        default: return super.pasteboardPropertyList(forType: type)
        }
    }
}

/// Starts the drag and reports where it ended.
@MainActor
final class ClipboardDragSource: NSObject, NSDraggingSource {
    /// The drag ended on a target that took it.
    var onAccepted: ((NSPoint) -> Void)?
    private var promise: ClipFilePromise?

    /// The dragging items for one stored item, or none when its payload is gone.
    func items(for item: ClipItem, store: LauncherClipboard, preview: NSImage?) -> [NSDraggingItem] {
        func pasteboardItem() -> NSPasteboardItem {
            let entry = NSPasteboardItem()
            entry.setData(Data(item.id.utf8), forType: .clipItem)
            return entry
        }
        let frame = NSRect(x: 0, y: 0, width: 160, height: 90)
        let image = preview ?? Self.badge(for: item)
        func dragging(_ writer: NSPasteboardWriting) -> NSDraggingItem {
            let entry = NSDraggingItem(pasteboardWriter: writer)
            entry.setDraggingFrame(frame, contents: image)
            return entry
        }
        switch item.kind {
        case .text, .code, .colour:
            guard let text = store.text(for: item) else { return [] }
            let entry = pasteboardItem()
            entry.setString(item.kind == .colour ? (item.meta.original ?? text) : text, forType: .string)
            return [dragging(entry)]
        case .link:
            guard let text = store.text(for: item) else { return [] }
            let entry = pasteboardItem()
            entry.setString(text, forType: .string)
            entry.setString(text, forType: .URL)
            return [dragging(entry)]
        case .rich:
            guard let text = store.text(for: item) else { return [] }
            let entry = pasteboardItem()
            entry.setString(text, forType: .string)
            if let html = store.html(for: item) { entry.setData(html, forType: .html) }
            return [dragging(entry)]
        case .image:
            guard let png = store.pngData(for: item) else { return [] }
            let promise = ClipFilePromise(data: png, date: Date(timeIntervalSince1970: item.createdAt))
            self.promise = promise
            let writer = ClipImageDragWriter(fileType: UTType.png.identifier, delegate: promise)
            writer.png = png
            writer.itemID = item.id
            return [dragging(writer)]
        case .file:
            let paths = store.files(for: item)
            guard !paths.isEmpty else { return [] }
            return paths.enumerated().map { index, path in
                let entry = index == 0 ? pasteboardItem() : NSPasteboardItem()
                entry.setString(URL(fileURLWithPath: path).absoluteString, forType: .fileURL)
                return dragging(entry)
            }
        }
    }

    /// A small stand-in picture when the item has no thumbnail: its first line on a rounded tile.
    private static func badge(for item: ClipItem) -> NSImage {
        let size = NSSize(width: 160, height: 90)
        return NSImage(size: size, flipped: false) { rect in
            NSColor(white: 0.12, alpha: 0.95).setFill()
            NSBezierPath(roundedRect: rect, xRadius: 10, yRadius: 10).fill()
            let text = item.kind == .image ? item.kind.title : item.firstLine
            (text as NSString).draw(in: rect.insetBy(dx: 10, dy: 10), withAttributes: [
                .font: NSFont.systemFont(ofSize: 11),
                .foregroundColor: NSColor(white: 1, alpha: 0.88),
            ])
            return true
        }
    }

    // MARK: NSDraggingSource

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .outsideApplication ? [.copy, .generic] : [.copy, .generic, .move]
    }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        promise = nil
        if operation != [] { onAccepted?(screenPoint) }
    }
}
#endif
