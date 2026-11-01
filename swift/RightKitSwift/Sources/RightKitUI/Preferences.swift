#if os(macOS)
import Combine
import Foundation
import ServiceManagement
import os

/// What the user has chosen, kept in `UserDefaults`.
@MainActor
final class Preferences: ObservableObject {
    static let showUsagePaceKey = GeneratedSettings.Key.showUsagePace

    /// Provider IDs that currently have a ring. Stored as the ones that are
    /// on, so a provider added later stays off until someone switches it on —
    /// Claude and Codex excepted, which still default on as a family.
    @Published var connectedProviders: Set<String> {
        didSet { defaults.set(Array(connectedProviders), forKey: Keys.connected) }
    }

    /// Every provider id this copy has already decided on, so a later version's
    /// new provider is recognised as new rather than as "never chosen".
    @Published private(set) var seenProviders: Set<String> {
        didSet { defaults.set(Array(seenProviders), forKey: Keys.seen) }
    }

    /// Loaded-model cells hide without stopping the shared runtime. Stored as
    /// the ones that are off: a model Ollama or LM Studio loads later stays
    /// visible until someone hides it. Providers cannot share this list —
    /// their on-list treats absence as off.
    @Published private(set) var disabledModels: Set<String> {
        didSet { defaults.set(Array(disabledModels), forKey: Keys.disabledModels) }
    }

    /// The old hidden-providers list, kept only until `reconcile` can invert it
    /// against the ids actually on this Mac.
    private var pendingHidden: Set<String>?

    /// Providers whose threshold alerts are muted. Stored as the muted set so
    /// a provider added later alerts by default. Connection is stored the
    /// other way: the ones that are on.
    @Published var mutedAlertProviders: Set<String> {
        didSet { defaults.set(Array(mutedAlertProviders), forKey: Keys.mutedAlerts) }
    }

    /// Names given to accounts in Settings, by provider id. Only the ones
    /// set: an account with no entry keeps the name its provider gives.
    @Published var accountNicknames: [String: String] {
        didSet { defaults.set(accountNicknames, forKey: Keys.accountNicknames) }
    }

    /// The order the user has dragged the rings into, as provider ids.
    ///
    /// Stored as the ids actually placed rather than as every id known at the
    /// time: providers are discovered at launch — Claude Code contributes one
    /// per `~/.claude-<slug>` — so an exhaustive list written today is wrong
    /// the moment a profile appears. `ProviderOrder` reconciles the two,
    /// forgivingly in both directions.
    ///
    /// Empty means never chosen, which is not the same as having chosen the
    /// order the app ships with: keeping them distinct is what lets a later
    /// version change the built-in order for everyone who never had an opinion.
    @Published var providerOrder: [String] {
        didSet { defaults.set(providerOrder, forKey: Keys.order) }
    }

    /// How much of itself the notch shows at rest.
    @Published var notchVisibility: NotchVisibility {
        didSet { defaults.set(notchVisibility.rawValue, forKey: Keys.visibility) }
    }

    /// The master switch for every Pulse notification: the notch's toasts, the system
    /// banners and their sounds, in every visibility state (Hidden and folded included).
    /// On by default.
    @Published var notifications: Bool {
        didSet { defaults.set(notifications, forKey: "notifications") }
    }

    /// Whether a frontmost full-screen app folds the notch away.
    @Published var foldsForFullScreen: Bool {
        didSet { defaults.set(foldsForFullScreen, forKey: Keys.foldsForFullScreen) }
    }

    /// Which screen edge the notch is welded to.
    @Published var notchEdge: NotchEdge {
        didSet { defaults.set(notchEdge.rawValue, forKey: Keys.edge) }
    }

    /// How large the notch is drawn, as one of three named sizes.
    ///
    /// Ignored while `usesCustomNotchScale` is on — the two are kept apart
    /// rather than collapsed into one number so that switching back to the
    /// presets returns to the preset you last chose, instead of to whichever
    /// preset happens to sit nearest the slider.
    @Published var notchSize: NotchSize {
        didSet { defaults.set(notchSize.rawValue, forKey: Keys.size) }
    }

    /// Whether the slider decides the size rather than the three presets.
    @Published var usesCustomNotchScale: Bool {
        didSet { defaults.set(usesCustomNotchScale, forKey: Keys.usesCustomSize) }
    }

    /// The slider's own multiplier, honoured only when the slider is in
    /// charge. Clamped on the way in: a value typed straight into `defaults`
    /// could otherwise shrink the notch to nothing or blow it off the screen.
    @Published var customNotchScale: Double {
        didSet {
            let clamped = min(max(customNotchScale, Self.customScaleRange.lowerBound),
                              Self.customScaleRange.upperBound)
            if clamped != customNotchScale { customNotchScale = clamped; return }
            defaults.set(customNotchScale, forKey: Keys.customSize)
        }
    }

    /// Where the slider may go. Wider than the presets at both ends, but not
    /// unbounded.
    ///
    /// The floor was three quarters, because below that the percentage under
    /// each ring stopped being readable — and that is the one thing the notch
    /// exists for. The reading is a setting of its own now (`showsNotchReadings`),
    /// so anyone who wants the notch smaller than the type allows can turn the
    /// type off and keep the rings, which read as colour and fill at any size.
    /// Half is as small as a ring stays legible as a ring.
    static let customScaleRange: ClosedRange<Double> = 0.5...1.5

    /// What the notch is actually drawn at, whichever control is in charge.
    var notchScale: CGFloat {
        usesCustomNotchScale ? CGFloat(customNotchScale) : notchSize.scale
    }

    /// The display the notch stays on, or the original focus-following behaviour.
    ///
    /// Only meaningful in `NotchScreenScope.main` — pinning a display and
    /// drawing on every display are two different questions, and this answers
    /// the first one. `all` ignores it entirely: there is no "the" display to
    /// pin when every one of them gets its own notch.
    @Published var displayPreference: DisplayPreference {
        didSet {
            switch displayPreference {
            case .followActiveWindow:
                defaults.removeObject(forKey: Keys.display)
            case .display(let id):
                defaults.set(id, forKey: Keys.display)
            }
        }
    }

    /// Which displays get a notch when more than one is connected.
    @Published var notchScope: NotchScreenScope {
        didSet { defaults.set(notchScope.rawValue, forKey: Keys.scope) }
    }

    /// Where along that edge the notch sits, nudged from the centred default
    /// by ⌥-dragging the pill. One value per edge — moving it on the right
    /// should not silently relocate it on the top too — so this is read and
    /// written through `offset(for:)`/`setOffset(_:for:)` rather than exposed
    /// as a single published value the way the other settings are.
    func offset(for edge: NotchEdge) -> CGFloat {
        CGFloat(defaults.double(forKey: Self.offsetKey(for: edge)))
    }

    func setOffset(_ offset: CGFloat, for edge: NotchEdge) {
        defaults.set(Double(offset), forKey: Self.offsetKey(for: edge))
    }

    private static func offsetKey(for edge: NotchEdge) -> String { "notchOffset.\(edge.rawValue)" }

    @Published var resetTimeFormat: ResetTimeFormat {
        didSet { defaults.set(resetTimeFormat.rawValue, forKey: Keys.resetTimeFormat) }
    }

    @Published var showUsagePace: Bool {
        didSet { defaults.set(showUsagePace, forKey: Self.showUsagePaceKey) }
    }

    /// Whether pointing at a ring, or opening the menu bar's menu, refuses every
    /// reading a provider is holding and asks the provider itself.
    ///
    /// Off by default, and it has to be: it is not strictly better. A look
    /// already asks for a live reading, which is served from a cache only while
    /// that cache is newer than a couple of minutes. This spends a request even
    /// when the cache was written seconds ago — and on a provider that rate
    /// limits, one request too many is answered with a back-off that then holds
    /// a number older than the cache would have been. Worth having for somebody
    /// comparing Codenotch against a vendor's own dashboard figure by figure;
    /// not worth making everybody pay for.
    @Published var asksProviderOnLook: Bool {
        didSet { defaults.set(asksProviderOnLook, forKey: Keys.asksProviderOnLook) }
    }

    /// Whether Pulse looks for a newer release, at most every six hours. On by
    /// default. It only checks: nothing is downloaded or installed until Update.
    @Published var autoUpdateCheck: Bool {
        didSet { defaults.set(autoUpdateCheck, forKey: Keys.autoUpdateCheck) }
    }

    // Pulse fork: Mac conveniences. Every one is off until chosen, and each
    // needs Accessibility (see `Conveniences/ConveniencesService.swift`).
    @Published var convFinderCutPaste: Bool {
        didSet { defaults.set(convFinderCutPaste, forKey: Keys.convFinderCutPaste) }
    }
    @Published var convWindowMaximizer: Bool {
        didSet { defaults.set(convWindowMaximizer, forKey: Keys.convWindowMaximizer) }
    }
    @Published var convDockClickMinimize: Bool {
        didSet { defaults.set(convDockClickMinimize, forKey: Keys.convDockClickMinimize) }
    }
    /// Fn works as Command (see `Keyboard/FnCommand.swift`).
    @Published var convFnCommand: Bool {
        didSet { defaults.set(convFnCommand, forKey: Keys.convFnCommand) }
    }
    @Published var convAutoQuit: Bool {
        didSet { defaults.set(convAutoQuit, forKey: Keys.convAutoQuit) }
    }
    /// Bundle ids opted in to Auto Quit. Empty until the person adds one.
    /// Offer to install an app from a mounted disk image and eject it (on by
    /// default), and move the downloaded .dmg to the Trash afterwards (off).
    @Published var convDiskImageInstaller: Bool {
        didSet { defaults.set(convDiskImageInstaller, forKey: Keys.convDiskImageInstaller) }
    }
    /// Install automatically, with no prompt, when the image holds one signed,
    /// notarized app that is not installed and not running (on by default).
    /// Everything else is asked in the notch.
    @Published var convDiskImageAuto: Bool {
        didSet { defaults.set(convDiskImageAuto, forKey: Keys.convDiskImageAuto) }
    }
    /// Also replace an installed copy with a lower version, with no prompt, when
    /// the image's app is signed, notarized, not running and the same bundle id
    /// (off by default). Undo restores the old copy from the Trash.
    @Published var convDiskImageAutoUpdate: Bool {
        didSet { defaults.set(convDiskImageAutoUpdate, forKey: Keys.convDiskImageAutoUpdate) }
    }
    /// Orthic-Labs/pulse#186: a path field above the front Finder window. Off by default.
    @Published var finderPathField: Bool {
        didSet { defaults.set(finderPathField, forKey: Keys.finderPathField) }
    }
    /// The hub's Diagnostics page (on by default); the Tauri hub reads it from notch-state.json.
    @Published var hubDiagnosticsPage: Bool {
        didSet { defaults.set(hubDiagnosticsPage, forKey: Keys.hubDiagnosticsPage) }
    }
    /// Look for new Claude account folders and offer to bring this computer's chats in (on by default).
    @Published var claudeAccountScan: Bool {
        didSet { defaults.set(claudeAccountScan, forKey: Keys.claudeAccountScan) }
    }
    @Published var convDiskImageTrashDownload: Bool {
        didSet { defaults.set(convDiskImageTrashDownload, forKey: Keys.convDiskImageTrashDownload) }
    }
    @Published var convAutoQuitApps: [String] {
        didSet { defaults.set(convAutoQuitApps, forKey: Keys.convAutoQuitApps) }
    }

    /// Pulse fork: the middle-click tool wheel (Conveniences/ToolWheel.swift).
    /// On by default; off releases the middle button and needs no permission.
    @Published var toolWheelEnabled: Bool {
        didSet { defaults.set(toolWheelEnabled, forKey: "toolWheelEnabled") }
    }

    // MARK: Pulse fork: a switch for every feature (docs/settings.md)

    /// The settings handle (the gear) at the end of the notch. On by default.
    /// Off hides the gear and its update and permission dots; Settings stays
    /// reachable from the hub (a click on a ring opens it) and the Settings shortcut.
    @Published var showsSettingsHandle: Bool {
        didSet { defaults.set(showsSettingsHandle, forKey: "showsSettingsHandle") }
    }
    /// The minimise button at the end of the notch opposite the drag handle (#375). On by
    /// default; off hides it and the notch is never minimised.
    @Published var notchMinimizeButton: Bool {
        didSet { defaults.set(notchMinimizeButton, forKey: GeneratedSettings.Key.notchMinimizeButton) }
    }
    /// Whether the notch is minimised to its Send cell (#375). Internal: set by the button,
    /// kept across relaunches and display changes, one value for every display.
    @Published var notchMinimized: Bool {
        didSet { defaults.set(notchMinimized, forKey: GeneratedSettings.Key.notchMinimized) }
    }
    /// Option-drag slides the notch along its edge. On by default; off leaves Edge and Reset position.
    @Published var notchOptionDrag: Bool {
        didSet { defaults.set(notchOptionDrag, forKey: "notchOptionDrag") }
    }
    /// The card that opens when the pointer rests on a ring. On by default.
    @Published var showsHoverCards: Bool {
        didSet { defaults.set(showsHoverCards, forKey: "showsHoverCards") }
    }
    /// Tool ids (`ToolKit`) switched off; they leave the Tools card and the
    /// wheel and cannot run. Empty by default.
    @Published var disabledTools: [String] {
        didSet {
            defaults.set(disabledTools, forKey: "disabledTools")
            // The wheel's tools changed without being asked for once (#422): every change is kept.
            if disabledTools != oldValue {
                Log.record(.settings, "settings.tools_off_changed", counts: ["from": oldValue.count, "to": disabledTools.count])
            }
        }
    }
    /// The tool ids on the middle-click wheel, in order, clockwise from the top. Its own
    /// setting, apart from the Tools card (`disabledTools`). Default: Snip, Screen,
    /// Window, System Settings, Open hub, Launcher, Clipboard (docs/settings.md); a saved
    /// array is never changed.
    @Published var wheelTools: [String] {
        didSet {
            defaults.set(wheelTools, forKey: "wheelTools")
            if wheelTools != oldValue {
                Log.record(.settings, "settings.wheel_tools_changed", counts: ["from": oldValue.count, "to": wheelTools.count])
            }
        }
    }
    /// The notch opens with a message when a drive's health changes. On by default.
    @Published var announceDriveAlerts: Bool {
        didSet { defaults.set(announceDriveAlerts, forKey: "announceDriveAlerts") }
    }
    /// The notch shows what a remote tool (ChatGPT's files_fetch, chat_start, term, screen) waits
    /// for the owner to approve, until it is answered or expires. On by default.
    @Published var remoteApprovalAlerts: Bool {
        didSet { defaults.set(remoteApprovalAlerts, forKey: "remoteApprovalAlerts") }
    }
    /// A drive alert (a volume filling or almost full, or a health change) also plays the usage-reset chime.
    /// Off by default, as on Windows.
    @Published var driveAlertSound: Bool {
        didSet { defaults.set(driveAlertSound, forKey: "driveAlertSound") }
    }
    /// Pulse watches Claude and Codex sessions for the working state and the
    /// "finished" announcement. On by default; off stops the watchers.
    @Published var watchesAgentSessions: Bool {
        didSet { defaults.set(watchesAgentSessions, forKey: "watchesAgentSessions") }
    }
    /// Launcher: answers to sums and unit conversions. On by default.
    @Published var launcherCalculator: Bool {
        didSet { defaults.set(launcherCalculator, forKey: "launcherCalculator") }
    }
    /// Launcher: Pulse's own commands (open the hub's pages, clear clipboard history). On by default.
    @Published var launcherPulseCommands: Bool {
        didSet { defaults.set(launcherPulseCommands, forKey: "launcherPulseCommands") }
    }
    /// Launcher: typing a snippet's keyword anywhere replaces it with the snippet. Off by default.
    @Published var launcherSnippetExpansion: Bool {
        didSet { defaults.set(launcherSnippetExpansion, forKey: "launcherSnippetExpansion") }
    }
    /// Launcher: who keeps the launcher's key when another app holds it too. "" means the
    /// person has not been asked yet; "pulse" takes the key (needs Accessibility); "other"
    /// stands down and leaves the key to the other app. Asked once per key.
    @Published var launcherHotkeyConflict: String {
        didSet { defaults.set(launcherHotkeyConflict, forKey: "launcherHotkeyConflict") }
    }
    /// Middle-click wheel: who keeps the middle button when another program that remaps it is
    /// running. "ask" (default) asks once per run, the first time the wheel would open; "override"
    /// keeps the button for Pulse; "standDown" gives it back (the wheel's tap comes down).
    @Published var wheelMiddleClickConflict: String {
        didSet { defaults.set(wheelMiddleClickConflict, forKey: "wheelMiddleClickConflict") }
    }
    /// `sendReceivedShelf`: received files stay on the Send card to drag out or open (#388).
    @Published var sendReceivedShelf: Bool {
        didSet { defaults.set(sendReceivedShelf, forKey: "sendReceivedShelf") }
    }
    /// Launcher: where the one Ask row goes (`launcherAskTarget`): inLauncher, chatgpt or
    /// off. Unset means the default (a stored `claude`, removed 2026-10-29, reads as the default).
    @Published var launcherAskTarget: String {
        didSet { defaults.set(launcherAskTarget, forKey: "launcherAskTarget") }
    }
    /// Launcher: the "Search Google" row (named for the engine below) under the results. On by default.
    @Published var launcherWebSearch: Bool {
        didSet { defaults.set(launcherWebSearch, forKey: "launcherWebSearch") }
    }
    /// Launcher: google, bing or duckduckgo. Google by default.
    @Published var launcherWebSearchEngine: String {
        didSet { defaults.set(launcherWebSearchEngine, forKey: "launcherWebSearchEngine") }
    }
    /// The Send ring pulses for a few seconds on a Chat message. On by default.
    @Published var chatRingLight: Bool {
        didSet { defaults.set(chatRingLight, forKey: "chatRingLight") }
    }
    /// Install Pulse Chat into the agent apps found here (Claude Code, Codex, Muse). On by default.
    @Published var chatPluginAutoInstall: Bool {
        didSet { defaults.set(chatPluginAutoInstall, forKey: "chatPluginAutoInstall") }
    }
    /// Copy the signed-in Claude account's chat list to the other accounts. On by default.
    @Published var claudeChatMirror: Bool {
        didSet { defaults.set(claudeChatMirror, forKey: "claudeChatMirror") }
    }
    /// Remember which Claude chats were running so a restart can reopen them. On by default.
    @Published var claudeChatRemember: Bool {
        didSet { defaults.set(claudeChatRemember, forKey: "claudeChatRemember") }
    }
    /// The hub samples drive health every two minutes. On by default.
    @Published var hubDriveHealth: Bool {
        didSet { defaults.set(hubDriveHealth, forKey: "hubDriveHealth") }
    }
    /// The hub keeps 30 minutes of CPU, memory and network history. On by default.
    @Published var hubMetricsHistory: Bool {
        didSet { defaults.set(hubMetricsHistory, forKey: "hubMetricsHistory") }
    }
    /// The hub builds a filename index of each disk for fast search. On by default.
    @Published var hubFileIndex: Bool {
        didSet { defaults.set(hubFileIndex, forKey: "hubFileIndex") }
    }
    /// Manual image and video compression in the hub's Storage (Pulse #202): a separate copy, never the original. Off by default.
    @Published var mediaCompression: Bool {
        didSet { defaults.set(mediaCompression, forKey: "mediaCompression") }
    }
    /// The first-run onboarding has been finished (the hub's onboarding sets it; Settings > General > "Show onboarding again" clears it).
    @Published var onboardingDone: Bool {
        didSet { defaults.set(onboardingDone, forKey: "onboardingDone") }
    }
    /// Storage and Cleanup scan by themselves when opened with nothing saved or
    /// something old. On by default; off shows saved results until you press Scan.
    @Published var hubAutoScan: Bool {
        didSet { defaults.set(hubAutoScan, forKey: "hubAutoScan") }
    }
    /// Once a day the notch counts Chrome's leftover code-signing copies for Cleanup's
    /// daily history. On by default; read straight from the defaults by the sampler.
    @Published var chromeSnapshotCount: Bool {
        didSet { defaults.set(chromeSnapshotCount, forKey: "chromeSnapshotCount") }
    }
    /// The three items Pulse's Finder extension adds to the right-click menu.
    /// On by default; published to the extension by `FinderMenuState`.
    @Published var finderMenuCut: Bool {
        didSet { defaults.set(finderMenuCut, forKey: "finderMenuCut") }
    }
    @Published var finderMenuCopyPath: Bool {
        didSet { defaults.set(finderMenuCopyPath, forKey: "finderMenuCopyPath") }
    }
    @Published var finderMenuTerminal: Bool {
        didSet { defaults.set(finderMenuTerminal, forKey: "finderMenuTerminal") }
    }
    /// Remote tools (docs/settings.md, "Remote tools"): read-only file and chat access for
    /// agents on this computer, enforced by the `pulse` CLI and the engine from these values.
    /// Off by default; no folder is approved and no chat is selected until chosen.
    @Published var remoteTools: Bool {
        didSet { defaults.set(remoteTools, forKey: "remoteTools") }
    }
    /// The approved folders, absolute paths. Empty by default.
    @Published var remoteRoots: [String] {
        didSet { defaults.set(remoteRoots, forKey: "remoteRoots") }
    }
    /// The chats a remote reader may read, as `<id>` or `<claude|codex>:<id>`. Empty by default.
    @Published var remoteChatRead: [String] {
        didSet { defaults.set(remoteChatRead, forKey: "remoteChatRead") }
    }
    /// The owner's own paired phones may read every chat (`chat.read`); on by default. Off: a phone
    /// reads only the chats in `remoteChatRead`. Never applies to a guest, the gateway or agent tools.
    @Published var phoneChatReadAll: Bool {
        didSet { defaults.set(phoneChatReadAll, forKey: GeneratedSettings.Key.phoneChatReadAll) }
    }
    /// Caps on one remote call. Defaults: 1 MiB read, 200 find results, 50 chat turns, 256 KiB of
    /// chat text, 10 MiB image.
    @Published var remoteReadMaxBytes: Int {
        didSet { defaults.set(remoteReadMaxBytes, forKey: "remoteReadMaxBytes") }
    }
    @Published var remoteFindMaxResults: Int {
        didSet { defaults.set(remoteFindMaxResults, forKey: "remoteFindMaxResults") }
    }
    @Published var remoteChatReadMaxTurns: Int {
        didSet { defaults.set(remoteChatReadMaxTurns, forKey: "remoteChatReadMaxTurns") }
    }
    @Published var remoteChatReadMaxBytes: Int {
        didSet { defaults.set(remoteChatReadMaxBytes, forKey: "remoteChatReadMaxBytes") }
    }
    @Published var remoteImageMaxBytes: Int {
        didSet { defaults.set(remoteImageMaxBytes, forKey: "remoteImageMaxBytes") }
    }
    /// The ChatGPT gateway (`pulse-gateway`, docs/chatgpt-gateway.md): lets OpenAI's tunnel
    /// reach the remote tools above. Needs `remoteTools` as well. Off by default.
    @Published var remoteGateway: Bool {
        didSet { defaults.set(remoteGateway, forKey: "remoteGateway") }
    }
    /// Document tools (`pulse docs read`, the engine's `docs.read`): lets an agent read the text of a
    /// PDF inside the approved folders. Needs `remoteTools` as well. Off by default.
    @Published var remoteDocs: Bool {
        didSet { defaults.set(remoteDocs, forKey: "remoteDocs") }
    }
    /// The largest document file `docs.read` opens, in bytes. Default 20 MiB, at most 64 MiB.
    @Published var remoteDocsMaxBytes: Int {
        didSet { defaults.set(remoteDocsMaxBytes, forKey: "remoteDocsMaxBytes") }
    }
    /// Lets `files.patch` and `files.manage` change files in the approved folders, one local
    /// approval per change. Needs `remoteTools` as well. Off by default.
    @Published var remoteFilesWrite: Bool {
        didSet { defaults.set(remoteFilesWrite, forKey: "remoteFilesWrite") }
    }
    /// Lets `term.*` and `procs.*` run terminal commands and stop processes, through a local
    /// approval. Needs `remoteTools` as well. Off by default.
    @Published var remoteTerminals: Bool {
        didSet { defaults.set(remoteTerminals, forKey: "remoteTerminals") }
    }
    /// The terminal family needs no per-session approval (a standing policy). Off by default; the
    /// 30-minute idle expiry still applies.
    @Published var remoteTermStanding: Bool {
        didSet { defaults.set(remoteTermStanding, forKey: "remoteTermStanding") }
    }
    /// The programs `term.exec` may start (an empty list is no restriction). Owner-only.
    @Published var remoteTermAllow: [String] {
        didSet { defaults.set(remoteTermAllow, forKey: "remoteTermAllow") }
    }
    /// The programs `term.exec` refuses. Owner-only.
    @Published var remoteTermDeny: [String] {
        didSet { defaults.set(remoteTermDeny, forKey: "remoteTermDeny") }
    }
    /// Lets `clipboard.read` act, with a local approval each time. Owner-only. Off by default.
    @Published var remoteClipboard: Bool {
        didSet { defaults.set(remoteClipboard, forKey: "remoteClipboard") }
    }
    /// The gateway refuses every tool that changes something. Owner-only. Off by default.
    @Published var remoteReadOnly: Bool {
        didSet { defaults.set(remoteReadOnly, forKey: "remoteReadOnly") }
    }
    /// Membrane's ChatGPT and Codex history index (#431); owner-only, off by default.
    @Published var ledgerTranscriptSearch: Bool {
        didSet { defaults.set(ledgerTranscriptSearch, forKey: GeneratedSettings.Key.ledgerTranscriptSearch) }
    }
    /// Lets a paired computer search the chats `remoteChatRead` allows (`ledger.history`); owner-only, off by default.
    @Published var ledgerTranscriptShare: Bool {
        didSet { defaults.set(ledgerTranscriptShare, forKey: GeneratedSettings.Key.ledgerTranscriptShare) }
    }
    /// Lets `screen.observe` read the windows of any app that is not in `remoteBlockedApps` (their
    /// accessibility tree and, on request, a screenshot). Needs `remoteTools` and
    /// `remoteScreenDisclosure` as well, and
    /// the Accessibility permission (Screen Recording for a screenshot), which is never prompted for
    /// from a remote call. Off by default.
    @Published var remoteScreenObserve: Bool {
        didSet { defaults.set(remoteScreenObserve, forKey: "remoteScreenObserve") }
    }
    /// Lets `screen.act` press, type and click in any app that is not blocked. The local agent and
    /// the owner are asked only for what sends, buys or publishes (and a delete); the ChatGPT
    /// gateway is asked for every action. Its own switch, tier 3. Needs the same grants as
    /// `remoteScreenObserve`. Off by default.
    @Published var remoteScreenAct: Bool {
        didSet { defaults.set(remoteScreenAct, forKey: "remoteScreenAct") }
    }
    /// The apps no remote tool may launch, observe or act on, as bundle ids (a block list).
    /// Added with the picker or by dropping a .app in the hub. Empty by default.
    @Published var remoteBlockedApps: [String] {
        didSet { defaults.set(remoteBlockedApps, forKey: "remoteBlockedApps") }
    }
    /// The disclosure grant: what an approved window shows (text, values, a screenshot) goes to the
    /// remote caller. Off by default.
    @Published var remoteScreenDisclosure: Bool {
        didSet { defaults.set(remoteScreenDisclosure, forKey: "remoteScreenDisclosure") }
    }
    /// Lets `apps.launch` open any app by bundle id except the ones in `remoteBlockedApps`; no
    /// per-app approval for the local agent (the gateway still asks once per app). Needs
    /// `remoteTools` as well. Off by default.
    @Published var remoteLaunch: Bool {
        didSet { defaults.set(remoteLaunch, forKey: "remoteLaunch") }
    }
    /// Lets `media.control` play, pause, skip and search-and-play in Spotify and Music. Needs
    /// `remoteTools` as well. Off by default.
    @Published var remoteMedia: Bool {
        didSet { defaults.set(remoteMedia, forKey: "remoteMedia") }
    }
    /// Per-chat unlock: the first effectful tool call from an external chat waits for the owner to
    /// authenticate (Touch ID, the login password, or the Pulse PIN). On by default; the engine
    /// decides, the notch asks (`ChatUnlockPanel`).
    @Published var chatUnlock: Bool {
        didSet { defaults.set(chatUnlock, forKey: "chatUnlock") }
    }
    /// Minutes an unlock lasts, 0 to 10080 (a week); 0 means until the chat ends.
    @Published var chatUnlockMinutes: Int {
        didSet { defaults.set(chatUnlockMinutes, forKey: "chatUnlockMinutes") }
    }
    /// Lets `screen.act` type the user's own non-sensitive details (`remoteFormFillProfile`) into
    /// forms in any app that is not blocked (the gateway after a standing grant per app). Passwords, card numbers,
    /// bank details and government IDs are never typed. Off by default.
    @Published var remoteFormFill: Bool {
        didSet { defaults.set(remoteFormFill, forKey: "remoteFormFill") }
    }
    /// The apps (a bundle id, or `app::title fragment` for a site) where the gateway's form fill
    /// needs no question; the local agent is never asked.
    @Published var remoteFormFillStanding: [String] {
        didSet { defaults.set(remoteFormFillStanding, forKey: "remoteFormFillStanding") }
    }
    /// The user's own details for form fill, each `field=value` (name, email, phone, address),
    /// typed here by the user and never read from anywhere else.
    @Published var remoteFormFillProfile: [String] {
        didSet { defaults.set(remoteFormFillProfile, forKey: "remoteFormFillProfile") }
    }
    /// Saved logins and virtual cards for agents (`login.fill`, `payment.fill`; the Secure box). Each
    /// needs `remoteTools` as well. Off by default. Owner-only (`remoteFormFill` prefix).
    @Published var remoteFormFillLogins: Bool {
        didSet { defaults.set(remoteFormFillLogins, forKey: "remoteFormFillLogins") }
    }
    @Published var remoteFormFillPayments: Bool {
        didSet { defaults.set(remoteFormFillPayments, forKey: "remoteFormFillPayments") }
    }
    /// Password managers an agent may start, `name=key chord` per line. Empty by default.
    @Published var remoteFormFillLoginManagers: [String] {
        didSet { defaults.set(remoteFormFillLoginManagers, forKey: "remoteFormFillLoginManagers") }
    }
    /// The global payment caps in US cents over every saved card: the largest purchase approved
    /// without Touch ID (2500) and the calendar-month total (10000). The engine clamps to 10,000,000.
    @Published var remoteFormFillPaymentsAutoCapCents: Int {
        didSet { defaults.set(remoteFormFillPaymentsAutoCapCents, forKey: "remoteFormFillPaymentsAutoCapCents") }
    }
    @Published var remoteFormFillPaymentsMonthCapCents: Int {
        didSet { defaults.set(remoteFormFillPaymentsMonthCapCents, forKey: "remoteFormFillPaymentsMonthCapCents") }
    }
    /// Lets an agent hand the owner one region of a window to finish (a login, a payment
    /// confirmation, a CAPTCHA). Owner-only (`remoteScreen` prefix). Off by default.
    @Published var remoteScreenSpotTakeover: Bool {
        didSet { defaults.set(remoteScreenSpotTakeover, forKey: "remoteScreenSpotTakeover") }
    }
    /// Hub health (docs/settings.md, "Hub background"): the resource watchdog, the hub log and the
    /// macOS report watch. On by default. The engine reads each one from notch-state.json.
    @Published var hubWatchdog: Bool {
        didSet { defaults.set(hubWatchdog, forKey: "hubWatchdog") }
    }
    @Published var hubLog: Bool {
        didSet { defaults.set(hubLog, forKey: "hubLog") }
    }
    @Published var hubDiagReports: Bool {
        didSet { defaults.set(hubDiagReports, forKey: "hubDiagReports") }
    }
    /// Watchdog budgets: CPU percent of one core, footprint MB, threads, writes MB per hour, and the
    /// sustained window in minutes. Defaults 5, 1024, 200, 500 and 10.
    @Published var hubWatchdogCpuPercent: Int {
        didSet { defaults.set(hubWatchdogCpuPercent, forKey: "hubWatchdogCpuPercent") }
    }
    @Published var hubWatchdogMinutes: Int {
        didSet { defaults.set(hubWatchdogMinutes, forKey: "hubWatchdogMinutes") }
    }
    @Published var hubWatchdogFootprintMb: Int {
        didSet { defaults.set(hubWatchdogFootprintMb, forKey: "hubWatchdogFootprintMb") }
    }
    @Published var hubWatchdogThreads: Int {
        didSet { defaults.set(hubWatchdogThreads, forKey: "hubWatchdogThreads") }
    }
    @Published var hubWatchdogWriteMbPerHour: Int {
        didSet { defaults.set(hubWatchdogWriteMbPerHour, forKey: "hubWatchdogWriteMbPerHour") }
    }
    /// The file-index pause ladder (docs/settings.md, "Index file names"): on by default; trips
    /// before the index is switched off (3), the first pause in minutes (5), and its budgets.
    @Published var hubFileIndexAutoPause: Bool {
        didSet { defaults.set(hubFileIndexAutoPause, forKey: "hubFileIndexAutoPause") }
    }
    @Published var hubFileIndexMaxTrips: Int {
        didSet { defaults.set(hubFileIndexMaxTrips, forKey: "hubFileIndexMaxTrips") }
    }
    @Published var hubFileIndexPauseMinutes: Int {
        didSet { defaults.set(hubFileIndexPauseMinutes, forKey: "hubFileIndexPauseMinutes") }
    }
    @Published var hubFileIndexMaxCpuPercent: Int {
        didSet { defaults.set(hubFileIndexMaxCpuPercent, forKey: "hubFileIndexMaxCpuPercent") }
    }
    @Published var hubFileIndexMaxFootprintMb: Int {
        didSet { defaults.set(hubFileIndexMaxFootprintMb, forKey: "hubFileIndexMaxFootprintMb") }
    }
    @Published var hubFileIndexMaxWriteMbPerHour: Int {
        didSet { defaults.set(hubFileIndexMaxWriteMbPerHour, forKey: "hubFileIndexMaxWriteMbPerHour") }
    }
    @Published var hubFileIndexWholeDisk: Bool {
        didSet { defaults.set(hubFileIndexWholeDisk, forKey: "hubFileIndexWholeDisk") }
    }
    @Published var hubFileIndexVolumes: [String] {
        didSet { defaults.set(hubFileIndexVolumes, forKey: "hubFileIndexVolumes") }
    }
    @Published var hubFileIndexExcludes: [String] {
        didSet { defaults.set(hubFileIndexExcludes, forKey: "hubFileIndexExcludes") }
    }

    /// Nearby sharing (LocalSend protocol, run by the hub): on by default.
    @Published var nearbyEnabled: Bool {
        didSet { defaults.set(nearbyEnabled, forKey: Keys.nearbyEnabled) }
    }

    /// Lets the resident engine answer `agent.request` tasks (`pulse task ask`, voice hand-offs) with
    /// one bounded run of the user's own Claude Code CLI, which uses his Claude subscription. Default
    /// off: every request spends his quota.
    @Published var agentRequests: Bool {
        didSet { defaults.set(agentRequests, forKey: Keys.agentRequests) }
    }
    /// Lets the resident engine run `agent.act` tasks (`pulse task act`, voice hand-offs while it is
    /// on): Claude Code with only the Pulse tools that are switched on (files, terminals, screen),
    /// every act approved by the user in Pulse. Default off: it spends his Claude subscription quota
    /// and can change this computer once he approves.
    @Published var agentActions: Bool {
        didSet { defaults.set(agentActions, forKey: Keys.agentActions) }
    }
    /// Path to the `claude` executable the agent runs. Empty means automatic: the engine looks in a
    /// fixed list of install locations, then on PATH.
    @Published var agentCliPath: String {
        didSet { defaults.set(agentCliPath, forKey: Keys.agentCliPath) }
    }
    /// Accepts device links on the LAN (the resident engine's `deviceLinkListen`). Default off:
    /// while off nothing listens and no pairing channel is open.
    @Published var deviceLinkListen: Bool {
        didSet { defaults.set(deviceLinkListen, forKey: Keys.deviceLinkListen) }
    }
    /// The relay the device link uses when the other computer is not on this network (the
    /// engine's `deviceLinkRelayUrl`). Empty means no relay.
    @Published var deviceLinkRelayUrl: String {
        didSet { defaults.set(deviceLinkRelayUrl, forKey: GeneratedSettings.Key.deviceLinkRelayUrl) }
    }
    /// How the relay admits devices (`deviceLinkRelayAdmission`): off, allowlist or callback.
    @Published var deviceLinkRelayAdmission: String {
        didSet { defaults.set(deviceLinkRelayAdmission, forKey: GeneratedSettings.Key.deviceLinkRelayAdmission) }
    }
    /// Find the user's other computers on this network (the mesh), as well as through a relay. On by default.
    @Published var deviceMeshLocal: Bool {
        didSet { defaults.set(deviceMeshLocal, forKey: GeneratedSettings.Key.deviceMeshLocal) }
    }
    /// Send files to the user's other computers over the RightKit mesh as well as nearby sharing.
    /// On by default.
    @Published var sendOverMesh: Bool {
        didSet { defaults.set(sendOverMesh, forKey: GeneratedSettings.Key.sendOverMesh) }
    }
    /// Agent files (`pulse chat send --file`, #592) from the user's own paired computers are saved
    /// without the Accept prompt. On by default; the engine reads it from the published settings.
    @Published var agentFileAutoAccept: Bool {
        didSet { defaults.set(agentFileAutoAccept, forKey: GeneratedSettings.Key.agentFileAutoAccept) }
    }
    /// Chat over a paired computer's device link (`chatOverDeviceLink`; the engine's `chat_enabled`). On by default.
    @Published var chatOverDeviceLink: Bool {
        didSet { defaults.set(chatOverDeviceLink, forKey: GeneratedSettings.Key.chatOverDeviceLink) }
    }
    /// Paired phones may read and message my chats (`devicePhoneChats`). On by default.
    @Published var devicePhoneChats: Bool {
        didSet { defaults.set(devicePhoneChats, forKey: GeneratedSettings.Key.devicePhoneChats) }
    }
    /// What a newly paired phone may do with my chat list (`devicePhoneRead`): allow or deny.
    @Published var devicePhoneRead: String {
        didSet { defaults.set(devicePhoneRead, forKey: GeneratedSettings.Key.devicePhoneRead) }
    }
    /// What a newly paired phone may send to my chats (`devicePhoneSend`): allow, ask or deny.
    @Published var devicePhoneSend: String {
        didSet { defaults.set(devicePhoneSend, forKey: GeneratedSettings.Key.devicePhoneSend) }
    }
    /// The owner's own paired phone's chat messages are typed into the chat's composer (`devicePhoneTypeIntoChat`, #598). On by default.
    @Published var devicePhoneTypeIntoChat: Bool {
        didSet { defaults.set(devicePhoneTypeIntoChat, forKey: GeneratedSettings.Key.devicePhoneTypeIntoChat) }
    }
    /// Paired phones may pick up items I send them (`devicePhoneReceive`). On by default.
    @Published var devicePhoneReceive: Bool {
        didSet { defaults.set(devicePhoneReceive, forKey: GeneratedSettings.Key.devicePhoneReceive) }
    }
    /// Minutes an item waits for its phone (`devicePhoneReceiveTtlMinutes`), 1 to 1440.
    @Published var devicePhoneReceiveTtlMinutes: Int {
        didSet { defaults.set(devicePhoneReceiveTtlMinutes, forKey: GeneratedSettings.Key.devicePhoneReceiveTtlMinutes) }
    }
    /// The account chips on the Claude card (`claudeAccountSwitcher`). Read by ClaudeChatSync under the same key.
    @Published var claudeAccountSwitcher: Bool {
        didSet { defaults.set(claudeAccountSwitcher, forKey: GeneratedSettings.Key.claudeAccountSwitcher) }
    }
    /// Cleanup also offers developer caches, build output and old release builds (`cleanupDeveloperItems`).
    @Published var cleanupDeveloperItems: Bool {
        didSet { defaults.set(cleanupDeveloperItems, forKey: GeneratedSettings.Key.cleanupDeveloperItems) }
    }
    /// Restart the engine when its memory runs away (`engineMemoryGuard`). On by default.
    @Published var engineMemoryGuard: Bool {
        didSet { defaults.set(engineMemoryGuard, forKey: GeneratedSettings.Key.engineMemoryGuard) }
    }
    /// The engine memory cap in MB (`engineMemoryCapMb`), 512 to 65536.
    @Published var engineMemoryCapMb: Int {
        didSet { defaults.set(engineMemoryCapMb, forKey: GeneratedSettings.Key.engineMemoryCapMb) }
    }
    /// Enrol git repositories in Blueprint automatically (`membraneAutoEnroll`, read by the engine's
    /// `membrane_enroll.rs`). On by default. Not yet in GeneratedSettings, so the key is literal here.
    @Published var membraneAutoEnroll: Bool {
        didSet { defaults.set(membraneAutoEnroll, forKey: Keys.membraneAutoEnroll) }
    }
    /// `sendDefaultDevice`: the paired device a paste on the Send cell goes to (device id); empty
    /// means the only other paired computer, or ask (#471).
    @Published var sendDefaultDevice: String {
        didSet { defaults.set(sendDefaultDevice, forKey: GeneratedSettings.Key.sendDefaultDevice) }
    }
    /// Which agent route answers `pulse task ask` and voice requests. `claude-code` by default.
    @Published var agentRoute: String {
        didSet { defaults.set(agentRoute, forKey: GeneratedSettings.Key.agentRoute) }
    }
    /// How other devices list this Mac; empty means "<Mac name> (Pulse)".
    @Published var nearbyAlias: String {
        didSet { defaults.set(nearbyAlias, forKey: Keys.nearbyAlias) }
    }
    /// Where received files go; empty means Downloads.
    @Published var nearbySaveFolder: String {
        didSet { defaults.set(nearbySaveFolder, forKey: Keys.nearbySaveFolder) }
    }

    /// Pulse fork: the launcher is off until switched on.
    @Published var launcherEnabled: Bool {
        didSet { defaults.set(launcherEnabled, forKey: Keys.launcherEnabled) }
    }

    @Published var launcherHotkey: LauncherHotkeyChoice {
        didSet { defaults.set(launcherHotkey.rawValue, forKey: Keys.launcherHotkey) }
    }

    /// Launcher items the hub edits: pinned apps, file folders, app and command
    /// hotkeys, quicklinks, snippets, commands. One JSON string; see `LauncherConfig`.
    @Published var launcherConfigJSON: String {
        didSet { defaults.set(launcherConfigJSON, forKey: Keys.launcherConfig) }
    }
    /// Launcher sources, each off or on in the hub. Clipboard and currency are
    /// off until chosen: one stores what you copy, the other fetches rates.
    @Published var launcherClipboard: Bool {
        didSet { defaults.set(launcherClipboard, forKey: Keys.launcherClipboard) }
    }
    /// The clipboard manager's switches (docs/clipboard-spec.md 8.1). It is standalone: none
    /// of these need the launcher. `launcherClipboard` above is the history's master switch.
    @Published var clipboardStripEnabled: Bool {
        didSet { defaults.set(clipboardStripEnabled, forKey: "clipboardStripEnabled") }
    }
    /// 7.12 grammar: modifier names joined by `+`, then one key.
    @Published var clipboardHotkey: String {
        didSet { defaults.set(clipboardHotkey, forKey: "clipboardHotkey") }
    }
    /// `small`, `medium` or `large`.
    @Published var clipboardCardSize: String {
        didSet { defaults.set(clipboardCardSize, forKey: "clipboardCardSize") }
    }
    /// `notch` or `bottom`.
    @Published var clipboardStripPlacement: String {
        didSet { defaults.set(clipboardStripPlacement, forKey: "clipboardStripPlacement") }
    }
    @Published var clipboardShowNumbers: Bool {
        didSet { defaults.set(clipboardShowNumbers, forKey: "clipboardShowNumbers") }
    }
    /// `paste` or `copy`.
    @Published var clipboardPasteMode: String {
        didSet { defaults.set(clipboardPasteMode, forKey: "clipboardPasteMode") }
    }
    @Published var clipboardPlainDefault: Bool {
        didSet { defaults.set(clipboardPlainDefault, forKey: "clipboardPlainDefault") }
    }
    @Published var clipboardCaptureImages: Bool {
        didSet { defaults.set(clipboardCaptureImages, forKey: "clipboardCaptureImages") }
    }
    @Published var clipboardCaptureFiles: Bool {
        didSet { defaults.set(clipboardCaptureFiles, forKey: "clipboardCaptureFiles") }
    }
    @Published var clipboardCaptureRich: Bool {
        didSet { defaults.set(clipboardCaptureRich, forKey: "clipboardCaptureRich") }
    }
    /// 50 to 5000.
    @Published var clipboardMaxItems: Int {
        didSet { defaults.set(clipboardMaxItems, forKey: "clipboardMaxItems") }
    }
    /// 20 to 4096.
    @Published var clipboardMaxMB: Int {
        didSet { defaults.set(clipboardMaxMB, forKey: "clipboardMaxMB") }
    }
    /// 0 (forever) to 365.
    @Published var clipboardMaxAgeDays: Int {
        didSet { defaults.set(clipboardMaxAgeDays, forKey: "clipboardMaxAgeDays") }
    }
    /// 0 (until a size cap) to 720.
    @Published var clipboardScreenshotHours: Int {
        didSet { defaults.set(clipboardScreenshotHours, forKey: "clipboardScreenshotHours") }
    }
    @Published var clipboardSkipAutoGenerated: Bool {
        didSet { defaults.set(clipboardSkipAutoGenerated, forKey: "clipboardSkipAutoGenerated") }
    }
    @Published var clipboardSkipRemote: Bool {
        didSet { defaults.set(clipboardSkipRemote, forKey: "clipboardSkipRemote") }
    }
    @Published var clipboardSecretFilter: Bool {
        didSet { defaults.set(clipboardSecretFilter, forKey: "clipboardSecretFilter") }
    }
    /// Seal the clipboard history at rest with a Keychain key (docs/clipboard-spec.md 9). On by default.
    @Published var clipboardEncryption: Bool {
        didSet { defaults.set(clipboardEncryption, forKey: "clipboardEncryption") }
    }
    /// Bundle ids whose copies are never kept (spec 8.2 lists the default).
    @Published var clipboardExcludedApps: [String] {
        didSet { defaults.set(clipboardExcludedApps, forKey: "clipboardExcludedApps") }
    }
    /// Extra pasteboard types that make Pulse skip a copy. Empty by default.
    @Published var clipboardIgnoredTypes: [String] {
        didSet { defaults.set(clipboardIgnoredTypes, forKey: "clipboardIgnoredTypes") }
    }
    @Published var clipboardRememberBoard: Bool {
        didSet { defaults.set(clipboardRememberBoard, forKey: "clipboardRememberBoard") }
    }
    /// `clipboardHideFromCapture`: the strip stays out of screenshots and screen shares. On by default.
    @Published var clipboardHideFromCapture: Bool {
        didSet { defaults.set(clipboardHideFromCapture, forKey: "clipboardHideFromCapture") }
    }
    /// `clipboardRestoreAfterPaste`: after a paste, the previous clipboard goes back 1.5 s later. Off by default.
    @Published var clipboardRestoreAfterPaste: Bool {
        didSet { defaults.set(clipboardRestoreAfterPaste, forKey: "clipboardRestoreAfterPaste") }
    }
    /// `clipboardWheelRing` (#300): resting on Clipboard on the wheel opens a ring of the last 8 items, pinned first. On by default.
    @Published var clipboardWheelRing: Bool {
        didSet { defaults.set(clipboardWheelRing, forKey: "clipboardWheelRing") }
    }
    /// `clipboardCopyBurst` (#387): copies under 60 s apart show as dots on the Send cell and paste in order.
    @Published var clipboardCopyBurst: Bool {
        didSet { defaults.set(clipboardCopyBurst, forKey: "clipboardCopyBurst") }
    }
    /// What the store and the strip read, built from the keys above.
    var clipboardSettings: ClipSettings {
        var settings = ClipSettings()
        settings.enabled = launcherClipboard
        settings.stripEnabled = clipboardStripEnabled
        settings.hotkey = clipboardHotkey
        settings.cardSize = ClipCardSize(rawValue: clipboardCardSize) ?? .medium
        settings.placement = ClipPlacement(rawValue: clipboardStripPlacement) ?? .notch
        settings.showNumbers = clipboardShowNumbers
        settings.pasteMode = ClipPasteMode(rawValue: clipboardPasteMode) ?? .paste
        settings.plainDefault = clipboardPlainDefault
        settings.captureImages = clipboardCaptureImages
        settings.captureFiles = clipboardCaptureFiles
        settings.captureRich = clipboardCaptureRich
        settings.maxItems = clipboardMaxItems
        settings.maxMB = clipboardMaxMB
        settings.maxAgeDays = clipboardMaxAgeDays
        settings.screenshotHours = clipboardScreenshotHours
        settings.skipAutoGenerated = clipboardSkipAutoGenerated
        settings.skipRemote = clipboardSkipRemote
        settings.secretFilter = clipboardSecretFilter
        settings.encrypt = clipboardEncryption
        settings.excludedApps = clipboardExcludedApps
        settings.ignoredTypes = clipboardIgnoredTypes
        settings.rememberBoard = clipboardRememberBoard
        settings.hideFromCapture = clipboardHideFromCapture
        settings.restoreAfterPaste = clipboardRestoreAfterPaste
        settings.copyBurst = clipboardCopyBurst
        return settings
    }
    @Published var launcherCurrency: Bool {
        didSet { defaults.set(launcherCurrency, forKey: Keys.launcherCurrency) }
    }
    @Published var launcherDictionary: Bool {
        didSet { defaults.set(launcherDictionary, forKey: Keys.launcherDictionary) }
    }
    @Published var launcherShortcuts: Bool {
        didSet { defaults.set(launcherShortcuts, forKey: Keys.launcherShortcuts) }
    }

    /// Pulse fork: window-management shortcuts (`Conveniences/Windows/`), off
    /// until switched on. `windowHotkeys` maps an action id to its shortcut
    /// text; a missing id means the action's default, "" means none.
    @Published var windowManagementEnabled: Bool {
        didSet { defaults.set(windowManagementEnabled, forKey: Keys.windowManagementEnabled) }
    }

    @Published var windowHotkeys: [String: String] {
        didSet { defaults.set(windowHotkeys, forKey: Keys.windowHotkeys) }
    }

    /// Pulse fork: the global shortcut that opens the hub's Settings
    /// (`Conveniences/Windows/SettingsHotKey.swift`), as shortcut text such as
    /// `cmd+shift+,`. "" means none, which is the default.
    @Published var settingsHotkey: String {
        didSet { defaults.set(settingsHotkey, forKey: Keys.settingsHotkey) }
    }

    /// Whether Claude's big ring shows the day's share of the weekly limit
    /// instead of the session. See `DailyPace`.
    @Published var claudeDailyPaceRing: Bool {
        didSet { defaults.set(claudeDailyPaceRing, forKey: Keys.claudeDailyPaceRing) }
    }

    /// Whether the big ring shows the weekly limit instead of the shorter
    /// window, for every provider that has both. See `WeeklyHeadline`.
    @Published var weeklyHeadline: Bool {
        didSet { defaults.set(weeklyHeadline, forKey: Keys.weeklyHeadline) }
    }

    /// Whether Spark and code-review Codex windows appear in the hover card.
    /// On by default so a first launch shows them; the ring still follows
    /// the main Codex window either way.
    @Published var showCodexExtraLimits: Bool {
        didSet { defaults.set(showCodexExtraLimits, forKey: Keys.showCodexExtraLimits) }
    }

    /// Whether the weekly limit gets a ring of its own, and where it sits.
    /// Whether each ring carries its percentage under it, on every edge.
    ///
    /// On by default, which is what the notch has always drawn everywhere but
    /// the strip beside a Mac's own cutout. There it costs ring size, because
    /// the bar is the cutout's depth and one ring already fills it — see
    /// `NotchViewModel.showsCellReading`.
    @Published var showsNotchReadings: Bool {
        didSet { defaults.set(showsNotchReadings, forKey: Keys.showsNotchReadings) }
    }

    @Published var weeklyRingDashed: Bool {
        didSet { defaults.set(weeklyRingDashed, forKey: Keys.weeklyRingDashed) }
    }

    /// Whether the reading under each ring adds the weekly ring's percentage,
    /// as "30%/70%". Only while the weekly ring is on.
    @Published var weeklyReading: Bool {
        didSet { defaults.set(weeklyReading, forKey: Keys.weeklyReading) }
    }

    /// One Claude ring per known account (#454); off shows only the signed-in account's.
    @Published var claudeShowAllAccounts: Bool {
        didSet { defaults.set(claudeShowAllAccounts, forKey: Keys.claudeShowAllAccounts) }
    }

    @Published var weeklyRing: WeeklyRing {
        didSet { defaults.set(weeklyRing.rawValue, forKey: Keys.weeklyRing) }
    }


    /// The colour used for positive usage and active-work indicators.
    @Published var accentColor: AccentColorChoice {
        didSet { defaults.set(accentColor.rawValue, forKey: Keys.accentColor) }
    }

    /// The material the expanded notch, tooltip and settings orb are painted with.
    @Published var notchSurfaceStyle: NotchSurfaceStyle {
        didSet { defaults.set(notchSurfaceStyle.rawValue, forKey: Keys.notchSurfaceStyle) }
    }

    @Published var watchLimit: Double {
        didSet {
            let clamped = min(max(watchLimit, 0.01), criticalLimit - 0.01)
            if clamped != watchLimit { watchLimit = clamped; return }
            defaults.set(watchLimit, forKey: Keys.watchLimit)
        }
    }

    @Published var criticalLimit: Double {
        didSet {
            let clamped = min(max(criticalLimit, watchLimit + 0.01), 1.0)
            if clamped != criticalLimit { criticalLimit = clamped; return }
            defaults.set(criticalLimit, forKey: Keys.criticalLimit)
        }
    }

    /// Hard step or continuous ramp — see `ColorTransitionStyle`.
    @Published var colorTransitionStyle: ColorTransitionStyle {
        didSet { defaults.set(colorTransitionStyle.rawValue, forKey: Keys.colorTransitionStyle) }
    }

    /// The language the app itself speaks.
    ///
    /// `.system` follows the Mac. Written through `L10n.apply` so the store
    /// and the change notification stay a single write.
    @Published var language: AppLanguage {
        didSet { L10n.apply(language) }
    }

    /// Where every notification goes: the notch, or a banner. One choice for
    /// all of them; which events notify stays a switch per event.
    @Published var notificationChannel: NotificationChannel {
        didSet { defaults.set(notificationChannel.rawValue, forKey: Keys.notificationChannel) }
    }

    /// The default alert sound with a banner (the Mac channel only). Off by default, like every
    /// other alert sound: the banner shows, and it is silent unless this is on.
    @Published var channelBannerSound: Bool {
        didSet { defaults.set(channelBannerSound, forKey: Keys.channelBannerSound) }
    }

    /// Open the notch for a few seconds when an agent stops working.
    ///
    /// On by default: the app already knows the moment a session ends, and a
    /// user who installed a thing that watches sessions is unlikely to want
    /// that particular fact kept from them. It is a peek, not a notification —
    /// nothing to dismiss, and it takes no focus.
    @Published var announceSessionEnd: Bool {
        didSet { defaults.set(announceSessionEnd, forKey: Keys.announceSessionEnd) }
    }

    /// How long that peek lasts.
    @Published var peekDuration: PeekDuration {
        didSet { defaults.set(peekDuration.rawValue, forKey: Keys.peekDuration) }
    }

    /// Sound the system alert alongside the peek.
    ///
    /// Separate from the peek because they fail differently: the peek is no use
    /// on another Space or behind a full-screen window, and the sound is no use
    /// in a meeting. Kept switchable on its own so neither one forces the
    /// other.
    @Published var sessionEndSound: Bool {
        didSet { defaults.set(sessionEndSound, forKey: Keys.sessionEndSound) }
    }

    /// Which sound a finished turn makes.
    @Published var sessionEndSoundName: String {
        didSet { defaults.set(sessionEndSoundName, forKey: Keys.sessionEndSoundName) }
    }

    /// And which one a session blocked on you makes.
    ///
    /// A separate choice because the two say different things — one is "that's
    /// done", the other is "you are the hold-up" — and a single sound for both
    /// makes the second one easy to ignore.
    @Published var sessionBlockedSoundName: String {
        didSet { defaults.set(sessionBlockedSoundName, forKey: Keys.sessionBlockedSoundName) }
    }

    /// Show a notification modal from the notch when a provider's limit resets.
    @Published var announceUsageReset: Bool {
        didSet { defaults.set(announceUsageReset, forKey: Keys.announceUsageReset) }
    }

    /// Sound an alert alongside the usage reset notification modal.
    @Published var usageResetSound: Bool {
        didSet { defaults.set(usageResetSound, forKey: Keys.usageResetSound) }
    }

    /// Which sound a usage reset notification makes.
    @Published var usageResetSoundName: String {
        didSet { defaults.set(usageResetSoundName, forKey: Keys.usageResetSoundName) }
    }

    /// Show a notification modal from the notch when a provider's session limit is reached.
    @Published var announceSessionLimitReached: Bool {
        didSet { defaults.set(announceSessionLimitReached, forKey: Keys.announceSessionLimitReached) }
    }

    /// Show a notification modal from the notch when a provider's weekly limit is reached.
    @Published var announceWeeklyLimitReached: Bool {
        didSet { defaults.set(announceWeeklyLimitReached, forKey: Keys.announceWeeklyLimitReached) }
    }

    /// Sound an alert alongside the limit reached notification modal.
    @Published var limitReachedSound: Bool {
        didSet { defaults.set(limitReachedSound, forKey: Keys.limitReachedSound) }
    }

    /// Which sound a limit reached notification makes.
    @Published var limitReachedSoundName: String {
        didSet { defaults.set(limitReachedSoundName, forKey: Keys.limitReachedSoundName) }
    }

    /// The version whose changes have already been shown.
    ///
    /// Written when the What's New dialogue is dismissed rather than when it
    /// opens, so a crash in between cannot swallow the one launch it was going
    /// to appear on.
    @Published var lastSeenVersion: String? {
        didSet { defaults.set(lastSeenVersion, forKey: Keys.lastSeenVersion) }
    }

    @Published var launchAtLogin: Bool {
        didSet {
            guard launchAtLogin != Self.isRegisteredForLogin else { return }
            applyLaunchAtLogin()
        }
    }

    /// Set when the login-item request was refused, so the UI can say so rather
    /// than quietly flipping the switch back.
    @Published private(set) var launchAtLoginProblem: String?

    private let defaults: UserDefaults
    private enum Keys {
        /// The old off-list. Kept so a 1.9 install can invert it once.
        static let disconnected = "hiddenProviders"
        static let connected = "connectedProviders"
        static let seen = "seenProviders"
        static let disabledModels = "disabledModels"
        static let autoUpdateCheck = GeneratedSettings.Key.autoUpdateCheck
        static let mutedAlerts = "mutedAlertProviders"
        static let accountNicknames = GeneratedSettings.Key.accountNicknames
        static let hasLaunched = GeneratedSettings.Key.hasLaunchedBefore
        static let visibility = GeneratedSettings.Key.notchVisibility
        static let foldsForFullScreen = GeneratedSettings.Key.foldsForFullScreen
        static let notificationChannel = GeneratedSettings.Key.notificationChannel
        static let channelBannerSound = GeneratedSettings.Key.channelBannerSound
        static let edge = GeneratedSettings.Key.notchEdge
        // A new key, so there is nothing under the old app name to migrate.
        static let size = GeneratedSettings.Key.notchSize
        static let usesCustomSize = GeneratedSettings.Key.usesCustomNotchScale
        static let customSize = GeneratedSettings.Key.customNotchScale
        static let display = GeneratedSettings.Key.displayPreference
        static let resetTimeFormat = GeneratedSettings.Key.resetTimeFormat
        static let asksProviderOnLook = GeneratedSettings.Key.asksProviderOnLook
        static let convFinderCutPaste = GeneratedSettings.Key.convFinderCutPaste
        static let convWindowMaximizer = GeneratedSettings.Key.convWindowMaximizer
        static let convDockClickMinimize = GeneratedSettings.Key.convDockClickMinimize
        static let convFnCommand = GeneratedSettings.Key.convFnCommand
        static let convAutoQuit = GeneratedSettings.Key.convAutoQuit
        static let convAutoQuitApps = GeneratedSettings.Key.convAutoQuitApps
        static let convDiskImageInstaller = GeneratedSettings.Key.convDiskImageInstaller
        static let convDiskImageTrashDownload = GeneratedSettings.Key.convDiskImageTrashDownload
        static let finderPathField = GeneratedSettings.Key.finderPathField
        static let convDiskImageAuto = GeneratedSettings.Key.convDiskImageAuto
        static let convDiskImageAutoUpdate = GeneratedSettings.Key.convDiskImageAutoUpdate
        static let hubDiagnosticsPage = GeneratedSettings.Key.hubDiagnosticsPage
        static let claudeAccountScan = GeneratedSettings.Key.claudeAccountScan
        static let nearbyEnabled = GeneratedSettings.Key.nearbyEnabled
        static let agentRequests = GeneratedSettings.Key.agentRequests
        static let agentActions = GeneratedSettings.Key.agentActions
        static let agentCliPath = GeneratedSettings.Key.agentCliPath
        static let deviceLinkListen = GeneratedSettings.Key.deviceLinkListen
        static let membraneAutoEnroll = GeneratedSettings.Key.membraneAutoEnroll
        static let nearbyAlias = GeneratedSettings.Key.nearbyAlias
        static let nearbySaveFolder = GeneratedSettings.Key.nearbySaveFolder
        static let launcherEnabled = GeneratedSettings.Key.launcherEnabled
        static let launcherHotkey = GeneratedSettings.Key.launcherHotkey
        static let launcherConfig = GeneratedSettings.Key.launcherConfig
        static let launcherClipboard = GeneratedSettings.Key.launcherClipboard
        static let launcherCurrency = GeneratedSettings.Key.launcherCurrency
        static let launcherDictionary = GeneratedSettings.Key.launcherDictionary
        static let launcherShortcuts = GeneratedSettings.Key.launcherShortcuts
        static let windowManagementEnabled = GeneratedSettings.Key.windowManagementEnabled
        static let windowHotkeys = GeneratedSettings.Key.windowHotkeys
        static let settingsHotkey = GeneratedSettings.Key.settingsHotkey
        static let scope = GeneratedSettings.Key.notchScope
        static let accentColor = GeneratedSettings.Key.accentColor
        // A new key, so there is nothing under the old app name to migrate.
        static let weeklyRing = GeneratedSettings.Key.weeklyRing
        static let weeklyRingDashed = GeneratedSettings.Key.weeklyRingDashed
        static let showsNotchReadings = GeneratedSettings.Key.showsNotchReadings
        static let weeklyReading = GeneratedSettings.Key.weeklyReading
        static let claudeShowAllAccounts = "claudeShowAllAccounts"
        static let claudeDailyPaceRing = GeneratedSettings.Key.claudeDailyPaceRing
        static let weeklyHeadline = GeneratedSettings.Key.weeklyHeadline
        static let notchSurfaceStyle = GeneratedSettings.Key.notchSurfaceStyle
        static let watchLimit = GeneratedSettings.Key.watchLimit
        static let criticalLimit = GeneratedSettings.Key.criticalLimit
        static let colorTransitionStyle = GeneratedSettings.Key.colorTransitionStyle
        static let lastSeenVersion = GeneratedSettings.Key.lastSeenVersion
        static let order = GeneratedSettings.Key.providerOrder
        static let announceSessionEnd = GeneratedSettings.Key.announceSessionEnd
        static let sessionEndSound = GeneratedSettings.Key.sessionEndSound
        static let peekDuration = GeneratedSettings.Key.peekDuration
        static let sessionEndSoundName = GeneratedSettings.Key.sessionEndSoundName
        static let sessionBlockedSoundName = GeneratedSettings.Key.sessionBlockedSoundName
        static let announceUsageReset = GeneratedSettings.Key.announceUsageReset
        static let usageResetSound = GeneratedSettings.Key.usageResetSound
        static let usageResetSoundName = GeneratedSettings.Key.usageResetSoundName
        static let announceSessionLimitReached = GeneratedSettings.Key.announceSessionLimitReached
        static let announceWeeklyLimitReached = GeneratedSettings.Key.announceWeeklyLimitReached
        static let limitReachedSound = GeneratedSettings.Key.limitReachedSound
        static let limitReachedSoundName = GeneratedSettings.Key.limitReachedSoundName
        static let showCodexExtraLimits = GeneratedSettings.Key.showCodexExtraLimits
    }

    /// Whether extra Codex windows (Spark, code review) are shown, read off
    /// the main actor.
    ///
    /// The Codex provider is an actor and asks for this on every fetch, and
    /// `@Published` state is main-actor-isolated where `UserDefaults` is
    /// thread-safe — so the provider reads the store, not the object. Absent
    /// means on: a first launch should show them. `bool(forKey:)` cannot stand
    /// in for that default — it answers false for a key that was never written.
    nonisolated static func storedShowCodexExtraLimits(
        defaults: UserDefaults = .standard
    ) -> Bool {
        defaults.object(forKey: Keys.showCodexExtraLimits) as? Bool ?? GeneratedSettings.Default.showCodexExtraLimits
    }

    /// "Sample drive health", read from the store for the same reason as above: the
    /// `DriveHealth` actor asks on every nudge. Absent means on.
    nonisolated static func storedHubDriveHealth(defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: GeneratedSettings.Key.hubDriveHealth) as? Bool ?? GeneratedSettings.Default.hubDriveHealth
    }

    /// True the very first time this copy runs, and never again.
    ///
    /// Deliberately *not* inferred from "there are no readings yet" — that is
    /// also true of someone who switched every provider off, and re-introducing
    /// them to the app every launch would be worse than never introducing them
    /// at all.
    let isFirstLaunch: Bool

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.isFirstLaunch = !defaults.bool(forKey: Keys.hasLaunched)
        defaults.set(true, forKey: Keys.hasLaunched)
        let storedConnected = defaults.stringArray(forKey: Keys.connected)
        let storedSeen = Set(defaults.stringArray(forKey: Keys.seen) ?? [])
        let connected: Set<String>
        let seen: Set<String>
        let hidden: Set<String>?
        if let storedConnected {
            connected = Set(storedConnected)
            seen = storedSeen.isEmpty ? connected : storedSeen
            hidden = nil
        } else if defaults.object(forKey: Keys.disconnected) != nil {
            // An empty off-list is still a choice: everyone was on.
            hidden = Set(defaults.stringArray(forKey: Keys.disconnected) ?? [])
            connected = []
            seen = storedSeen
        } else if !self.isFirstLaunch {
            // Launched before this key existed, and never hid anyone.
            hidden = []
            connected = []
            seen = storedSeen
        } else {
            hidden = nil
            connected = []
            seen = storedSeen
        }
        self.connectedProviders = connected.filter { !Self.isModelCell($0) }
        self.seenProviders = seen.filter { !Self.isModelCell($0) }
        self.pendingHidden = hidden
        let models: Set<String>
        if let storedDisabled = defaults.stringArray(forKey: Keys.disabledModels) {
            models = Set(storedDisabled)
        } else {
            // Model cells that lived on the old off-list stay off. Read the
            // leftover even after `connectedProviders` exists: an earlier
            // invert left those ids in `hiddenProviders` and then ignored them.
            let leftover = hidden ?? Set(defaults.stringArray(forKey: Keys.disconnected) ?? [])
            models = leftover.filter(Self.isModelCell)
        }
        self.disabledModels = models
        defaults.set(Array(models), forKey: Keys.disabledModels)
        self.mutedAlertProviders = Set(defaults.stringArray(forKey: Keys.mutedAlerts) ?? [])
        self.accountNicknames = defaults.dictionary(forKey: Keys.accountNicknames) as? [String: String] ?? [:]
        // Absent means never chosen, which is the hover behaviour the app was
        // designed around — not hidden, which would make a fresh install look
        // like it failed to start.
        self.notchVisibility = defaults.string(forKey: Keys.visibility)
            // Pulse fork: the notch is always on screen by default.
            .flatMap(NotchVisibility.init(rawValue:)) ?? .alwaysShow
        // Absent means the fold that has shipped since full-screen detection
        // exists — the setting silences it, it does not introduce it.
        self.foldsForFullScreen = defaults.object(forKey: Keys.foldsForFullScreen) as? Bool ?? GeneratedSettings.Default.foldsForFullScreen
        // The notch, because that is what every earlier version did; a banner
        // is the choice of someone who found the notch too quiet.
        self.notificationChannel = defaults.string(forKey: Keys.notificationChannel)
            .flatMap(NotificationChannel.init(rawValue:)) ?? .notch
        self.channelBannerSound = defaults.object(forKey: Keys.channelBannerSound) as? Bool ?? GeneratedSettings.Default.channelBannerSound
        // The right edge is where the notch has always been, and it is the one
        // side of a Mac that no system chrome claims by default.
        self.notchEdge = defaults.string(forKey: Keys.edge)
            .flatMap(NotchEdge.init(rawValue:)) ?? .right
        // Medium is the design frame at 1:1. Only an install that never chose a size
        // gets it: a stored choice (Small included) is kept.
        self.notchSize = defaults.string(forKey: Keys.size)
            .flatMap(NotchSize.init(rawValue:)) ?? .medium
        // Absent means never chosen, and the presets are what every earlier
        // version had — so the slider is opt-in rather than the default.
        self.usesCustomNotchScale = defaults.bool(forKey: Keys.usesCustomSize)
        let stored = defaults.object(forKey: Keys.customSize) as? Double
        self.customNotchScale = stored.map {
            min(max($0, Self.customScaleRange.lowerBound), Self.customScaleRange.upperBound)
        } ?? GeneratedSettings.Default.customNotchScale
        self.displayPreference = defaults.string(forKey: Keys.display)
            .map(DisplayPreference.display) ?? .followActiveWindow
        self.resetTimeFormat = defaults.string(forKey: Keys.resetTimeFormat)
            .flatMap(ResetTimeFormat.init(rawValue:)) ?? .automatic
        self.showUsagePace = defaults.bool(forKey: Self.showUsagePaceKey)
        // Off by default: see the property. A request spent on every look is a
        // choice, and on a rate-limited provider it can cost freshness rather
        // than buy it.
        self.asksProviderOnLook = defaults.bool(forKey: Keys.asksProviderOnLook)
        // On unless the user has switched it off: absent means never chosen.
        self.autoUpdateCheck = defaults.object(forKey: Keys.autoUpdateCheck) as? Bool ?? GeneratedSettings.Default.autoUpdateCheck
        self.convFinderCutPaste = defaults.object(forKey: Keys.convFinderCutPaste) as? Bool ?? GeneratedSettings.Default.convFinderCutPaste
        self.convWindowMaximizer = defaults.bool(forKey: Keys.convWindowMaximizer)
        self.convDockClickMinimize = defaults.bool(forKey: Keys.convDockClickMinimize)
        self.convFnCommand = defaults.bool(forKey: Keys.convFnCommand)
        self.convAutoQuit = defaults.bool(forKey: Keys.convAutoQuit)
        self.convDiskImageInstaller = defaults.object(forKey: Keys.convDiskImageInstaller) as? Bool ?? GeneratedSettings.Default.convDiskImageInstaller
        self.convDiskImageTrashDownload = defaults.bool(forKey: Keys.convDiskImageTrashDownload)
        self.finderPathField = defaults.bool(forKey: Keys.finderPathField)
        self.convDiskImageAuto = defaults.object(forKey: Keys.convDiskImageAuto) as? Bool ?? GeneratedSettings.Default.convDiskImageAuto
        self.convDiskImageAutoUpdate = defaults.bool(forKey: Keys.convDiskImageAutoUpdate)
        self.hubDiagnosticsPage = defaults.object(forKey: Keys.hubDiagnosticsPage) as? Bool ?? GeneratedSettings.Default.hubDiagnosticsPage
        self.claudeAccountScan = defaults.object(forKey: Keys.claudeAccountScan) as? Bool ?? GeneratedSettings.Default.claudeAccountScan
        self.convAutoQuitApps = defaults.stringArray(forKey: Keys.convAutoQuitApps) ?? GeneratedSettings.Default.convAutoQuitApps
        self.nearbyEnabled = defaults.object(forKey: Keys.nearbyEnabled) as? Bool ?? GeneratedSettings.Default.nearbyEnabled
        self.agentRequests = defaults.object(forKey: Keys.agentRequests) as? Bool ?? GeneratedSettings.Default.agentRequests
        self.agentActions = defaults.object(forKey: Keys.agentActions) as? Bool ?? GeneratedSettings.Default.agentActions
        self.agentCliPath = defaults.string(forKey: Keys.agentCliPath) ?? GeneratedSettings.Default.agentCliPath
        self.deviceLinkListen = defaults.object(forKey: Keys.deviceLinkListen) as? Bool ?? GeneratedSettings.Default.deviceLinkListen
        self.deviceLinkRelayUrl = defaults.string(forKey: GeneratedSettings.Key.deviceLinkRelayUrl) ?? GeneratedSettings.Default.deviceLinkRelayUrl
        self.deviceLinkRelayAdmission = defaults.string(forKey: GeneratedSettings.Key.deviceLinkRelayAdmission) ?? GeneratedSettings.Default.deviceLinkRelayAdmission
        self.deviceMeshLocal = defaults.object(forKey: GeneratedSettings.Key.deviceMeshLocal) as? Bool ?? GeneratedSettings.Default.deviceMeshLocal
        self.sendOverMesh = defaults.object(forKey: GeneratedSettings.Key.sendOverMesh) as? Bool ?? GeneratedSettings.Default.sendOverMesh
        self.agentFileAutoAccept = defaults.object(forKey: GeneratedSettings.Key.agentFileAutoAccept) as? Bool ?? GeneratedSettings.Default.agentFileAutoAccept
        self.chatOverDeviceLink = defaults.object(forKey: GeneratedSettings.Key.chatOverDeviceLink) as? Bool ?? GeneratedSettings.Default.chatOverDeviceLink
        self.devicePhoneChats = defaults.object(forKey: GeneratedSettings.Key.devicePhoneChats) as? Bool ?? GeneratedSettings.Default.devicePhoneChats
        self.devicePhoneRead = defaults.string(forKey: GeneratedSettings.Key.devicePhoneRead) ?? GeneratedSettings.Default.devicePhoneRead
        self.devicePhoneSend = defaults.string(forKey: GeneratedSettings.Key.devicePhoneSend) ?? GeneratedSettings.Default.devicePhoneSend
        self.devicePhoneTypeIntoChat = defaults.object(forKey: GeneratedSettings.Key.devicePhoneTypeIntoChat) as? Bool ?? GeneratedSettings.Default.devicePhoneTypeIntoChat
        self.devicePhoneReceive = defaults.object(forKey: GeneratedSettings.Key.devicePhoneReceive) as? Bool ?? GeneratedSettings.Default.devicePhoneReceive
        self.devicePhoneReceiveTtlMinutes = min(1_440, max(1, defaults.object(forKey: GeneratedSettings.Key.devicePhoneReceiveTtlMinutes) as? Int ?? GeneratedSettings.Default.devicePhoneReceiveTtlMinutes))
        self.claudeAccountSwitcher = defaults.object(forKey: GeneratedSettings.Key.claudeAccountSwitcher) as? Bool ?? GeneratedSettings.Default.claudeAccountSwitcher
        self.cleanupDeveloperItems = defaults.object(forKey: GeneratedSettings.Key.cleanupDeveloperItems) as? Bool ?? GeneratedSettings.Default.cleanupDeveloperItems
        self.engineMemoryGuard = defaults.object(forKey: GeneratedSettings.Key.engineMemoryGuard) as? Bool ?? GeneratedSettings.Default.engineMemoryGuard
        self.engineMemoryCapMb = min(65_536, max(512, defaults.object(forKey: GeneratedSettings.Key.engineMemoryCapMb) as? Int ?? GeneratedSettings.Default.engineMemoryCapMb))
        self.membraneAutoEnroll = defaults.object(forKey: Keys.membraneAutoEnroll) as? Bool ?? GeneratedSettings.Default.membraneAutoEnroll
        self.sendDefaultDevice = defaults.string(forKey: GeneratedSettings.Key.sendDefaultDevice) ?? GeneratedSettings.Default.sendDefaultDevice
        self.agentRoute = defaults.string(forKey: GeneratedSettings.Key.agentRoute) ?? GeneratedSettings.Default.agentRoute
        self.nearbyAlias = defaults.string(forKey: Keys.nearbyAlias) ?? GeneratedSettings.Default.nearbyAlias
        self.nearbySaveFolder = defaults.string(forKey: Keys.nearbySaveFolder) ?? GeneratedSettings.Default.nearbySaveFolder
        self.launcherEnabled = defaults.bool(forKey: Keys.launcherEnabled)
        self.launcherHotkey = defaults.string(forKey: Keys.launcherHotkey)
            .flatMap(LauncherHotkeyChoice.init(rawValue:)) ?? .optionSpace
        self.launcherConfigJSON = defaults.string(forKey: Keys.launcherConfig) ?? GeneratedSettings.Default.launcherConfig
        self.launcherClipboard = defaults.bool(forKey: Keys.launcherClipboard)
        self.clipboardStripEnabled = defaults.object(forKey: GeneratedSettings.Key.clipboardStripEnabled) as? Bool ?? GeneratedSettings.Default.clipboardStripEnabled
        self.clipboardHotkey = defaults.string(forKey: GeneratedSettings.Key.clipboardHotkey) ?? GeneratedSettings.Default.clipboardHotkey
        self.clipboardCardSize = defaults.string(forKey: GeneratedSettings.Key.clipboardCardSize) ?? GeneratedSettings.Default.clipboardCardSize
        self.clipboardStripPlacement = defaults.string(forKey: GeneratedSettings.Key.clipboardStripPlacement) ?? GeneratedSettings.Default.clipboardStripPlacement
        self.clipboardShowNumbers = defaults.object(forKey: GeneratedSettings.Key.clipboardShowNumbers) as? Bool ?? GeneratedSettings.Default.clipboardShowNumbers
        self.clipboardPasteMode = defaults.string(forKey: GeneratedSettings.Key.clipboardPasteMode) ?? GeneratedSettings.Default.clipboardPasteMode
        self.clipboardPlainDefault = defaults.bool(forKey: GeneratedSettings.Key.clipboardPlainDefault)
        self.clipboardCaptureImages = defaults.object(forKey: GeneratedSettings.Key.clipboardCaptureImages) as? Bool ?? GeneratedSettings.Default.clipboardCaptureImages
        self.clipboardCaptureFiles = defaults.object(forKey: GeneratedSettings.Key.clipboardCaptureFiles) as? Bool ?? GeneratedSettings.Default.clipboardCaptureFiles
        self.clipboardCaptureRich = defaults.object(forKey: GeneratedSettings.Key.clipboardCaptureRich) as? Bool ?? GeneratedSettings.Default.clipboardCaptureRich
        self.clipboardMaxItems = min(5000, max(50, defaults.object(forKey: GeneratedSettings.Key.clipboardMaxItems) as? Int ?? GeneratedSettings.Default.clipboardMaxItems))
        self.clipboardMaxMB = min(4096, max(20, defaults.object(forKey: GeneratedSettings.Key.clipboardMaxMB) as? Int ?? GeneratedSettings.Default.clipboardMaxMB))
        self.clipboardMaxAgeDays = min(365, max(0, defaults.object(forKey: GeneratedSettings.Key.clipboardMaxAgeDays) as? Int ?? GeneratedSettings.Default.clipboardMaxAgeDays))
        self.clipboardScreenshotHours = min(720, max(0, defaults.object(forKey: GeneratedSettings.Key.clipboardScreenshotHours) as? Int ?? GeneratedSettings.Default.clipboardScreenshotHours))
        self.clipboardSkipAutoGenerated = defaults.object(forKey: GeneratedSettings.Key.clipboardSkipAutoGenerated) as? Bool ?? GeneratedSettings.Default.clipboardSkipAutoGenerated
        self.clipboardSkipRemote = defaults.object(forKey: GeneratedSettings.Key.clipboardSkipRemote) as? Bool ?? GeneratedSettings.Default.clipboardSkipRemote
        self.clipboardSecretFilter = defaults.object(forKey: GeneratedSettings.Key.clipboardSecretFilter) as? Bool ?? GeneratedSettings.Default.clipboardSecretFilter
        self.clipboardEncryption = defaults.object(forKey: GeneratedSettings.Key.clipboardEncryption) as? Bool ?? GeneratedSettings.Default.clipboardEncryption
        self.clipboardExcludedApps = defaults.stringArray(forKey: "clipboardExcludedApps") ?? ClipSettings.defaultExcludedApps
        self.clipboardIgnoredTypes = defaults.stringArray(forKey: GeneratedSettings.Key.clipboardIgnoredTypes) ?? GeneratedSettings.Default.clipboardIgnoredTypes
        self.clipboardRememberBoard = defaults.bool(forKey: GeneratedSettings.Key.clipboardRememberBoard)
        self.clipboardHideFromCapture = defaults.object(forKey: GeneratedSettings.Key.clipboardHideFromCapture) as? Bool ?? GeneratedSettings.Default.clipboardHideFromCapture
        self.clipboardCopyBurst = defaults.object(forKey: GeneratedSettings.Key.clipboardCopyBurst) as? Bool ?? GeneratedSettings.Default.clipboardCopyBurst
        self.clipboardWheelRing = defaults.object(forKey: GeneratedSettings.Key.clipboardWheelRing) as? Bool ?? GeneratedSettings.Default.clipboardWheelRing
        self.clipboardRestoreAfterPaste = defaults.object(forKey: GeneratedSettings.Key.clipboardRestoreAfterPaste) as? Bool ?? GeneratedSettings.Default.clipboardRestoreAfterPaste
        self.launcherCurrency = defaults.bool(forKey: Keys.launcherCurrency)
        self.launcherDictionary = defaults.object(forKey: Keys.launcherDictionary) as? Bool ?? GeneratedSettings.Default.launcherDictionary
        self.launcherShortcuts = defaults.object(forKey: Keys.launcherShortcuts) as? Bool ?? GeneratedSettings.Default.launcherShortcuts
        // Off by default, like every convenience.
        self.windowManagementEnabled = defaults.bool(forKey: Keys.windowManagementEnabled)
        self.windowHotkeys = defaults.dictionary(forKey: Keys.windowHotkeys) as? [String: String] ?? [:]
        self.settingsHotkey = defaults.string(forKey: Keys.settingsHotkey) ?? GeneratedSettings.Default.settingsHotkey
        // Pulse fork: a switch for every feature. Absent means the default,
        // which is on for each of these (docs/settings.md).
        self.toolWheelEnabled = defaults.object(forKey: GeneratedSettings.Key.toolWheelEnabled) as? Bool ?? GeneratedSettings.Default.toolWheelEnabled
        self.notifications = defaults.object(forKey: GeneratedSettings.Key.notifications) as? Bool ?? GeneratedSettings.Default.notifications
        self.showsSettingsHandle = defaults.object(forKey: GeneratedSettings.Key.showsSettingsHandle) as? Bool ?? GeneratedSettings.Default.showsSettingsHandle
        self.notchOptionDrag = defaults.object(forKey: GeneratedSettings.Key.notchOptionDrag) as? Bool ?? GeneratedSettings.Default.notchOptionDrag
        self.notchMinimizeButton = defaults.object(forKey: GeneratedSettings.Key.notchMinimizeButton) as? Bool ?? GeneratedSettings.Default.notchMinimizeButton
        self.notchMinimized = defaults.object(forKey: GeneratedSettings.Key.notchMinimized) as? Bool ?? GeneratedSettings.Default.notchMinimized
        self.showsHoverCards = defaults.object(forKey: GeneratedSettings.Key.showsHoverCards) as? Bool ?? GeneratedSettings.Default.showsHoverCards
        self.disabledTools = defaults.stringArray(forKey: GeneratedSettings.Key.toolsOff) ?? GeneratedSettings.Default.toolsOff
        self.wheelTools = defaults.stringArray(forKey: GeneratedSettings.Key.wheelTools) ?? GeneratedSettings.Default.wheelTools
        self.announceDriveAlerts = defaults.object(forKey: GeneratedSettings.Key.announceDriveAlerts) as? Bool ?? GeneratedSettings.Default.announceDriveAlerts
        self.remoteApprovalAlerts = defaults.object(forKey: GeneratedSettings.Key.remoteApprovalAlerts) as? Bool ?? GeneratedSettings.Default.remoteApprovalAlerts
        self.driveAlertSound = defaults.object(forKey: GeneratedSettings.Key.driveAlertSound) as? Bool ?? GeneratedSettings.Default.driveAlertSound
        self.watchesAgentSessions = defaults.object(forKey: GeneratedSettings.Key.watchesAgentSessions) as? Bool ?? GeneratedSettings.Default.watchesAgentSessions
        self.launcherCalculator = defaults.object(forKey: GeneratedSettings.Key.launcherCalculator) as? Bool ?? GeneratedSettings.Default.launcherCalculator
        self.launcherPulseCommands = defaults.object(forKey: GeneratedSettings.Key.launcherPulseCommands) as? Bool ?? GeneratedSettings.Default.launcherPulseCommands
        self.launcherSnippetExpansion = defaults.object(forKey: GeneratedSettings.Key.launcherSnippetExpansion) as? Bool ?? GeneratedSettings.Default.launcherSnippetExpansion
        self.launcherHotkeyConflict = ["pulse", "other"].contains(defaults.string(forKey: GeneratedSettings.Key.launcherHotkeyConflict) ?? "")
            ? (defaults.string(forKey: GeneratedSettings.Key.launcherHotkeyConflict) ?? "") : ""
        let storedWheelConflict = defaults.string(forKey: GeneratedSettings.Key.wheelMiddleClickConflict) ?? ""
        self.wheelMiddleClickConflict = ["override", "standDown"].contains(storedWheelConflict)
            ? storedWheelConflict : GeneratedSettings.Default.wheelMiddleClickConflict
        self.sendReceivedShelf = defaults.object(forKey: GeneratedSettings.Key.sendReceivedShelf) as? Bool ?? GeneratedSettings.Default.sendReceivedShelf
        let storedAskTarget = defaults.string(forKey: GeneratedSettings.Key.launcherAskTarget) ?? ""
        // A stored "claude" (the removed Ask Claude target, Adrian 2026-10-29) reads as the default.
        self.launcherAskTarget = ["inLauncher", "chatgpt", "off"].contains(storedAskTarget)
            ? storedAskTarget
            : GeneratedSettings.Default.launcherAskTarget
        self.launcherWebSearch = defaults.object(forKey: GeneratedSettings.Key.launcherWebSearch) as? Bool ?? GeneratedSettings.Default.launcherWebSearch
        self.launcherWebSearchEngine = ["google", "bing", "duckduckgo"].contains(defaults.string(forKey: GeneratedSettings.Key.launcherWebSearchEngine) ?? "")
            ? (defaults.string(forKey: GeneratedSettings.Key.launcherWebSearchEngine) ?? GeneratedSettings.Default.launcherWebSearchEngine) : GeneratedSettings.Default.launcherWebSearchEngine
        self.chatRingLight = defaults.object(forKey: GeneratedSettings.Key.chatRingLight) as? Bool ?? GeneratedSettings.Default.chatRingLight
        self.chatPluginAutoInstall = defaults.object(forKey: GeneratedSettings.Key.chatPluginAutoInstall) as? Bool ?? GeneratedSettings.Default.chatPluginAutoInstall
        self.claudeChatMirror = defaults.object(forKey: GeneratedSettings.Key.claudeChatMirror) as? Bool ?? GeneratedSettings.Default.claudeChatMirror
        self.claudeChatRemember = defaults.object(forKey: GeneratedSettings.Key.claudeChatRemember) as? Bool ?? GeneratedSettings.Default.claudeChatRemember
        self.hubDriveHealth = defaults.object(forKey: GeneratedSettings.Key.hubDriveHealth) as? Bool ?? GeneratedSettings.Default.hubDriveHealth
        self.hubMetricsHistory = defaults.object(forKey: GeneratedSettings.Key.hubMetricsHistory) as? Bool ?? GeneratedSettings.Default.hubMetricsHistory
        self.hubFileIndex = defaults.object(forKey: GeneratedSettings.Key.hubFileIndex) as? Bool ?? GeneratedSettings.Default.hubFileIndex
        self.hubAutoScan = defaults.object(forKey: GeneratedSettings.Key.hubAutoScan) as? Bool ?? GeneratedSettings.Default.hubAutoScan
        self.mediaCompression = defaults.object(forKey: GeneratedSettings.Key.mediaCompression) as? Bool ?? GeneratedSettings.Default.mediaCompression
        self.onboardingDone = defaults.bool(forKey: GeneratedSettings.Key.onboardingDone)
        self.chromeSnapshotCount = defaults.object(forKey: GeneratedSettings.Key.chromeSnapshotCount) as? Bool ?? GeneratedSettings.Default.chromeSnapshotCount
        self.finderMenuCut = defaults.object(forKey: GeneratedSettings.Key.finderMenuCut) as? Bool ?? GeneratedSettings.Default.finderMenuCut
        self.finderMenuCopyPath = defaults.object(forKey: GeneratedSettings.Key.finderMenuCopyPath) as? Bool ?? GeneratedSettings.Default.finderMenuCopyPath
        self.finderMenuTerminal = defaults.object(forKey: GeneratedSettings.Key.finderMenuTerminal) as? Bool ?? GeneratedSettings.Default.finderMenuTerminal
        self.remoteTools = defaults.object(forKey: GeneratedSettings.Key.remoteTools) as? Bool ?? GeneratedSettings.Default.remoteTools
        self.remoteRoots = defaults.stringArray(forKey: GeneratedSettings.Key.remoteRoots) ?? GeneratedSettings.Default.remoteRoots
        self.remoteChatRead = defaults.stringArray(forKey: GeneratedSettings.Key.remoteChatRead) ?? GeneratedSettings.Default.remoteChatRead
        self.remoteReadMaxBytes = min(2_097_152, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteReadMaxBytes) as? Int ?? GeneratedSettings.Default.remoteReadMaxBytes))
        self.remoteFindMaxResults = min(1000, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteFindMaxResults) as? Int ?? GeneratedSettings.Default.remoteFindMaxResults))
        self.remoteChatReadMaxTurns = min(500, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteChatReadMaxTurns) as? Int ?? GeneratedSettings.Default.remoteChatReadMaxTurns))
        self.remoteChatReadMaxBytes = min(2_097_152, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteChatReadMaxBytes) as? Int ?? GeneratedSettings.Default.remoteChatReadMaxBytes))
        self.remoteImageMaxBytes = min(26_214_400, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteImageMaxBytes) as? Int ?? GeneratedSettings.Default.remoteImageMaxBytes))
        self.remoteGateway = defaults.object(forKey: GeneratedSettings.Key.remoteGateway) as? Bool ?? GeneratedSettings.Default.remoteGateway
        self.remoteDocs = defaults.object(forKey: GeneratedSettings.Key.remoteDocs) as? Bool ?? GeneratedSettings.Default.remoteDocs
        self.remoteDocsMaxBytes = min(67_108_864, max(1, defaults.object(forKey: GeneratedSettings.Key.remoteDocsMaxBytes) as? Int ?? GeneratedSettings.Default.remoteDocsMaxBytes))
        self.remoteFilesWrite = defaults.object(forKey: GeneratedSettings.Key.remoteFilesWrite) as? Bool ?? GeneratedSettings.Default.remoteFilesWrite
        self.remoteTerminals = defaults.object(forKey: GeneratedSettings.Key.remoteTerminals) as? Bool ?? GeneratedSettings.Default.remoteTerminals
        self.remoteTermStanding = defaults.object(forKey: GeneratedSettings.Key.remoteTermStanding) as? Bool ?? GeneratedSettings.Default.remoteTermStanding
        self.remoteTermAllow = defaults.stringArray(forKey: GeneratedSettings.Key.remoteTermAllow) ?? GeneratedSettings.Default.remoteTermAllow
        self.remoteTermDeny = defaults.stringArray(forKey: GeneratedSettings.Key.remoteTermDeny) ?? GeneratedSettings.Default.remoteTermDeny
        self.remoteClipboard = defaults.object(forKey: GeneratedSettings.Key.remoteClipboard) as? Bool ?? GeneratedSettings.Default.remoteClipboard
        self.remoteReadOnly = defaults.object(forKey: GeneratedSettings.Key.remoteReadOnly) as? Bool ?? GeneratedSettings.Default.remoteReadOnly
        self.ledgerTranscriptSearch = defaults.object(forKey: GeneratedSettings.Key.ledgerTranscriptSearch) as? Bool ?? GeneratedSettings.Default.ledgerTranscriptSearch
        self.ledgerTranscriptShare = defaults.object(forKey: GeneratedSettings.Key.ledgerTranscriptShare) as? Bool ?? GeneratedSettings.Default.ledgerTranscriptShare
        self.remoteScreenObserve = defaults.object(forKey: GeneratedSettings.Key.remoteScreenObserve) as? Bool ?? GeneratedSettings.Default.remoteScreenObserve
        self.remoteScreenAct = defaults.object(forKey: GeneratedSettings.Key.remoteScreenAct) as? Bool ?? GeneratedSettings.Default.remoteScreenAct
        self.remoteBlockedApps = defaults.stringArray(forKey: GeneratedSettings.Key.remoteBlockedApps) ?? GeneratedSettings.Default.remoteBlockedApps
        self.remoteScreenDisclosure = defaults.object(forKey: GeneratedSettings.Key.remoteScreenDisclosure) as? Bool ?? GeneratedSettings.Default.remoteScreenDisclosure
        self.remoteLaunch = defaults.object(forKey: GeneratedSettings.Key.remoteLaunch) as? Bool ?? GeneratedSettings.Default.remoteLaunch
        self.remoteMedia = defaults.object(forKey: GeneratedSettings.Key.remoteMedia) as? Bool ?? GeneratedSettings.Default.remoteMedia
        self.phoneChatReadAll = defaults.object(forKey: GeneratedSettings.Key.phoneChatReadAll) as? Bool ?? GeneratedSettings.Default.phoneChatReadAll
        self.chatUnlock = defaults.object(forKey: GeneratedSettings.Key.chatUnlock) as? Bool ?? GeneratedSettings.Default.chatUnlock
        self.chatUnlockMinutes = min(10080, max(0, defaults.object(forKey: GeneratedSettings.Key.chatUnlockMinutes) as? Int ?? GeneratedSettings.Default.chatUnlockMinutes))
        self.remoteFormFill = defaults.object(forKey: GeneratedSettings.Key.remoteFormFill) as? Bool ?? GeneratedSettings.Default.remoteFormFill
        self.remoteFormFillStanding = defaults.stringArray(forKey: GeneratedSettings.Key.remoteFormFillStanding) ?? GeneratedSettings.Default.remoteFormFillStanding
        self.remoteFormFillProfile = defaults.stringArray(forKey: GeneratedSettings.Key.remoteFormFillProfile) ?? GeneratedSettings.Default.remoteFormFillProfile
        self.remoteFormFillLogins = defaults.object(forKey: GeneratedSettings.Key.remoteFormFillLogins) as? Bool ?? GeneratedSettings.Default.remoteFormFillLogins
        self.remoteFormFillPayments = defaults.object(forKey: GeneratedSettings.Key.remoteFormFillPayments) as? Bool ?? GeneratedSettings.Default.remoteFormFillPayments
        self.remoteFormFillLoginManagers = defaults.stringArray(forKey: GeneratedSettings.Key.remoteFormFillLoginManagers) ?? GeneratedSettings.Default.remoteFormFillLoginManagers
        self.remoteFormFillPaymentsAutoCapCents = min(10_000_000, max(0, defaults.object(forKey: GeneratedSettings.Key.remoteFormFillPaymentsAutoCapCents) as? Int ?? GeneratedSettings.Default.remoteFormFillPaymentsAutoCapCents))
        self.remoteFormFillPaymentsMonthCapCents = min(10_000_000, max(0, defaults.object(forKey: GeneratedSettings.Key.remoteFormFillPaymentsMonthCapCents) as? Int ?? GeneratedSettings.Default.remoteFormFillPaymentsMonthCapCents))
        self.remoteScreenSpotTakeover = defaults.object(forKey: GeneratedSettings.Key.remoteScreenSpotTakeover) as? Bool ?? GeneratedSettings.Default.remoteScreenSpotTakeover
        self.hubWatchdog = defaults.object(forKey: GeneratedSettings.Key.hubWatchdog) as? Bool ?? GeneratedSettings.Default.hubWatchdog
        self.hubLog = defaults.object(forKey: GeneratedSettings.Key.hubLog) as? Bool ?? GeneratedSettings.Default.hubLog
        self.hubDiagReports = defaults.object(forKey: GeneratedSettings.Key.hubDiagReports) as? Bool ?? GeneratedSettings.Default.hubDiagReports
        self.hubWatchdogCpuPercent = min(1_000, max(1, defaults.object(forKey: GeneratedSettings.Key.hubWatchdogCpuPercent) as? Int ?? GeneratedSettings.Default.hubWatchdogCpuPercent))
        self.hubWatchdogMinutes = min(60, max(1, defaults.object(forKey: GeneratedSettings.Key.hubWatchdogMinutes) as? Int ?? GeneratedSettings.Default.hubWatchdogMinutes))
        self.hubWatchdogFootprintMb = min(65_536, max(64, defaults.object(forKey: GeneratedSettings.Key.hubWatchdogFootprintMb) as? Int ?? GeneratedSettings.Default.hubWatchdogFootprintMb))
        self.hubWatchdogThreads = min(10_000, max(10, defaults.object(forKey: GeneratedSettings.Key.hubWatchdogThreads) as? Int ?? GeneratedSettings.Default.hubWatchdogThreads))
        self.hubWatchdogWriteMbPerHour = min(100_000, max(1, defaults.object(forKey: GeneratedSettings.Key.hubWatchdogWriteMbPerHour) as? Int ?? GeneratedSettings.Default.hubWatchdogWriteMbPerHour))
        self.hubFileIndexAutoPause = defaults.object(forKey: GeneratedSettings.Key.hubFileIndexAutoPause) as? Bool ?? GeneratedSettings.Default.hubFileIndexAutoPause
        self.hubFileIndexMaxTrips = min(20, max(1, defaults.object(forKey: GeneratedSettings.Key.hubFileIndexMaxTrips) as? Int ?? GeneratedSettings.Default.hubFileIndexMaxTrips))
        self.hubFileIndexPauseMinutes = min(1_440, max(1, defaults.object(forKey: GeneratedSettings.Key.hubFileIndexPauseMinutes) as? Int ?? GeneratedSettings.Default.hubFileIndexPauseMinutes))
        self.hubFileIndexMaxCpuPercent = min(1_000, max(1, defaults.object(forKey: GeneratedSettings.Key.hubFileIndexMaxCpuPercent) as? Int ?? GeneratedSettings.Default.hubFileIndexMaxCpuPercent))
        self.hubFileIndexMaxFootprintMb = min(65_536, max(64, defaults.object(forKey: GeneratedSettings.Key.hubFileIndexMaxFootprintMb) as? Int ?? GeneratedSettings.Default.hubFileIndexMaxFootprintMb))
        self.hubFileIndexMaxWriteMbPerHour = min(100_000, max(1, defaults.object(forKey: GeneratedSettings.Key.hubFileIndexMaxWriteMbPerHour) as? Int ?? GeneratedSettings.Default.hubFileIndexMaxWriteMbPerHour))
        self.hubFileIndexWholeDisk = defaults.object(forKey: GeneratedSettings.Key.hubFileIndexWholeDisk) as? Bool ?? GeneratedSettings.Default.hubFileIndexWholeDisk
        self.hubFileIndexVolumes = defaults.stringArray(forKey: GeneratedSettings.Key.hubFileIndexVolumes) ?? GeneratedSettings.Default.hubFileIndexVolumes
        self.hubFileIndexExcludes = defaults.stringArray(forKey: GeneratedSettings.Key.hubFileIndexExcludes) ?? GeneratedSettings.Default.hubFileIndexExcludes
        // Off by default: it swaps what Claude's ring means, and that is a
        // choice for whoever budgets their week that way.
        self.claudeDailyPaceRing = defaults.bool(forKey: Keys.claudeDailyPaceRing)
        self.claudeShowAllAccounts = defaults.object(forKey: Keys.claudeShowAllAccounts) as? Bool ?? true
        // Off by default for the same reason: it changes what every ring means.
        // Pulse fork: the weekly limit leads (main, outer ring); the
        // five-hour session is the thin inner ring.
        self.weeklyHeadline = defaults.object(forKey: Keys.weeklyHeadline) as? Bool ?? GeneratedSettings.Default.weeklyHeadline
        self.showCodexExtraLimits = Self.storedShowCodexExtraLimits(defaults: defaults)
        // Absent means never chosen. Main display only, because that is what a
        // single-panel setup always did — all-displays on a fresh install
        // would put notches where none were expected.
        self.notchScope = defaults.string(forKey: Keys.scope)
            .flatMap(NotchScreenScope.init(rawValue:)) ?? .mainDisplay
        // Follow the Mac unless the user explicitly chooses a Codenotch colour.
        // Off by default: an extra arc in a 44pt circle is a change to how
        // every reading looks, and nobody asked for it on their behalf.
        self.weeklyRingDashed = defaults.object(forKey: Keys.weeklyRingDashed) as? Bool ?? GeneratedSettings.Default.weeklyRingDashed
        // Pulse fork: rings only by default; numbers are in the hover card.
        self.showsNotchReadings = defaults.object(forKey: Keys.showsNotchReadings) as? Bool ?? GeneratedSettings.Default.showsNotchReadings
        self.weeklyReading = defaults.object(forKey: Keys.weeklyReading) as? Bool ?? GeneratedSettings.Default.weeklyReading

        self.weeklyRing = defaults.string(forKey: Keys.weeklyRing)
            // Pulse fork: one cell per reading pair — the main ring outside,
            // the second reading as a thinner ring inside it.
            .flatMap(WeeklyRing.init(rawValue:)) ?? .inside
        // On unless turned off: it is how the notch is carried to another edge,
        // and a control that is missing by default is one nobody finds.
        self.accentColor = defaults.string(forKey: Keys.accentColor)
            .flatMap(AccentColorChoice.init(rawValue:)) ?? .green
        self.notchSurfaceStyle = defaults.string(forKey: Keys.notchSurfaceStyle)
            .flatMap(NotchSurfaceStyle.init(rawValue:))
            ?? NotchSurfaceStyle(rawValue: GeneratedSettings.Default.notchSurfaceStyle) ?? .solid
        let storedWatchLimit = defaults.object(forKey: Keys.watchLimit) as? Double ?? GeneratedSettings.Default.watchLimit
        let storedCriticalLimit = defaults.object(forKey: Keys.criticalLimit) as? Double ?? GeneratedSettings.Default.criticalLimit
        // `didSet` does the clamping, and it does not run for these assignments,
        // so a stored pair that crossed over is repaired here instead.
        let critical = min(max(storedCriticalLimit, 0.02), 1.0)
        self.criticalLimit = critical
        self.watchLimit = min(max(storedWatchLimit, 0.01), critical - 0.01)
        self.colorTransitionStyle = defaults.string(forKey: Keys.colorTransitionStyle)
            .flatMap(ColorTransitionStyle.init(rawValue:)) ?? .hardStep
        // Absent means never chosen, which is follow-the-Mac.
        self.language = defaults.string(forKey: L10n.languageDefaultsKey)
            .flatMap(AppLanguage.init(rawValue:)) ?? .system
        // Absent means nothing has been shown yet, which is true of a fresh
        // install — so the current release reads as new to it.
        self.lastSeenVersion = defaults.string(forKey: Keys.lastSeenVersion)
        // Absent means never chosen, so the rings keep the order the app ships
        // with until someone drags one.
        self.providerOrder = defaults.stringArray(forKey: Keys.order) ?? GeneratedSettings.Default.providerOrder
        // Both default to on, so `bool(forKey:)` — which answers false for a
        // key that was never written — cannot stand in for the default.
        self.announceSessionEnd = defaults.object(forKey: Keys.announceSessionEnd) as? Bool ?? GeneratedSettings.Default.announceSessionEnd
        self.sessionEndSound = defaults.object(forKey: Keys.sessionEndSound) as? Bool ?? GeneratedSettings.Default.sessionEndSound
        self.peekDuration = defaults.string(forKey: Keys.peekDuration)
            .flatMap(PeekDuration.init(rawValue:)) ?? .standard
        self.sessionEndSoundName = defaults.string(forKey: Keys.sessionEndSoundName)
            ?? SessionChime.defaultFinished
        self.sessionBlockedSoundName = defaults.string(forKey: Keys.sessionBlockedSoundName)
            ?? SessionChime.defaultBlocked
        self.announceUsageReset = defaults.object(forKey: Keys.announceUsageReset) as? Bool ?? GeneratedSettings.Default.announceUsageReset
        self.usageResetSound = defaults.object(forKey: Keys.usageResetSound) as? Bool ?? GeneratedSettings.Default.usageResetSound
        self.usageResetSoundName = defaults.string(forKey: Keys.usageResetSoundName)
            ?? SessionChime.defaultFinished
        self.announceSessionLimitReached = defaults.object(forKey: Keys.announceSessionLimitReached) as? Bool ?? GeneratedSettings.Default.announceSessionLimitReached
        self.announceWeeklyLimitReached = defaults.object(forKey: Keys.announceWeeklyLimitReached) as? Bool ?? GeneratedSettings.Default.announceWeeklyLimitReached
        self.limitReachedSound = defaults.object(forKey: Keys.limitReachedSound) as? Bool ?? GeneratedSettings.Default.limitReachedSound
        self.limitReachedSoundName = defaults.string(forKey: Keys.limitReachedSoundName)
            ?? SessionChime.defaultBlocked
        // Read from the system rather than from our own store: the user can turn
        // this off in System Settings, and a remembered `true` would then be a lie.
        self.launchAtLogin = Self.isRegisteredForLogin
    }

    // MARK: Account names

    func nickname(for providerID: String) -> String? {
        accountNicknames[providerID]
    }

    // MARK: Threshold alerts

    func isMutedAlerts(for providerID: String) -> Bool {
        if mutedAlertProviders.contains(providerID) { return true }
        // The switch mutes a provider's every profile, the extra ones included.
        if ClaudeProfile.isClaude(providerID: providerID) { return mutedAlertProviders.contains(ClaudeProfile.defaultID) }
        if CodexProfile.isCodex(providerID: providerID) { return mutedAlertProviders.contains(CodexProfile.defaultID) }
        return false
    }

    func setAlertsMuted(_ muted: Bool, for providerID: String) {
        if muted {
            mutedAlertProviders.insert(providerID)
        } else {
            mutedAlertProviders.remove(providerID)
        }
    }

    /// Claude and Codex stay on for a first install and for a newly discovered
    /// profile. Everyone else starts off.
    nonisolated static func isDefaultOnFamily(_ providerID: String) -> Bool {
        ClaudeProfile.isClaude(providerID: providerID)
            || CodexProfile.isCodex(providerID: providerID)
            || SystemProviders.isSystem(providerID: providerID)
    }

    /// Model cells are `providerID:model:…`. A new loaded model is not a new
    /// provider, and absence on the provider on-list cannot mean on for these.
    static func isModelCell(_ id: String) -> Bool {
        id.contains(":model:")
    }

    func isConnected(_ providerID: String) -> Bool {
        if Self.isModelCell(providerID) {
            return !disabledModels.contains(providerID)
        }
        if defaults.object(forKey: Keys.connected) != nil {
            return connectedProviders.contains(providerID)
        }
        if let hidden = pendingHidden {
            // Absence from the old off-list means on — for providers that
            // off-list could have named. MiniMax did not exist then, so
            // missing from it is not a choice to show it.
            if providerID == "minimax" { return false }
            return !hidden.contains(providerID)
        }
        return Self.isDefaultOnFamily(providerID)
    }

    func setConnected(_ connected: Bool, for providerID: String) {
        if Self.isModelCell(providerID) {
            if connected {
                disabledModels.remove(providerID)
            } else {
                disabledModels.insert(providerID)
            }
            return
        }
        if defaults.object(forKey: Keys.connected) == nil, let hidden = pendingHidden {
            let next = connected ? hidden.subtracting([providerID]) : hidden.union([providerID])
            pendingHidden = next
            defaults.set(Array(next), forKey: Keys.disconnected)
            seenProviders.insert(providerID)
            return
        }
        if defaults.object(forKey: Keys.connected) == nil {
            // First install: persist Claude and Codex as on, then apply this toggle.
            connectedProviders = ["claude", "codex"]
        }
        if connected {
            connectedProviders.insert(providerID)
        } else {
            connectedProviders.remove(providerID)
        }
        seenProviders.insert(providerID)
    }

    /// Fold this Mac's current provider ids into the stored on-list.
    ///
    /// First launch writes Claude and Codex. An upgrade from `hiddenProviders`
    /// inverts that off-list against `discoveredIDs`. After that, only a
    /// never-seen Claude or Codex id is added automatically. Model cells stay
    /// on `disabledModels` and are not inverted.
    func reconcile(discoveredIDs: [String]) {
        let discovered = Set(discoveredIDs.filter { !Self.isModelCell($0) })
        if defaults.object(forKey: Keys.connected) != nil {
            connectedProviders.subtract(connectedProviders.filter(Self.isModelCell))
            seenProviders.subtract(seenProviders.filter(Self.isModelCell))
            let novel = discovered.subtracting(seenProviders)
            for id in novel where Self.isDefaultOnFamily(id) {
                connectedProviders.insert(id)
            }
            seenProviders.formUnion(discovered)
            return
        }
        if let hidden = pendingHidden {
            // Invert the old off-list, then drop MiniMax: it did not exist
            // when that list was written, so absence from it is not "on".
            connectedProviders = discovered
                .subtracting(hidden.filter { !Self.isModelCell($0) })
                .subtracting(["minimax"])
            seenProviders = discovered
            pendingHidden = nil
            return
        }
        connectedProviders = Set(discovered.filter(Self.isDefaultOnFamily))
        seenProviders = discovered
    }

    /// What `UsageStore` still treats as the off-list, among ids it knows.
    func disconnectedIDs(among discovered: [String]) -> Set<String> {
        Set(discovered.filter { !isConnected($0) })
    }

    /// Record a new order, keeping the ids that are not on this Mac today.
    ///
    /// Settings can only show what was discovered at launch, so writing its
    /// list verbatim would quietly forget where a Claude profile sat the moment
    /// its directory was moved away — and put it back at the end when it
    /// returned, for something the user never did.
    func setProviderOrder(_ ids: [String]) {
        providerOrder = ProviderOrder.remember(ids, keeping: providerOrder)
    }

    // MARK: - Login item

    static var isRegisteredForLogin: Bool {
        SMAppService.mainApp.status == .enabled
    }

    private func applyLaunchAtLogin() {
        // A copy outside /Applications/Pulse.app never owns the login item (Runtime.fixedInstallPath).
        guard Runtime.isFixedInstall else {
            launchAtLoginProblem = L10n.t("macOS refused this — try moving Pulse to /Applications.")
            launchAtLogin = Self.isRegisteredForLogin
            return
        }
        do {
            if launchAtLogin {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            launchAtLoginProblem = nil
        } catch {
            // Commonly refused for an app running from a build directory rather
            // than /Applications, which is worth saying plainly.
            Log.usage.error("launch at login failed: \(error.localizedDescription, privacy: .public)")
            Log.record(.usage, "login_item.register_failed")
            launchAtLoginProblem = L10n.t("macOS refused this — try moving Pulse to /Applications.")
            launchAtLogin = Self.isRegisteredForLogin
        }
    }
}
#endif
