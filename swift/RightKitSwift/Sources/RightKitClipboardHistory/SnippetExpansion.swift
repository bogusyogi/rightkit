#if os(macOS)
import RightKitDesktopTweaks
import RightKitLauncher
import AppKit
import ApplicationServices
import Carbon.HIToolbox
import os
import RightKitAccessibility

/// Pulse: typing a snippet's keyword anywhere replaces it with the snippet.
///
/// Trigger rule: only a snippet whose keyword is 2 to 32 characters, starts
/// with a symbol (not a letter or digit) and holds no space or control
/// character expands, for example `;sig` or `/addr`. It expands the moment the
/// last character is typed, and only when the character before it is not a
/// letter or digit, so it cannot fire inside an ordinary word. Keywords are
/// matched exactly, case included. A keyword that begins another, longer
/// keyword is ignored (it would block the longer one). Snippets that use
/// {argument} never expand: nothing can supply the argument.
///
/// Privacy: a rolling buffer of at most the longest keyword's length (never
/// more than 32 characters) lives in memory only. It is cleared on any click,
/// any key that is not a plain character (arrows, Escape, Return, Tab, Command
/// or Control or Option or Fn combinations), an app switch, and 5 seconds after
/// the last buffered key (a timer, not the next key); it is never written,
/// logged or sent. No buffer is kept at all while the switch is off, no keyword
/// exists, Accessibility is missing, secure input is on (password fields), a
/// Pulse window (the launcher included) is key, the keyboard input source is
/// not a plain layout (input methods), or the key came from another process.
/// The handler never swallows a key.
///
/// A keyword fires only when the character before it is known to be a
/// non-letter, non-digit: a character typed in the buffer, or the very start of
/// fresh typing after Return, Tab, a click or an app switch. After a timeout
/// clear the character before is unknown, so nothing fires until one is typed.
///
/// Replacing: shortly after the last character, if no other key came in, and
/// only when a text field (never a password field) is focused in the same app,
/// it posts one Backspace per character of the keyword, puts the snippet on the
/// pasteboard as one item marked transient, and posts Command-V after checking
/// everything again. It does nothing when the clipboard holds a concealed
/// (password manager) item or cannot be read fully. Later it puts the user's
/// previous pasteboard items back, unless the pasteboard changed in the
/// meantime. Posted events carry `marker`, so this handler ignores them.
final class SnippetExpansion {
    private static let marker: Int64 = 0x536E_6970  // "Snip", on posted events
    private static let hardCap = 32
    private static let idleSeconds: TimeInterval = 5
    private static let restoreDelay: TimeInterval = 1.5
    private static let maxSavedBytes = 32 * 1024 * 1024
    private static let transientType = NSPasteboard.PasteboardType("org.nspasteboard.TransientType")
    private static let concealedType = NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType")
    private static let log = Logger(subsystem: "dev.orthic.pulse", category: "snippets")

    private typealias SavedItem = [(type: NSPasteboard.PasteboardType, data: Data)]

    // Whether a Pulse window is key. Two sources, both read from the tap thread.
    private static let keyLock = NSLock()
    private static var launcherKey = false
    private static var windowKey = false

    /// The launcher panel is non-activating, so it is key while Pulse is not
    /// frontmost; it reports itself here when it shows and hides.
    static func setLauncherKey(_ on: Bool) {
        keyLock.lock(); launcherKey = on; keyLock.unlock()
    }

    private static func setWindowKey(_ on: Bool) {
        keyLock.lock(); windowKey = on; keyLock.unlock()
    }

    private static var pulseWindowIsKey: Bool {
        keyLock.lock(); defer { keyLock.unlock() }
        return launcherKey || windowKey
    }

    /// Turns a keyword into the text to insert. Set once, on the main actor.
    var resolve: (@MainActor (String) -> String?)?

    private let lock = NSLock()
    private var keywords: [[Character]] = []
    private var cap = 0
    private var active = false
    private var buffer: [Character] = []
    /// Whether the character just before `buffer` is known not to be a letter
    /// or digit (fresh typing after Return, Tab, a click or an app switch).
    private var boundaryKnown = false
    private var lastKey = Date.distantPast
    private var frontPID: pid_t = 0
    private var generation = 0
    private var idleItem: DispatchWorkItem?
    /// Cached on the main thread; false (do nothing) until known.
    private var plainLayout = false
    private var observers: [(center: NotificationCenter, token: NSObjectProtocol)] = []

    init() {
        let main = OperationQueue.main
        let own = NotificationCenter.default
        observers.append((own, own.addObserver(forName: NSWindow.didBecomeKeyNotification, object: nil, queue: main) { _ in
            Self.setWindowKey(true)
        }))
        observers.append((own, own.addObserver(forName: NSWindow.didResignKeyNotification, object: nil, queue: main) { _ in
            Self.setWindowKey(false)
        }))
        let workspace = NSWorkspace.shared.notificationCenter
        observers.append((workspace, workspace.addObserver(forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: main) { [weak self] _ in
            self?.clear(boundary: true)
        }))
        let distributed = DistributedNotificationCenter.default()
        observers.append((distributed, distributed.addObserver(
            forName: Notification.Name("com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged"),
            object: nil, queue: main) { [weak self] _ in
            self?.refreshInputSource()
        }))
        DispatchQueue.main.async { [weak self] in self?.refreshInputSource() }
    }

    deinit {
        for observer in observers { observer.center.removeObserver(observer.token) }
    }

    /// Main thread only: the input-source API is never called on the tap thread.
    private func refreshInputSource() {
        var plain = false
        if let unmanaged = TISCopyCurrentKeyboardInputSource() {
            let source = unmanaged.takeRetainedValue()
            if let raw = TISGetInputSourceProperty(source, kTISPropertyInputSourceType) {
                let kind = Unmanaged<CFString>.fromOpaque(raw).takeUnretainedValue() as String
                plain = kind == "TISTypeKeyboardLayout"
            }
        }
        lock.lock(); plainLayout = plain; lock.unlock()
        if !plain { clear(boundary: false) }
    }

    // MARK: - Configuration (main actor)

    /// Recomputes which snippets can expand. Clears the buffer when they changed.
    func configure(snippets: [LauncherSnippet], on: Bool) {
        var found: [[Character]] = []
        if on {
            for snippet in snippets where !snippet.body.isEmpty {
                guard let keyword = Self.triggerKeyword(snippet.keyword) else { continue }
                let nested = LauncherSnippetNesting.expand(
                    snippet.body, snippets: snippets,
                    visiting: [snippet.name.lowercased(), snippet.keyword.lowercased()].filter { !$0.isEmpty })
                if nested.contains("{argument}") { continue }
                if !found.contains(keyword) { found.append(keyword) }
            }
            let all = found
            found = all.filter { keyword in
                !all.contains { $0.count > keyword.count && Array($0.prefix(keyword.count)) == keyword }
            }
        }
        lock.lock()
        if found != keywords || active != (on && !found.isEmpty) {
            keywords = found
            cap = min(Self.hardCap, found.map { $0.count }.max() ?? 0)
            active = on && !found.isEmpty
            clearLocked(boundary: false)
        }
        lock.unlock()
    }

    /// Stops listening and forgets everything.
    func reset() {
        lock.lock()
        active = false
        keywords = []
        cap = 0
        clearLocked(boundary: false)
        lock.unlock()
    }

    private static func triggerKeyword(_ raw: String) -> [Character]? {
        let chars = Array(raw)
        guard chars.count >= 2, chars.count <= hardCap, let first = chars.first,
              !first.isLetter, !first.isNumber else { return nil }
        // Smart substitutions in the target field would change the length.
        if raw.contains("--") || raw.contains("...") { return nil }
        for char in chars {
            if char.isWhitespace || char.isNewline { return nil }
            if char.unicodeScalars.contains(where: { $0.value < 0x20 || $0.value == 0x7F }) { return nil }
        }
        return chars
    }

    private static func isWord(_ char: Character) -> Bool { char.isLetter || char.isNumber }

    /// Caller holds the lock. Any pending expansion and the idle timer are
    /// cancelled too. `boundary` says the next typing starts at a known break.
    private func clearLocked(boundary: Bool) {
        buffer.removeAll()
        boundaryKnown = boundary
        frontPID = 0
        generation += 1
        idleItem?.cancel()
        idleItem = nil
    }

    private func clear(boundary: Bool) {
        lock.lock(); clearLocked(boundary: boundary); lock.unlock()
    }

    /// Caller holds the lock. Forgets the buffer 5 seconds after the last key.
    private func armIdleLocked() {
        idleItem?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.clear(boundary: false) }
        idleItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.idleSeconds, execute: item)
    }

    // MARK: - Tap handler (tap thread)

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        switch type {
        case .leftMouseDown, .leftMouseUp:
            clear(boundary: true)
            return .pass
        case .keyDown:
            break
        default:
            return .pass
        }
        if event.getIntegerValueField(.eventSourceUserData) == Self.marker { return .pass }

        lock.lock(); let listening = active; let plain = plainLayout; lock.unlock()
        guard listening else { return .pass }

        // Keys from another process (password auto-type, other expanders) never count.
        if event.getIntegerValueField(.eventSourceUnixProcessID) != 0 {
            clear(boundary: false)
            return .pass
        }
        // Nothing is kept while a password field has focus, a Pulse window is
        // key, or an input method may be composing.
        if IsSecureEventInputEnabled() || !RightKitAXTrust.isTrusted() || Self.pulseWindowIsKey || !plain {
            clear(boundary: false)
            return .pass
        }
        let notTyping: CGEventFlags = [.maskCommand, .maskControl, .maskAlternate, .maskSecondaryFn]
        if !event.flags.intersection(notTyping).isEmpty {
            clear(boundary: false)
            return .pass
        }
        guard let front = NSWorkspace.shared.frontmostApplication,
              front.processIdentifier != getpid(),
              !(front.bundleIdentifier ?? "").hasPrefix("dev.orthic.pulse")
        else {
            clear(boundary: false)
            return .pass
        }
        let pid = front.processIdentifier
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        let text = Self.typedText(event)
        let now = Date()

        var matched: [Character]?
        var expected = 0
        lock.lock()
        if !active {
            lock.unlock()
            return .pass
        }
        if pid != frontPID || now.timeIntervalSince(lastKey) > Self.idleSeconds {
            clearLocked(boundary: false)
        }
        frontPID = pid
        lastKey = now
        generation += 1  // a new key cancels any expansion still waiting
        if key == Int64(kVK_Return) || key == Int64(kVK_ANSI_KeypadEnter) || key == Int64(kVK_Tab) {
            clearLocked(boundary: true)
            lock.unlock()
            return .pass
        }
        if key == Int64(kVK_Delete) {
            if buffer.isEmpty { boundaryKnown = false } else { buffer.removeLast() }
            if buffer.isEmpty { idleItem?.cancel(); idleItem = nil } else { armIdleLocked() }
            lock.unlock()
            return .pass
        }
        guard let text else {
            clearLocked(boundary: false)
            lock.unlock()
            return .pass
        }
        for char in text {
            buffer.append(char)
            if buffer.count > cap {
                boundaryKnown = !Self.isWord(buffer.removeFirst())
            }
        }
        for keyword in keywords where keyword.count <= buffer.count {
            guard Array(buffer.suffix(keyword.count)) == keyword else { continue }
            let start = buffer.count - keyword.count
            let boundary = start > 0 ? !Self.isWord(buffer[start - 1]) : boundaryKnown
            if !boundary { continue }
            if let current = matched, current.count >= keyword.count { continue }
            matched = keyword
        }
        if matched != nil {
            clearLocked(boundary: false)
            expected = generation
            frontPID = pid
        } else if buffer.isEmpty {
            idleItem?.cancel(); idleItem = nil
        } else {
            armIdleLocked()
        }
        lock.unlock()

        if let matched {
            let word = String(matched)
            let token = expected
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
                MainActor.assumeIsolated { self?.expand(word, generation: token, pid: pid) }
            }
        }
        return .pass
    }

    /// The characters this key typed, from the event itself so every layout
    /// works. Nil for dead keys and composition (empty), control characters
    /// and the private-use codes macOS gives arrows and function keys.
    private static func typedText(_ event: CGEvent) -> String? {
        var length = 0
        var units = [UniChar](repeating: 0, count: 8)
        event.keyboardGetUnicodeString(maxStringLength: units.count, actualStringLength: &length, unicodeString: &units)
        guard length > 0, length <= units.count else { return nil }
        let typed = Array(units.prefix(length))
        for unit in typed where unit < 0x20 || unit == 0x7F || (0xF700...0xF8FF).contains(unit) {
            return nil
        }
        return String(decoding: typed, as: UTF16.self)
    }

    // MARK: - Replacing (main actor)

    /// The whole guard, run before the Backspaces and again right before the paste.
    @MainActor
    private func mayReplace(generation expected: Int, pid: pid_t) -> Bool {
        lock.lock()
        let stillValid = active && generation == expected && plainLayout
        lock.unlock()
        guard stillValid, !Self.pulseWindowIsKey, NSApp.keyWindow == nil,
              !IsSecureEventInputEnabled(), RightKitAXTrust.isTrusted(),
              NSWorkspace.shared.frontmostApplication?.processIdentifier == pid
        else { return false }
        return Self.textFieldHasFocus(pid: pid)
    }

    /// True only when the focused element is positively a text input in `pid`
    /// and not a password field. Unknown or failed queries read as false.
    @MainActor
    private static func textFieldHasFocus(pid: pid_t) -> Bool {
        guard let focused = AX.element(AX.systemWide, "AXFocusedUIElement") else { return false }
        guard RightKitAXElement(focused).pid == pid else { return false }
        let role = AX.string(focused, "AXRole") ?? ""
        let subrole = AX.string(focused, "AXSubrole") ?? ""
        if subrole == "AXSecureTextField" { return false }
        if ["AXTextField", "AXTextArea", "AXComboBox"].contains(role) || subrole == "AXSearchField" { return true }
        return AX.attribute(focused, "AXSelectedTextRange") != nil
    }

    @MainActor
    private func expand(_ keyword: String, generation expected: Int, pid: pid_t) {
        guard mayReplace(generation: expected, pid: pid),
              let resolver = resolve, let snippet = resolver(keyword), !snippet.isEmpty
        else { return }

        // Save the clipboard first: if it cannot be kept whole, or holds a
        // password manager's item, change nothing.
        let board = NSPasteboard.general
        guard let saved = Self.snapshot(board) else { return }
        board.clearContents()
        let item = NSPasteboardItem()
        guard item.setString(snippet, forType: .string),
              item.setData(Data(), forType: Self.transientType),
              board.writeObjects([item])
        else {
            Self.restore(saved, ifChangeCount: nil)
            return
        }
        let written = board.changeCount

        Self.post(kVK_Delete, flags: [], count: keyword.count)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.08) { [weak self] in
            MainActor.assumeIsolated {
                // A Tab, click or key since the checks may have moved the caret.
                guard let self, self.mayReplace(generation: expected, pid: pid) else {
                    Self.restore(saved, ifChangeCount: written)
                    return
                }
                Self.post(kVK_ANSI_V, flags: .maskCommand, count: 1)
                DispatchQueue.main.asyncAfter(deadline: .now() + Self.restoreDelay) {
                    MainActor.assumeIsolated { Self.restore(saved, ifChangeCount: written) }
                }
            }
        }
        Self.log.info("expanded a snippet")
    }

    /// Nil (do nothing) when a type cannot be read, the copy is too large, or
    /// the pasteboard holds a concealed item.
    private static func snapshot(_ board: NSPasteboard) -> [SavedItem]? {
        if board.types?.contains(concealedType) == true { return nil }
        var total = 0
        var items: [SavedItem] = []
        for item in board.pasteboardItems ?? [] {
            if item.types.contains(concealedType) { return nil }
            var entry: SavedItem = []
            for type in item.types {
                guard let data = item.data(forType: type) else { return nil }
                total += data.count
                if total > maxSavedBytes { return nil }
                entry.append((type: type, data: data))
            }
            items.append(entry)
        }
        return items
    }

    /// Puts the saved items back. With a change count, only if the pasteboard
    /// still holds what the expansion wrote; otherwise something else was
    /// copied meanwhile and is left alone. Pulse's clipboard history is told
    /// about the write so it does not store it again.
    @MainActor
    private static func restore(_ saved: [SavedItem], ifChangeCount written: Int?) {
        let board = NSPasteboard.general
        if let written, board.changeCount != written { return }
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

    private static func post(_ key: Int, flags: CGEventFlags, count: Int) {
        let source = CGEventSource(stateID: .hidSystemState)
        for _ in 0..<max(count, 1) {
            for down in [true, false] {
                guard let event = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(key), keyDown: down)
                else { continue }
                event.flags = flags
                event.setIntegerValueField(.eventSourceUserData, value: marker)
                event.post(tap: .cghidEventTap)
            }
        }
    }
}
#endif
