#if os(macOS)
import RightKitEngineClient
import Foundation
import Observation
import RightKitHub

/// State and actions behind one Settings page (Permissions, Accounts, Appearance, Notifications or
/// General): the notch state, the kit's settings model over `NotchSettingsStore`, and the engine-backed
/// sub-models. Port of `useNotch` and the page-level state in hub/src/views/Settings.tsx.
@MainActor
@Observable
final class SettingsPageModel {
    let kind: SettingsPageKind
    private let notch: any NotchBridge
    /// The kit's model: every scalar setting reads and writes through it.
    let kit: HubSettingsModel
    let share: SettingsShareModel
    let permissions: SettingsPermissionsModel
    /// The agent secure box (saved logins and virtual cards) and the owner-only switches around it.
    let secureBox: SecureBoxModel
    let claudeMove: SettingsClaudeMoveModel
    private let live: Bool

    /// The notch state; nil until the notch has published one.
    private(set) var state: SettingsNotchState?
    /// Every setting by key, built from the options the notch published.
    private(set) var catalog: [String: HubSetting]
    private(set) var context: SettingsCatalogContext
    /// The failure of the last command the notch refused (the page's `error` line).
    var commandError: String?
    /// What the page's "Search settings" box holds.
    var query = ""

    init(kind: SettingsPageKind, notch: any NotchBridge, engine: any EngineClient, live: Bool) {
        self.kind = kind
        self.notch = notch
        self.live = live
        let initial = SettingsCatalogContext()
        context = initial
        catalog = SettingsCatalog.lookup(initial)
        kit = HubSettingsModel(
            store: NotchSettingsStore(notch: notch, choiceKeys: SettingsCatalog.choiceKeys, observes: live),
            groups: SettingsCatalog.allGroups(initial)
        )
        share = SettingsShareModel(engine: engine)
        permissions = SettingsPermissionsModel(engine: engine)
        secureBox = SecureBoxModel(engine: engine)
        claudeMove = SettingsClaudeMoveModel(engine: engine)
    }

    // MARK: Loading

    /// Reads everything the page shows once, so a picture shows real data (the off-screen renderer).
    func prepare() async {
        await refreshState()
        await kit.start()  // the store of a prepared model does not observe, so this returns after one read
        switch kind {
        case .general:
            await share.load()
        case .permissions:
            if isWindows {
                await permissions.loadWindows()
                await share.load()
            } else {
                await permissions.loadFDA()
            }
        default:
            break
        }
    }

    /// Everything that keeps the page current while it is on screen; ends when the view goes away.
    func run() async {
        await refreshState()
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await self.runKit() }
            group.addTask { await self.followState() }
            group.addTask { await self.pollState() }
            switch self.kind {
            case .general:
                group.addTask { await self.share.poll() }
            case .permissions:
                let windows = self.isWindows
                group.addTask { await self.permissions.poll(windows: windows) }
                if windows {
                    group.addTask { await self.share.poll() }
                }
            default:
                break
            }
        }
    }

    private func runKit() async { await kit.start() }

    private func followState() async {
        for await data in notch.stateUpdates() {
            if Task.isCancelled { break }
            apply(data)
        }
    }

    /// The page also re-read the state every 3 s.
    private func pollState() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3))
            if Task.isCancelled { break }
            await refreshState()
        }
    }

    func refreshState() async {
        guard let data = await notch.state() else { return }
        apply(data)
    }

    private func apply(_ data: Data) {
        guard let next = SettingsNotchState.decode(data) else { return }
        if next != state { state = next }
        let nextContext = SettingsCatalogContext(state: next)
        if nextContext != context {
            context = nextContext
            catalog = SettingsCatalog.lookup(nextContext)
        }
    }

    // MARK: Reading the state

    var isWindows: Bool { SettingsPlatform.isWindows || state?.platform == "windows" }

    var settings: [String: SettingsJSON] { state?.settings ?? [:] }

    /// True when the notch published the key. The Windows notch publishes only part of the Mac's
    /// settings, and a row shows only for a key that is published.
    func has(_ key: String) -> Bool { settings[key] != nil }

    func bool(_ key: String) -> Bool { settings[key]?.boolValue ?? false }

    func text(_ key: String) -> String { settings[key]?.stringValue ?? "" }

    func number(_ key: String) -> Double? { settings[key]?.numberValue }

    func list(_ key: String) -> [String] { settings[key]?.stringList ?? [] }

    /// The options the notch published for a choice; empty hides the row.
    func options(_ key: String) -> [String] { state?.options[key] ?? [] }

    func setting(_ key: String) -> HubSetting? { catalog[key] }

    /// True while the page's search box has a word in it. Rows that need their parent switch on show
    /// anyway then, so they can be found (the page's `searching`).
    var searching: Bool { !SettingsSearch.words(query).isEmpty }

    /// What decides the wording of a note under a row.
    var noteContext: SettingsNotes.Context {
        SettingsNotes.Context(windows: isWindows, hasToolWheel: has("toolWheelEnabled"), hasClipboard: has("launcherClipboard"))
    }

    // MARK: Commands

    /// Sends a notch command (Settings.tsx:207 `send`): the JSON object is `{"command": name, ...args}`.
    func send(_ command: String, _ args: [String: SettingsJSON] = [:]) async {
        var body = args
        body["command"] = .string(command)
        do {
            try await notch.command(try JSON.encoder.encode(body))
            commandError = nil
        } catch {
            commandError = SettingsFormat.describe(error)
        }
    }

    /// `set(key, value)` for a value the kit does not edit (numbers, lists, JSON text).
    func setValue(_ key: String, _ value: SettingsJSON) async {
        state?.settings[key] = value
        await send("set", ["key": .string(key), "value": value])
    }

    /// Shows or hides a gauge: the `connect` command, with the change shown at once.
    func setGauge(_ id: String, on: Bool) async {
        if let index = state?.gauges?.firstIndex(where: { $0.id == id }) {
            state?.gauges?[index].connected = on
        }
        await send("connect", ["provider": .string(id), "value": .bool(on)])
    }

    /// The `order` command with the new order of the gauges (Settings.tsx GaugesGroup `move`), shown at once.
    func setGaugeOrder(_ ids: [String]) async {
        if let gauges = state?.gauges {
            for (rank, id) in ids.enumerated() {
                if let index = gauges.firstIndex(where: { $0.id == id }) { state?.gauges?[index].order = rank }
            }
        }
        await send("order", ["value": SettingsJSON.strings(ids)])
    }

    /// The `order` command with the new order of the logins.
    func setOrder(_ ids: [String]) async {
        state?.providerOrder = ids
        await send("order", ["value": SettingsJSON.strings(ids)])
    }
}
#endif
