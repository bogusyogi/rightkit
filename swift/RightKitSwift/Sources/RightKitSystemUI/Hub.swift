#if os(macOS)
import Foundation
import RightKitHub
import SwiftUI

/// Pulse's hub: the sections it registers with `HubShell`, in sidebar order.
///
/// Ids match the Tauri hub's (`hub/src/App.tsx`) so the notch can keep opening a named section.
/// Every engine method a section calls is a `pulse-engine` command with the same params and result
/// as the Tauri command of that name (`engine/src/contract.rs`).
public enum Hub {
    /// All sections. `notch` is the in-process notch bridge (state and settings commands); the
    /// notch supplies it when it hosts the hub as a scene.
    public static func sections(notch: any NotchBridge) -> [any HubSection] {
        [
            OverviewSection(notch: notch),
            ActivitySection(),
            StorageSection(notch: notch),
            CleanupSection(notch: notch),
            MonitorSection(notch: notch),
            MembraneSection(),
            AppsSection(),
            ChatSection(),
        ] + SettingsSections.all(notch: notch)
    }
}

/// A Pulse section. `preparedView` returns the view with its data already loaded, so an off-screen
/// render (the gate's `pulse-hub-render`) shows real content without waiting on `.task`.
public protocol PulseSection: HubSection {
    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView
    /// Further named states of the section for the off-screen renderer (an image selected, a dialog
    /// open), each with its data already loaded. The renderer draws each as `<id>-<name>-<scheme>.png`.
    /// Most sections have none.
    @MainActor func preparedStates(_ ctx: HubContext) async -> [SectionState]
}

public extension PulseSection {
    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView { makeView(ctx) }
    @MainActor func preparedStates(_ ctx: HubContext) async -> [SectionState] { [] }
}

/// One named, prepared state of a section, for the off-screen renderer.
public struct SectionState {
    public let name: String
    public let view: AnyView

    public init(name: String, view: AnyView) {
        self.name = name
        self.view = view
    }
}

/// What the hub needs from the notch it lives in: the state the notch publishes (today
/// `notch-state.json`, the same JSON the Tauri hub's `notch_state` returns) and the settings
/// commands it accepts (today the `hub-commands` file, the same JSON `notch_command` takes).
public protocol NotchBridge: Sendable {
    /// The notch state as JSON, or nil when the notch has not published one.
    func state() async -> Data?
    /// Sends one command, e.g. `{"set": {"key": "...", "value": ...}}`.
    func command(_ json: Data) async throws
    /// New state each time the notch publishes it.
    func stateUpdates() -> AsyncStream<Data>
}

/// A bridge with nothing behind it: no state, and every command refused.
public struct NoNotchBridge: NotchBridge {
    public init() {}
    public func state() async -> Data? { nil }
    public func command(_ json: Data) async throws {
        throw EngineError(code: "unsupported", message: "no notch", retryable: false)
    }
    public func stateUpdates() -> AsyncStream<Data> { AsyncStream { $0.finish() } }
}

/// Shared JSON coding for engine params and results: the page's keys are camelCase already, so no
/// key strategy.
public enum JSON {
    public static let decoder = JSONDecoder()
    public static let encoder = JSONEncoder()
}
#endif
