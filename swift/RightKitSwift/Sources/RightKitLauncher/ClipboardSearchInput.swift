#if os(macOS)
import AppKit

/// The text input behind the strip's search field (bogusyogi/pulse#377). It is an
/// `NSTextInputClient`, so an input method (Japanese, Chinese, Korean and others) composes here:
/// the composing text is shown as marked text, and only what the method commits reaches the
/// search. Keys the method does not take are left to the strip.
@MainActor
final class ClipboardSearchInput: NSView, NSTextInputClient {
    /// Text the method committed (or typed plainly), in order.
    var onCommit: ((String) -> Void)?
    /// The composition in progress; "" when none.
    var onComposition: ((String) -> Void)?
    /// Where the candidate window sits, in screen coordinates.
    var anchor: () -> NSRect = { .zero }

    private var marked = ""
    private var composing = false

    override var acceptsFirstResponder: Bool { true }

    override func keyDown(with event: NSEvent) {
        interpretKeyEvents([event])
    }

    /// Keys with no text meaning here (arrows, Escape while not composing) do nothing in the field.
    override func doCommand(by selector: Selector) {}

    func insertText(_ string: Any, replacementRange: NSRange) {
        let text = Self.plain(string)
        endComposition()
        if !text.isEmpty { onCommit?(text) }
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        marked = Self.plain(string)
        // Empty marked text is no composition: the method removed it, so the keys return to the strip.
        guard !marked.isEmpty else { endComposition(); return }
        composing = true
        onComposition?(marked)
    }

    func unmarkText() {
        let text = marked
        endComposition()
        if !text.isEmpty { onCommit?(text) }
    }

    func selectedRange() -> NSRange { NSRange(location: 0, length: 0) }

    func markedRange() -> NSRange {
        composing ? NSRange(location: 0, length: marked.utf16.count) : NSRange(location: NSNotFound, length: 0)
    }

    func hasMarkedText() -> Bool { composing }

    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        nil
    }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        anchor()
    }

    func characterIndex(for point: NSPoint) -> Int { NSNotFound }

    private func endComposition() {
        marked = ""
        composing = false
        onComposition?("")
    }

    private static func plain(_ string: Any) -> String {
        if let attributed = string as? NSAttributedString { return attributed.string }
        return string as? String ?? ""
    }
}
#endif
