#if os(macOS)
import RightKitNotchCore
import RightKitNotch
import AppKit
import ApplicationServices
import Carbon.HIToolbox
import RightKitAccessibility

/// docs/clipboard-spec.md 7.11.2 steps 4, 5 and 8: bring the remembered app forward if it is
/// not, wait until it is, and send Cmd+V as the target app should see it (Cmd only, with any
/// modifier the user still holds lifted first and pressed again afterwards).
@MainActor
enum ClipboardPaster {
    /// Whether the paste keystroke would be delivered (Accessibility, 9).
    static var canSendKeys: Bool { RightKitAXTrust.isTrusted() }

    private static var asked = false

    /// Asks macOS for Accessibility once, the first time a paste needs it (9).
    static func askForAccessibilityOnce() {
        guard !asked, !RightKitAXTrust.isTrusted() else { return }
        asked = true
        _ = RightKitAXTrust.isTrusted(prompt: true)
    }

    /// Sends the paste once `app` is frontmost: polls up to 300 ms every 10 ms.
    /// `afterKeystroke` runs once Cmd+V has been posted (the restore-after-paste setting uses it).
    static func paste(into app: NSRunningApplication?, afterKeystroke: (@MainActor () -> Void)? = nil) {
        Runtime.refuseUnderTest("the paste keystroke")
        if let app, NSWorkspace.shared.frontmostApplication?.processIdentifier != app.processIdentifier {
            app.activate()
        }
        Task { @MainActor in
            for _ in 0..<30 {
                let front = NSWorkspace.shared.frontmostApplication?.processIdentifier
                if app == nil || front == app?.processIdentifier { break }
                try? await Task.sleep(nanoseconds: 10_000_000)
            }
            sendCommandV()
            afterKeystroke?()
        }
    }

    /// The modifiers a user may still be holding, as the keys that would be lifted and pressed.
    private static let heldKeys: [(key: Int, flag: CGEventFlags)] = [
        (kVK_Shift, .maskShift), (kVK_RightShift, .maskShift),
        (kVK_Option, .maskAlternate), (kVK_RightOption, .maskAlternate),
        (kVK_Control, .maskControl), (kVK_RightControl, .maskControl),
    ]

    private static func sendCommandV() {
        let source = CGEventSource(stateID: .combinedSessionState)
        // Read from the hardware state: what this process posted does not count as held.
        let held = heldKeys.filter {
            CGEventSource.keyState(.hidSystemState, key: CGKeyCode($0.key))
        }
        func post(_ key: Int, down: Bool, flags: CGEventFlags) {
            let event = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(key), keyDown: down)
            event?.flags = flags
            event?.post(tap: .cghidEventTap)
        }
        // Lift what is held, so the target sees Cmd+V and not Cmd+Shift+V.
        var remaining: CGEventFlags = []
        for entry in held { remaining.formUnion(entry.flag) }
        for entry in held {
            remaining.remove(entry.flag)
            post(entry.key, down: false, flags: remaining)
        }
        post(kVK_ANSI_V, down: true, flags: .maskCommand)
        post(kVK_ANSI_V, down: false, flags: .maskCommand)
        // And press them again, unless the user has let go meanwhile (a stuck key is worse).
        var pressed: CGEventFlags = []
        for entry in held where CGEventSource.keyState(.hidSystemState, key: CGKeyCode(entry.key)) {
            pressed.formUnion(entry.flag)
            post(entry.key, down: true, flags: pressed)
        }
    }
}
#endif
