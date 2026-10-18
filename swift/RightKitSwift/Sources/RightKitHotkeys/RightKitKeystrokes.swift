import Foundation

#if os(macOS)
import ApplicationServices
import CoreGraphics

/// One synthetic keyboard event.
public struct RightKitKeyStep: Equatable, Sendable {
    public var keyCode: UInt16
    public var isDown: Bool
    /// `CGEventFlags.rawValue` carried by the event.
    public var flagsRawValue: UInt64
    /// Post as a `flagsChanged` event (a modifier or Fn key press) rather than a key event.
    public var isFlagsChanged: Bool

    public init(keyCode: UInt16, isDown: Bool, flagsRawValue: UInt64, isFlagsChanged: Bool = false) {
        self.keyCode = keyCode
        self.isDown = isDown
        self.flagsRawValue = flagsRawValue
        self.isFlagsChanged = isFlagsChanged
    }
}

/// A modifier key a person may still be holding, with the flag it contributes.
public struct RightKitModifierKey: Equatable, Sendable {
    public var keyCode: UInt16
    public var flagsRawValue: UInt64
    public init(keyCode: UInt16, flagsRawValue: UInt64) {
        self.keyCode = keyCode
        self.flagsRawValue = flagsRawValue
    }
}

/// Pure planning for "send this chord as if the user typed it", with no event posted and no
/// permission needed. The problem it solves: if the user is still holding Shift when a hot key
/// fires, a posted Command-V arrives as Command-Shift-V. The plan lifts what is held, sends the
/// chord, then presses the modifiers again unless the user has let go in the meantime (a stuck
/// modifier is worse than a lost one).
public enum RightKitKeystrokePlanner {
    public static let leftShift: UInt16 = 56
    public static let rightShift: UInt16 = 60
    public static let leftControl: UInt16 = 59
    public static let rightControl: UInt16 = 62
    public static let leftOption: UInt16 = 58
    public static let rightOption: UInt16 = 61
    public static let leftCommand: UInt16 = 55
    public static let rightCommand: UInt16 = 54

    /// The modifiers a user may still be holding when a hot key fires: Shift, Option and Control on
    /// both sides. Command is left out on purpose; the chord normally carries it itself.
    public static var liftableModifiers: [RightKitModifierKey] {
        [
            RightKitModifierKey(keyCode: leftShift, flagsRawValue: CGEventFlags.maskShift.rawValue),
            RightKitModifierKey(keyCode: rightShift, flagsRawValue: CGEventFlags.maskShift.rawValue),
            RightKitModifierKey(keyCode: leftOption, flagsRawValue: CGEventFlags.maskAlternate.rawValue),
            RightKitModifierKey(keyCode: rightOption, flagsRawValue: CGEventFlags.maskAlternate.rawValue),
            RightKitModifierKey(keyCode: leftControl, flagsRawValue: CGEventFlags.maskControl.rawValue),
            RightKitModifierKey(keyCode: rightControl, flagsRawValue: CGEventFlags.maskControl.rawValue),
        ]
    }

    /// Key-ups for `held`, in order. Each carries the flags of the modifiers not yet lifted, so the
    /// target sees the modifier state fall step by step.
    public static func lift(_ held: [RightKitModifierKey]) -> [RightKitKeyStep] {
        var steps: [RightKitKeyStep] = []
        for (index, key) in held.enumerated() {
            let remaining = held[(index + 1)...].reduce(UInt64(0)) { $0 | $1.flagsRawValue }
            steps.append(RightKitKeyStep(keyCode: key.keyCode, isDown: false, flagsRawValue: remaining))
        }
        return steps
    }

    /// Key-down then key-up of `keyCode` carrying `flagsRawValue`.
    public static func chord(keyCode: UInt16, flagsRawValue: UInt64) -> [RightKitKeyStep] {
        [
            RightKitKeyStep(keyCode: keyCode, isDown: true, flagsRawValue: flagsRawValue),
            RightKitKeyStep(keyCode: keyCode, isDown: false, flagsRawValue: flagsRawValue),
        ]
    }

    /// Key-downs that put back the modifiers in `held` that `stillHeld` reports as physically down
    /// now. The flags accumulate as keys go down.
    public static func restore(_ held: [RightKitModifierKey], stillHeld: (UInt16) -> Bool) -> [RightKitKeyStep] {
        var steps: [RightKitKeyStep] = []
        var pressed: UInt64 = 0
        for key in held where stillHeld(key.keyCode) {
            pressed |= key.flagsRawValue
            steps.append(RightKitKeyStep(keyCode: key.keyCode, isDown: true, flagsRawValue: pressed))
        }
        return steps
    }

    /// UTF-16 units of `text` in chunks of at most `size` that never split a surrogate pair.
    /// `CGEventKeyboardSetUnicodeString` is reliable only for short strings (about 20 units).
    public static func utf16Chunks(_ text: String, size: Int = 20) -> [[UniChar]] {
        precondition(size >= 2, "a chunk must hold a surrogate pair")
        var chunks: [[UniChar]] = []
        var current: [UniChar] = []
        for character in text {
            let units = Array(String(character).utf16)
            if current.count + units.count > size, !current.isEmpty {
                chunks.append(current)
                current = []
            }
            if units.count > size {
                // A grapheme larger than a chunk (a long combining sequence) is cut at unit
                // boundaries that avoid splitting a surrogate pair.
                var rest = units[...]
                while !rest.isEmpty {
                    var take = min(size, rest.count)
                    if take < rest.count, UTF16.isLeadSurrogate(rest[rest.startIndex + take - 1]) { take -= 1 }
                    chunks.append(Array(rest.prefix(take)))
                    rest = rest.dropFirst(take)
                }
            } else {
                current += units
            }
        }
        if !current.isEmpty { chunks.append(current) }
        return chunks
    }
}

/// Posts synthetic keyboard and mouse events. Posting needs Accessibility; without it macOS drops
/// the events silently, so check `canPost` first. This type never prompts.
public enum RightKitKeystrokes {
    /// Where an event enters the stream.
    public enum Location: Sendable {
        /// `cghidEventTap`: as if from the hardware; updates system-wide modifier state.
        case hid
        /// `cgSessionEventTap`: into the session, after hardware processing.
        case session

        fileprivate var tapLocation: CGEventTapLocation {
            switch self {
            case .hid: .cghidEventTap
            case .session: .cgSessionEventTap
            }
        }
    }

    /// The source state of the posted events. `hidSystemState` also updates the system-wide
    /// modifier state, which apps that query it (instead of the flags on the event) read.
    public enum SourceState: Sendable {
        case none
        case hidSystem
        case combinedSession

        fileprivate var source: CGEventSource? {
            switch self {
            case .none: nil
            case .hidSystem: CGEventSource(stateID: .hidSystemState)
            case .combinedSession: CGEventSource(stateID: .combinedSessionState)
            }
        }
    }

    public static let keyV: UInt16 = 9
    public static let keyC: UInt16 = 8

    /// True when this process is a trusted Accessibility client, so posted events are delivered.
    /// Never prompts.
    public static var canPost: Bool { AXIsProcessTrusted() }

    /// True when `event` carries `marker` in its source user data: how a tap recognises events its
    /// own app posted and lets them through.
    public static func isMarked(_ event: CGEvent, marker: Int64) -> Bool {
        event.getIntegerValueField(.eventSourceUserData) == marker
    }

    /// The Shift, Option and Control keys physically held right now (hardware state; what this
    /// process posted does not count).
    public static func heldModifiers() -> [RightKitModifierKey] {
        RightKitKeystrokePlanner.liftableModifiers.filter {
            CGEventSource.keyState(.hidSystemState, key: CGKeyCode($0.keyCode))
        }
    }

    /// Posts one step. Returns false when the event could not be created.
    @discardableResult
    public static func post(
        _ step: RightKitKeyStep,
        marker: Int64? = nil,
        location: Location = .hid,
        source: SourceState = .combinedSession,
    ) -> Bool {
        guard let event = CGEvent(
            keyboardEventSource: source.source, virtualKey: CGKeyCode(step.keyCode), keyDown: step.isDown)
        else { return false }
        if step.isFlagsChanged { event.type = .flagsChanged }
        event.flags = CGEventFlags(rawValue: step.flagsRawValue)
        if let marker { event.setIntegerValueField(.eventSourceUserData, value: marker) }
        event.post(tap: location.tapLocation)
        return true
    }

    /// Presses and releases `keyCode` with `flags`, `count` times.
    public static func tap(
        keyCode: UInt16,
        flags: CGEventFlags = [],
        count: Int = 1,
        marker: Int64? = nil,
        location: Location = .hid,
        source: SourceState = .hidSystem,
    ) {
        for _ in 0..<max(count, 1) {
            for step in RightKitKeystrokePlanner.chord(keyCode: keyCode, flagsRawValue: flags.rawValue) {
                post(step, marker: marker, location: location, source: source)
            }
        }
    }

    /// Sends `keyCode` with `flags` as the user would. With `liftHeldModifiers` (the default) the
    /// Shift, Option and Control keys the user is still holding are lifted first and put back after
    /// unless released meanwhile. Does nothing without Accessibility (the text, if any, stays on the
    /// clipboard). Returns whether it posted.
    @discardableResult
    public static func sendChord(
        keyCode: UInt16,
        flags: CGEventFlags,
        liftHeldModifiers: Bool = true,
        marker: Int64? = nil,
        location: Location = .hid,
        source: SourceState = .combinedSession,
    ) -> Bool {
        guard canPost else { return false }
        let held = liftHeldModifiers ? heldModifiers() : []
        for step in RightKitKeystrokePlanner.lift(held) {
            post(step, marker: marker, location: location, source: source)
        }
        for step in RightKitKeystrokePlanner.chord(keyCode: keyCode, flagsRawValue: flags.rawValue) {
            post(step, marker: marker, location: location, source: source)
        }
        let restore = RightKitKeystrokePlanner.restore(held) {
            CGEventSource.keyState(.hidSystemState, key: CGKeyCode($0))
        }
        for step in restore {
            post(step, marker: marker, location: location, source: source)
        }
        return true
    }

    /// Command-V.
    @discardableResult
    public static func sendPaste(marker: Int64? = nil) -> Bool {
        sendChord(keyCode: keyV, flags: .maskCommand, marker: marker)
    }

    /// Types `text` into the focused app as Unicode key events, in chunks, independent of the
    /// keyboard layout. Returns false without Accessibility.
    @discardableResult
    public static func typeText(
        _ text: String,
        marker: Int64? = nil,
        location: Location = .hid,
        source: SourceState = .hidSystem,
    ) -> Bool {
        guard canPost else { return false }
        for chunk in RightKitKeystrokePlanner.utf16Chunks(text) {
            for down in [true, false] {
                guard let event = CGEvent(
                    keyboardEventSource: source.source, virtualKey: 0, keyDown: down)
                else { continue }
                event.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: chunk)
                if let marker { event.setIntegerValueField(.eventSourceUserData, value: marker) }
                event.post(tap: location.tapLocation)
            }
        }
        return true
    }

    /// Posts a mouse-button press or release at `point` (global display coordinates). Pass a
    /// `marker` so a tap that swallowed the original can tell this is the re-post.
    @discardableResult
    public static func postMouseButton(
        _ button: CGMouseButton,
        down: Bool,
        at point: CGPoint,
        clickState: Int64 = 1,
        marker: Int64? = nil,
        location: Location = .session,
    ) -> Bool {
        let type: CGEventType
        switch button {
        case .left: type = down ? .leftMouseDown : .leftMouseUp
        case .right: type = down ? .rightMouseDown : .rightMouseUp
        default: type = down ? .otherMouseDown : .otherMouseUp
        }
        guard let event = CGEvent(
            mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: button)
        else { return false }
        event.setIntegerValueField(.mouseEventClickState, value: clickState)
        if let marker { event.setIntegerValueField(.eventSourceUserData, value: marker) }
        event.post(tap: location.tapLocation)
        return true
    }
}
#endif
