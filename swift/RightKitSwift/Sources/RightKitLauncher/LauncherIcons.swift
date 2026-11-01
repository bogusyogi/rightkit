#if os(macOS)
import AppKit

/// Icons for the launcher's rows, kept as realised `NSImage`s so drawing a row never waits on
/// IconServices. The rows an empty search shows are loaded off the main thread ahead of the
/// hotkey (`LauncherModel.warmDefaultRows`, and again after each hide). A row whose icon is not
/// there yet never loads it on the main thread: it draws its symbol, the icon loads on the
/// background queue, and `onLoad` tells the model to redraw once an icon has landed. An entry
/// older than `maxAge` is served as it is and reloaded in the background, so an app's new icon
/// shows up without a restart.
final class LauncherIconCache: @unchecked Sendable {
    static let shared = LauncherIconCache()

    private final class Entry {
        let image: NSImage
        let loaded = Date()
        init(_ image: NSImage) { self.image = image }
    }

    private static let maxAge: TimeInterval = 600
    private let cache = NSCache<NSString, Entry>()
    private let lock = NSLock()
    private var pending = Set<String>()
    private let queue = DispatchQueue(label: "dev.orthic.pulse.launcher.icons", qos: .userInitiated)
    /// Called on the main thread after a background load put new icons in the cache.
    var onLoad: (() -> Void)?

    private init() {
        cache.countLimit = 512
    }

    /// The cached icon for a file, or nil while it loads in the background (see `onLoad`).
    func icon(forFile path: String) -> NSImage? {
        let key = path as NSString
        if let entry = cache.object(forKey: key) {
            if Date().timeIntervalSince(entry.loaded) > Self.maxAge { preload([path]) }
            return entry.image
        }
        preload([path])
        return nil
    }

    /// Loads the icons that are missing or old on a background queue. `done` runs on the main
    /// thread when they are in the cache (at once when there was nothing to load).
    func preload(_ paths: [String], then done: (() -> Void)? = nil) {
        let now = Date()
        let wanted = Set(paths).filter { path in
            guard let entry = cache.object(forKey: path as NSString) else { return true }
            return now.timeIntervalSince(entry.loaded) > Self.maxAge
        }
        lock.lock()
        let fresh = wanted.filter { pending.insert($0).inserted }
        lock.unlock()
        guard !fresh.isEmpty else {
            if let done { DispatchQueue.main.async { done() } }
            return
        }
        queue.async { [self] in
            for path in fresh {
                let image = Self.load(path)
                cache.setObject(Entry(image), forKey: path as NSString)
                lock.lock()
                pending.remove(path)
                lock.unlock()
            }
            // Only a load that found no icon waiting skips this: the rows that drew a symbol redraw with the icon.
            DispatchQueue.main.async { [self] in
                if !fresh.isEmpty { onLoad?() }
                if let done { done() }
            }
        }
    }

    /// The workspace icon with its bitmap for a row's size already decoded, so the first draw is a copy.
    private static func load(_ path: String) -> NSImage {
        let image = NSWorkspace.shared.icon(forFile: path)
        var rect = NSRect(x: 0, y: 0, width: 56, height: 56)
        _ = image.cgImage(forProposedRect: &rect, context: nil, hints: nil)
        return image
    }
}
#endif
