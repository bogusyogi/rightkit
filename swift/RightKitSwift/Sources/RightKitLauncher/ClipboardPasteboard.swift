#if os(macOS)
import RightKitNotchCore
import AppKit
import Foundation

// Putting an item back on the pasteboard (docs/clipboard-spec.md 7.11.2 steps 2 and 3).
// The strip and the launcher call `write`, then send the paste keystroke themselves.

extension LauncherClipboard {
    /// Step 2: every format the item has, in the order the original had (rich: HTML, RTF, then
    /// plain text); `plain` offers only the text format: rich gives its plain text, a colour its
    /// original string, a link its URL, files their paths one per line. An image has no plain
    /// form, so it is written as it is. Step 3: marked as Pulse's own (`org.nspasteboard.source`
    /// and `com.pulse.clipboard.own`; never ConcealedType or TransientType), then `noteOwnWrite()`.
    /// False when the payload could not be read; the pasteboard is then left as it was.
    @discardableResult
    func write(_ item: ClipItem, plain: Bool) -> Bool {
        // The pasteboard is real user data: nothing writes it under test.
        if Runtime.isUnderTest { return false }
        let primary = NSPasteboardItem()
        var entries: [NSPasteboardItem] = [primary]

        if plain, item.kind != .image {
            guard let body = plainText(for: item) else { return false }
            primary.setString(body, forType: .string)
        } else {
            switch item.kind {
            case .text, .link, .colour, .code:
                guard let body = self.text(for: item) else { return false }
                primary.setString(body, forType: .string)
            case .rich:
                guard let body = self.text(for: item) else { return false }
                if let html = payload(of: item, "html") { primary.setData(html, forType: .html) }
                if let rtf = payload(of: item, "rtf") { primary.setData(rtf, forType: .rtf) }
                primary.setString(body, forType: .string)
            case .image:
                guard let png = imageData(for: item) else { return false }
                primary.setData(png, forType: .png)
            case .file:
                let paths = files(for: item)
                guard let first = paths.first else { return false }
                primary.setString(URL(fileURLWithPath: first).absoluteString, forType: .fileURL)
                for path in paths.dropFirst() {
                    let extra = NSPasteboardItem()
                    extra.setString(URL(fileURLWithPath: path).absoluteString, forType: .fileURL)
                    entries.append(extra)
                }
            }
        }
        primary.setString(Bundle.main.bundleIdentifier ?? Self.pulseApp.id, forType: Self.sourceType)
        primary.setString("1", forType: Self.ownType)

        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.writeObjects(entries) else { return false }
        noteOwnWrite()
        return true
    }

    /// The launcher's old `copy`: the item as it was copied.
    func copy(_ item: ClipItem) {
        write(item, plain: false)
    }

    /// 7.11.2 step 2, "as plain text".
    private func plainText(for item: ClipItem) -> String? {
        switch item.kind {
        case .text, .code, .rich:
            return text(for: item)
        case .colour:
            return item.meta.original ?? text(for: item)
        case .link:
            return item.preview.isEmpty ? text(for: item)?.trimmingCharacters(in: .whitespacesAndNewlines) : item.preview
        case .file:
            let paths = files(for: item)
            return paths.isEmpty ? nil : paths.joined(separator: "\n")
        case .image:
            return nil
        }
    }
}

// Restore after paste (docs/clipboard-spec.md 7.11.2 step 6, setting `clipboardRestoreAfterPaste`).
// Same rules as the snippet paste's save and restore: nothing is kept when the clipboard cannot
// be kept whole, holds a concealed item, or is too large; and the old clipboard only goes back
// if nothing else was copied meanwhile.
enum ClipboardRestore {
    typealias Entry = [(type: NSPasteboard.PasteboardType, data: Data)]

    private static let maxBytes = 32 * 1024 * 1024
    private static let concealedType = NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType")
    static let delaySeconds: TimeInterval = 1.5

    /// The general pasteboard as it is now, every type of every item, or nil when it cannot be
    /// kept whole.
    @MainActor
    static func snapshot() -> [Entry]? {
        let board = NSPasteboard.general
        if board.types?.contains(concealedType) == true { return nil }
        var total = 0
        var items: [Entry] = []
        for item in board.pasteboardItems ?? [] {
            if item.types.contains(concealedType) { return nil }
            var entry: Entry = []
            for type in item.types {
                guard let data = item.data(forType: type) else { return nil }
                total += data.count
                if total > maxBytes { return nil }
                entry.append((type: type, data: data))
            }
            items.append(entry)
        }
        return items
    }

    /// Puts `saved` back after `delaySeconds`, only while the pasteboard still has the change count
    /// `written` (what Pulse wrote for the paste). Pulse's own history is told, so it is not stored again.
    @MainActor
    static func restore(_ saved: [Entry], ifChangeCount written: Int) {
        DispatchQueue.main.asyncAfter(deadline: .now() + delaySeconds) {
            MainActor.assumeIsolated {
                let board = NSPasteboard.general
                guard board.changeCount == written else { return }
                board.clearContents()
                var items: [NSPasteboardItem] = []
                for entry in saved {
                    let item = NSPasteboardItem()
                    for part in entry { item.setData(part.data, forType: part.type) }
                    items.append(item)
                }
                if !items.isEmpty { _ = board.writeObjects(items) }
                LauncherClipboard.current?.noteOwnWrite()
            }
        }
    }
}
#endif
