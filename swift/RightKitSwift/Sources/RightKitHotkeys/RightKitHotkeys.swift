import Foundation

#if os(macOS)
import Carbon.HIToolbox

/// Modifier keys of a global hot key. Raw values are the Carbon modifier masks that
/// `RegisterEventHotKey` takes (`cmdKey`, `shiftKey`, `optionKey`, `controlKey`), so they are stable
/// and safe to persist.
public struct RightKitHotkeyModifiers: OptionSet, Hashable, Codable, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let command = Self(rawValue: 0x0100)
    public static let shift = Self(rawValue: 0x0200)
    public static let option = Self(rawValue: 0x0800)
    public static let control = Self(rawValue: 0x1000)

    /// Modifier glyphs in the macOS menu order (control, option, shift, command).
    public var symbols: String {
        var out = ""
        if contains(.control) { out += "\u{2303}" }
        if contains(.option) { out += "\u{2325}" }
        if contains(.shift) { out += "\u{21E7}" }
        if contains(.command) { out += "\u{2318}" }
        return out
    }
}

/// A key plus modifiers. `keyCode` is a virtual key code (`kVK_*`), which is layout independent.
public struct RightKitHotkeyCombo: Hashable, Codable, Sendable {
    public var keyCode: UInt32
    public var modifiers: RightKitHotkeyModifiers

    public init(keyCode: UInt32, modifiers: RightKitHotkeyModifiers) {
        self.keyCode = keyCode
        self.modifiers = modifiers
    }

    /// A cheap pre-check so a settings UI can reject a bare letter before registering it.
    public var hasModifier: Bool { !modifiers.isEmpty }
}

/// Which edge of the key press an event reports.
public enum RightKitHotkeyPhase: Sendable, Equatable {
    case pressed
    case released
}

/// A registered hot key was pressed or released.
public struct RightKitHotkeyEvent: Sendable, Equatable {
    public let id: UInt32
    public let combo: RightKitHotkeyCombo
    public let phase: RightKitHotkeyPhase
}

public enum RightKitHotkeyError: Error, Equatable, Sendable {
    /// Another registration in this registrar already uses the combo.
    case alreadyRegistered(RightKitHotkeyCombo)
    /// Carbon reported `eventHotKeyExistsErr`: another app (or the system) owns the combo.
    case unavailable(RightKitHotkeyCombo)
    /// `InstallEventHandler` or `RegisterEventHotKey` failed with this `OSStatus`.
    case carbon(OSStatus)
}

/// Carbon `RegisterEventHotKey` hot keys. Needs no Accessibility or Input Monitoring permission.
/// Events arrive on the main thread, so the registrar is main-actor isolated. Hot keys that need a
/// permission (event taps, modifier-only keys) are out of scope here.
@MainActor
public final class RightKitHotkeyRegistrar {
    private struct Entry {
        let combo: RightKitHotkeyCombo
        let ref: EventHotKeyRef
        let handler: @MainActor (RightKitHotkeyEvent) -> Void
    }

    /// Four-character code "RKHK".
    fileprivate static let signature: OSType = 0x524B_484B

    private var entries: [UInt32: Entry] = [:]
    private var nextID: UInt32 = 1
    private var eventHandler: EventHandlerRef?

    /// Call `unregisterAll()` before releasing the registrar: Swift 6.1 has no isolated `deinit`,
    /// so hot keys are not released automatically.
    public init() {}

    /// Registers `combo` and returns an id for `unregister(_:)`. `handler` runs on the main actor
    /// for both press and release (`event.phase`).
    @discardableResult
    public func register(
        _ combo: RightKitHotkeyCombo,
        handler: @escaping @MainActor (RightKitHotkeyEvent) -> Void
    ) throws(RightKitHotkeyError) -> UInt32 {
        if entries.values.contains(where: { $0.combo == combo }) { throw .alreadyRegistered(combo) }
        try installHandlerIfNeeded()

        let id = nextID
        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(
            combo.keyCode, combo.modifiers.rawValue,
            EventHotKeyID(signature: Self.signature, id: id),
            GetApplicationEventTarget(), 0, &ref
        )
        guard status == noErr, let ref else {
            throw status == OSStatus(eventHotKeyExistsErr) ? .unavailable(combo) : .carbon(status)
        }
        nextID += 1
        entries[id] = Entry(combo: combo, ref: ref, handler: handler)
        return id
    }

    /// Releases one hot key. Unknown ids are ignored.
    public func unregister(_ id: UInt32) {
        guard let entry = entries.removeValue(forKey: id) else { return }
        UnregisterEventHotKey(entry.ref)
    }

    public func unregisterAll() {
        for id in Array(entries.keys) { unregister(id) }
        if let eventHandler {
            RemoveEventHandler(eventHandler)
            self.eventHandler = nil
        }
    }

    public var registeredCombos: [RightKitHotkeyCombo] { entries.values.map(\.combo) }

    private func installHandlerIfNeeded() throws(RightKitHotkeyError) {
        guard eventHandler == nil else { return }
        var specs = [
            EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed)),
            EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyReleased)),
        ]
        var handler: EventHandlerRef?
        let status = InstallEventHandler(
            GetApplicationEventTarget(),
            { _, event, userData in
                guard let event, let userData else { return OSStatus(eventNotHandledErr) }
                var hotKeyID = EventHotKeyID()
                let got = GetEventParameter(
                    event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID),
                    nil, MemoryLayout<EventHotKeyID>.size, nil, &hotKeyID
                )
                guard got == noErr, hotKeyID.signature == RightKitHotkeyRegistrar.signature else {
                    return OSStatus(eventNotHandledErr)
                }
                let phase: RightKitHotkeyPhase = GetEventKind(event) == UInt32(kEventHotKeyReleased) ? .released : .pressed
                let registrar = Unmanaged<RightKitHotkeyRegistrar>.fromOpaque(userData)
                let id = hotKeyID.id
                MainActor.assumeIsolated {
                    registrar.takeUnretainedValue().dispatch(id: id, phase: phase)
                }
                return noErr
            },
            specs.count, &specs, Unmanaged.passUnretained(self).toOpaque(), &handler
        )
        guard status == noErr else { throw .carbon(status) }
        eventHandler = handler
    }

    private func dispatch(id: UInt32, phase: RightKitHotkeyPhase) {
        guard let entry = entries[id] else { return }
        entry.handler(RightKitHotkeyEvent(id: id, combo: entry.combo, phase: phase))
    }
}
#endif
