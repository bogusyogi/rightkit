#if canImport(CoreText)
import CoreText
import Foundation

/// Process-scope registration of font files an app ships in its bundle. Registration is idempotent
/// per file name, so a host may call it from several launch paths.
public enum RightKitFonts {
    private static let registered = RegisteredNames()

    private final class RegisteredNames: @unchecked Sendable {
        private let lock = NSLock()
        private var names = Set<String>()

        /// True the first time `name` is claimed.
        func claim(_ name: String) -> Bool {
            lock.lock()
            defer { lock.unlock() }
            return names.insert(name).inserted
        }
    }

    /// Registers each file in `names` (for example `"Body-400.ttf"`) with `CTFontManager` for this
    /// process. Each name is looked up in `<bundle resources>/fonts`, then in every `additionalRoots`
    /// directory in order (a development checkout, say), and the first match wins. A name that is not
    /// found is skipped and tried again on the next call; a name already registered is not registered
    /// twice.
    public static func register(names: [String], in bundle: Bundle, additionalRoots: [URL] = []) {
        var roots: [URL] = []
        if let resources = bundle.resourceURL {
            roots.append(resources.appendingPathComponent("fonts", isDirectory: true))
        }
        roots.append(contentsOf: additionalRoots)
        let fileManager = FileManager.default
        for name in names {
            guard let url = roots.lazy.map({ $0.appendingPathComponent(name) })
                .first(where: { fileManager.fileExists(atPath: $0.path) }) else { continue }
            guard registered.claim(name) else { continue }
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }
}
#endif
