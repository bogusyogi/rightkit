#if os(macOS)
import Foundation
import RightKitHub

/// The five Settings pages, as the sidebar and the section ids know them.
enum SettingsPageKind: String, CaseIterable, Sendable {
    case permissions, accounts, appearance, notifications, general

    var title: String {
        switch self {
        case .permissions: return "Permissions"
        case .accounts: return "Accounts"
        case .appearance: return "Appearance"
        case .notifications: return "Notifications"
        case .general: return "General"
        }
    }

    var symbol: String {
        switch self {
        case .permissions: return "checkmark.shield"
        case .accounts: return "person.crop.circle"
        case .appearance: return "paintpalette"
        case .notifications: return "bell"
        case .general: return "gearshape"
        }
    }

    var order: Int {
        switch self {
        case .permissions: return 70
        case .accounts: return 80
        case .appearance: return 90
        case .notifications: return 100
        case .general: return 110
        }
    }
}

/// What changes the wording or the choices of a setting: the platform, the options the notch
/// published, and the displays it lists.
struct SettingsCatalogContext: Sendable, Equatable {
    var windows = false
    var options: [String: [String]] = SettingsCatalog.defaultOptions
    var displays: [SettingsDisplay] = []
    /// The notch publishes `toolWheelEnabled` (changes the Hover cards note).
    var hasToolWheel = true

    init() {}

    init(state: SettingsNotchState?) {
        guard let state else { return }
        windows = SettingsPlatform.isWindows || state.platform == "windows"
        // The notch's own lists win; a key it does not publish keeps the documented list for search.
        for (key, list) in state.options where !list.isEmpty { options[key] = list }
        displays = state.displays
        hasToolWheel = state.settings["toolWheelEnabled"] != nil
    }
}

/// Every Settings row as a `HubSettingsGroup` of `HubSetting`s with its documented default, so the
/// hub's global search finds each one, and the pages draw the same definitions.
///
/// Titles and notes are the React page's (hub/src/views/Settings.tsx and the two files it uses);
/// the default is the one in docs/settings.md and is shown by the row, so a note never repeats it.
enum SettingsCatalog {
    // MARK: Documented choices

    static let defaultOptions: [String: [String]] = [
        "chatClaudeHookDelivery": ["auto", "on", "off"],
        "devicePhoneRead": ["allow", "deny"],
        "devicePhoneSend": ["allow", "ask", "deny"],
        "notchEdge": ["right", "left", "top", "bottom"],
        "notchScope": ["mainDisplay", "allDisplays"],
        "notchVisibility": ["alwaysShow", "onHover", "hidden"],
        "notchSize": ["small", "medium", "large"],
        "notchSurfaceStyle": ["glass", "darkGlass", "solid"],
        "accentColor": ["system", "ff33e1", "eb4236", "eb8436", "ffd400", "00ff88", "00e5cc", "36a8eb", "6c5ce7", "b026ff", "f7f6f5"],
        "colorTransitionStyle": ["hardStep", "ramp"],
        "weeklyRing": ["off", "inside", "outside"],
        "resetTimeFormat": ["automatic", "remaining"],
        "language": ["system", "en", "fr", "de", "id", "ja", "ko", "pt-BR", "ru", "zh-Hans", "zh-Hant", "uk", "uz", "tr"],
        "notificationChannel": ["notch", "mac"],
        "peekDuration": ["brief", "standard", "long"],
        "launcherHotkey": ["optionSpace", "commandSpace", "controlSpace", "optionShiftSpace", "controlOptionSpace"],
        "launcherHotkeyConflict": ["pulse", "other"],
        "launcherWebSearchEngine": ["google", "bing", "duckduckgo"],
        "launcherAskTarget": ["inLauncher", "chatgpt", "off"],
        "clipboardCardSize": ["small", "medium", "large"],
        "clipboardStripPlacement": ["notch", "bottom"],
        "clipboardPasteMode": ["paste", "copy"],
    ]

    /// The keys whose value is one of a list of choices.
    static var choiceKeys: Set<String> {
        var keys = Set(defaultOptions.keys)
        keys.insert("displayPreference")
        return keys
    }

    // MARK: Tools and wheel (Settings.tsx TOOLS and WHEEL)

    struct ToolItem: Sendable, Equatable {
        var id: String
        var name: String
        var note: String
        var on: Bool
    }

    static let tools: [ToolItem] = [
        ToolItem(id: "snip", name: "Snip", note: "Drag a region to copy the screenshot to the clipboard.", on: true),
        ToolItem(id: "screen", name: "Screen", note: "Copy a screenshot of the whole screen to the clipboard.", on: true),
        ToolItem(id: "window", name: "Window", note: "Copy a screenshot of one window to the clipboard.", on: true),
        ToolItem(id: "lock", name: "Lock screen", note: "Lock the screen.", on: true),
        ToolItem(id: "clip", name: "Clipboard", note: "Open the clipboard strip. Absent while clipboard history is off.", on: true),
    ]

    static let wheel: [ToolItem] = [
        ToolItem(id: "snip", name: "Snip", note: "Drag a region to copy the screenshot to the clipboard.", on: true),
        ToolItem(id: "screen", name: "Screen", note: "Copy a screenshot of the whole screen to the clipboard.", on: true),
        ToolItem(id: "window", name: "Window", note: "Copy a screenshot of one window to the clipboard.", on: true),
        ToolItem(id: "settings", name: "System Settings", note: "Open macOS System Settings (not Pulse's settings).", on: true),
        ToolItem(id: "hub", name: "Open hub", note: "Open the Pulse hub.", on: true),
        ToolItem(id: "launcher", name: "Launcher", note: "Open the Pulse launcher; absent from the wheel while the launcher is off.", on: true),
        ToolItem(id: "clip", name: "Clipboard history", note: "Opens the clipboard strip (Clipboard history in the launcher when the strip is off).", on: true),
        ToolItem(id: "notes", name: "Notes", note: "Open the launcher on notes; absent while the launcher or its notes are off.", on: false),
        ToolItem(id: "lock", name: "Lock screen", note: "Lock the screen.", on: false),
        ToolItem(id: "mission", name: "Mission Control", note: "Open Mission Control.", on: false),
    ]

    // MARK: Builders

    private static func toggle(
        _ key: String, _ title: String, _ detail: String, on: Bool, _ keywords: [String] = []
    ) -> HubSetting {
        HubSetting.toggle(key: key, title: title, detail: detail, defaultValue: on, keywords: keywords)
    }

    private static func choice(
        _ key: String, _ title: String, _ detail: String, _ defaultValue: String,
        _ ctx: SettingsCatalogContext, _ keywords: [String] = []
    ) -> HubSetting {
        let listed = ctx.options[key] ?? []
        var choices = listed.map {
            HubChoice(value: $0, title: SettingsLabels.name(key: key, option: $0, windows: ctx.windows))
        }
        if key == "displayPreference" {
            choices = [HubChoice(value: "followActiveWindow", title: "Follow active window")]
                + ctx.displays.map { HubChoice(value: $0.id, title: $0.name) }
        }
        return HubSetting.choice(
            key: key, title: title, detail: detail, defaultValue: defaultValue, choices: choices, keywords: keywords)
    }

    /// A number, whole number, shortcut, path or list stored as text for search; the page draws its own control.
    private static func text(
        _ key: String, _ title: String, _ detail: String, _ defaultValue: HubSettingValue,
        _ placeholder: String = "", _ keywords: [String] = []
    ) -> HubSetting {
        HubSetting(
            key: key, title: title, detail: detail, defaultValue: defaultValue,
            kind: .text(placeholder: placeholder), keywords: keywords)
    }

    private static func group(
        _ section: String, _ name: String, _ title: String, _ settings: [HubSetting], _ keywords: [String] = []
    ) -> HubSettingsGroup {
        HubSettingsGroup(id: "settings.\(section).\(name)", title: title, settings: settings, keywords: keywords)
    }

    // MARK: Groups per page

    static func groups(_ kind: SettingsPageKind, _ ctx: SettingsCatalogContext) -> [HubSettingsGroup] {
        switch kind {
        case .permissions, .accounts: return []
        case .appearance: return appearance(ctx)
        case .notifications: return notifications(ctx)
        case .general: return general(ctx)
        }
    }

    static func allGroups(_ ctx: SettingsCatalogContext) -> [HubSettingsGroup] {
        SettingsPageKind.allCases.flatMap { groups($0, ctx) }
    }

    /// Every setting by key.
    static func lookup(_ ctx: SettingsCatalogContext) -> [String: HubSetting] {
        var out: [String: HubSetting] = [:]
        for group in allGroups(ctx) {
            for setting in group.settings { out[setting.key] = setting }
        }
        return out
    }

    static func appearance(_ ctx: SettingsCatalogContext) -> [HubSettingsGroup] {
        let drag = ctx.windows ? "Alt" : "Option"
        return [
            group("appearance", "placement", "Placement", [
                choice("notchEdge", "Edge", "Which screen edge the notch is on.", "right", ctx, ["side", "position", "dock"]),
                choice("notchScope", "Displays", "One notch on the main display, or one per display.", "mainDisplay", ctx, ["monitor", "screens"]),
                choice("displayPreference", "Display", "Which display the notch follows.", "followActiveWindow", ctx, ["monitor", "screen"]),
                choice("notchVisibility", "Show", "Always shown, a pill that opens on hover, or hidden. Hidden does not stop toasts.", "alwaysShow", ctx, ["hide", "hover", "pill"]),
                toggle("notifications", "Notifications",
                       "Turns off Pulse's toasts and alert sounds; cards that answer something you did, or that need an answer, still show. With Show set to Hidden, or the notch folded for a full-screen app, a toast brings the notch out and it goes again.",
                       on: true, ["toasts", "alerts", "sounds", "banner", "mute"]),
                toggle("foldsForFullScreen", "Fold for full-screen apps", "Folds away when a full-screen app is in front.", on: true, ["fullscreen"]),
            ]),
            group("appearance", "size", "Size and surface", [
                choice("notchSize", "Size", "Small, medium or large. A stored choice is kept; only an install that never chose gets the default.", "medium", ctx, ["scale"]),
                toggle("usesCustomNotchScale", "Custom size", "A scale instead of the size presets.", on: false, ["scale", "slider"]),
                text("customNotchScale", "Scale", "How large the notch is, from 0.5 to 1.5.", .number(1), "1.00", ["size", "slider"]),
                choice("notchSurfaceStyle", "Surface", "Glass or solid material.", "glass", ctx, ["material", "dark glass", "solid"]),
            ]),
            group("appearance", "parts", "Notch parts", [
                toggle("showsSettingsHandle", "Settings handle",
                       "The gear at the end of the notch, with its update and permission dots. Off hides it; Settings stays reachable by clicking a ring, from the hub's sidebar, and with the Open Settings shortcut.",
                       on: true, ["gear", "dots"]),
                toggle("notchMinimizeButton", "Minimise button",
                       "The button at the end of the notch opposite the drag handle. It minimises the notch to its Send cell and restores it; hover it to preview. Off hides it and the notch is never minimised.",
                       on: true, ["minimize", "minimise", "restore", "send", "collapse"]),
                toggle("notchOptionDrag", "\(drag)-drag to move the notch",
                       "Holding \(drag) and dragging slides the notch along its edge. Off leaves the Edge row and Reset position; the grip beside the gear still moves it.",
                       on: true, ["move", "drag", "position", "grip"]),
                toggle("showsHoverCards", "Hover cards",
                       "The card that opens when the pointer rests on a ring. The Tools buttons and the Send device list live on those cards" + (ctx.hasToolWheel ? "; the middle-click wheel still offers the tools." : "."),
                       on: true, ["hover", "popover", "ring"]),
            ]),
            group("appearance", "rings", "Rings", [
                toggle("showsNotchReadings", "Show % under rings", "Prints the use percentage under each ring.", on: false, ["percent", "text"]),
                choice("weeklyRing", "Second ring", "Where the second (weekly) reading is drawn.", "inside", ctx, ["weekly", "inside", "outside"]),
                toggle("weeklyRingDashed", "Dashed second ring", "Draws the second ring dashed.", on: false, ["weekly"]),
                toggle("weeklyHeadline", "Weekly limit as main ring", "Makes the weekly limit the main ring.", on: true, ["weekly", "headline"]),
                toggle("weeklyReading", "Show both readings", "Adds the second reading to the text under a ring.", on: false, ["weekly"]),
                toggle("claudeShowAllAccounts", "Ring per Claude account", "One Claude ring for each known account, tagged A, B and so on; off shows only the signed-in account.", on: true, ["accounts", "claude"]),
                toggle("claudeDailyPaceRing", "Claude daily pace ring", "Claude's main ring shows today's share of the week.", on: false, ["pace", "daily"]),
                toggle("showUsagePace", "Show usage pace", "Shows whether use is ahead of or behind an even pace.", on: false, ["pace"]),
                toggle("showCodexExtraLimits", "Codex extra limits", "Spark and code-review windows on Codex's hover card.", on: true, ["spark", "review"]),
                choice("resetTimeFormat", "Reset time", "How reset times read.", "automatic", ctx, ["date", "remaining"]),
            ]),
            group("appearance", "colour", "Colour", [
                choice("accentColor", "Accent", "Colour for positive use and active work.", "00ff88", ctx, ["color", "green", "blue", "pink"]),
                choice("colorTransitionStyle", "Colour transition", "Hard step or smooth ramp between colour bands.", "hardStep", ctx, ["color", "ramp"]),
                text("watchLimit", "Watch limit", "Where a ring turns orange, as a share of the limit.", .number(0.7), "70%", ["orange", "slider", "warning"]),
                text("criticalLimit", "Critical limit", "Where a ring turns red, as a share of the limit.", .number(0.9), "90%", ["red", "slider", "critical"]),
            ]),
            group("appearance", "language", "Language", [
                choice("language", "Language", "Language of the notch's own text.", "system", ctx, ["locale", "translation"]),
            ]),
        ]
    }

    static func notifications(_ ctx: SettingsCatalogContext) -> [HubSettingsGroup] {
        [
            group("notifications", "where", "Where", [
                choice("notificationChannel", "Channel", "Where alerts go: the notch or the system's banners.", "notch", ctx, ["banner", "notification center"]),
                choice("peekDuration", "Open the notch for", "How long the notch stays open when it opens by itself.", "standard", ctx, ["peek", "seconds", "duration"]),
            ]),
            group("notifications", "sessions", "Agent sessions", [
                toggle("watchesAgentSessions", "Watch agent sessions",
                       "Shows running Claude Code and Codex chats on the rings and opens the notch when one finishes. Off: nothing is watched, so there is no movement, no announcement and no sound for a finished chat.",
                       on: false, ["claude code", "codex", "chats"]),
                toggle("announceSessionEnd", "When a session finishes", "Peeks the notch when a session finishes or is blocked.", on: true, ["peek"]),
                toggle("sessionEndSound", "Play a sound", "Plays a sound with it.", on: false, ["chime"]),
            ]),
            group("notifications", "limits", "Limits", [
                toggle("announceUsageReset", "When a limit resets", "Announces when a limit resets.", on: true),
                toggle("usageResetSound", "Play a sound on reset", "Sound with the reset announcement.", on: false, ["chime"]),
                toggle("announceSessionLimitReached", "When the session limit is reached", "Announces when the session limit is hit.", on: true),
                toggle("announceWeeklyLimitReached", "When the weekly limit is reached", "Announces when the weekly limit is hit.", on: true),
                toggle("limitReachedSound", "Play a sound at a limit", "Sound with limit alerts.", on: false, ["chime"]),
            ]),
            group("notifications", "drives", "Drives", [
                toggle("announceDriveAlerts", "When a drive fills up or its health changes",
                       "The notch opens with the message when a drive is filling (70 percent used), almost full (90 percent) or failing.",
                       on: true, ["disk", "volume", "health", "space"]),
                toggle("driveAlertSound", "Play a sound for a drive alert",
                       "Plays the usage-reset chime when a drive alert is announced; only when drive alerts are on.",
                       on: false, ["disk", "chime"]),
            ]),
            group("notifications", "remote-approvals", "Remote approvals", [
                toggle("remoteApprovalAlerts", "When a remote tool waits for your approval",
                       "The notch shows what ChatGPT or another remote tool wants to do while it waits for you; a click opens General > Remote access to approve or deny it.",
                       on: true, ["chatgpt", "remote", "approve", "approval", "term", "screen"]),
            ]),
            group("notifications", "mute", "Mute alerts", [
                toggle("mute_claude_alerts", "Mute Claude alerts", "No notch cards for the limits of Claude or any of its extra accounts.", on: false),
                toggle("mute_codex_alerts", "Mute Codex alerts", "No notch cards for the limits of Codex or any of its extra profiles.", on: false),
            ]),
        ]
    }

    static func general(_ ctx: SettingsCatalogContext) -> [HubSettingsGroup] {
        let pc = ctx.windows ? "PC" : "Mac"
        let toolSwitches: [HubSetting] = toolSettings(ctx)
        var groups: [HubSettingsGroup] = [
            group("general", "startup", "Startup", [
                toggle("launchAtLogin", "Open Pulse at login", "Registers Pulse as a login item.", on: true, ["startup", "login item", "autostart"]),
            ]),
            group("general", "diagnosticsAccounts", "Diagnostics and Claude accounts", [
                toggle("hubDiagnosticsPage", "Show the Diagnostics page in the hub",
                       "Pulse's crash and hang reports and warnings, with only times, process names and short codes.",
                       on: true, ["crash", "hang", "reports", "warnings"]),
                toggle("claudeAccountScan", "Look for new Claude accounts",
                       "Looks for new Claude account folders and offers to bring this computer's chats into them. Mac only: the Windows card is not built.",
                       on: true, ["scan", "account", "folders"]),
            ]),
            group("general", "agent", "Agent", [
                toggle("agentRequests", "Answer agent requests (uses your Claude subscription)",
                       "Lets Pulse's background agent answer `pulse task ask` and voice requests with one bounded run of Claude Code, which uses your Claude subscription quota.",
                       on: false, ["agent", "claude", "task ask", "voice"]),
                toggle("agentActions", "Let the agent act on this computer (uses your Claude subscription)",
                       "Lets Pulse's background agent act for `pulse task act` and voice requests, using only the Pulse tools you have switched on under Remote access. Every act waits for your approval in Pulse.",
                       on: false, ["agent", "claude", "task act", "voice"]),
                toggle("membraneAutoEnroll", "Enrol repositories in Blueprint automatically",
                       "When Pulse lists a chat's working folder that is a git repository, it asks Membrane to enrol the repository (only if it is not already enrolled) and build its map, at most once a day per repository, while the resident runs. Off: Pulse does nothing on its own.",
                       on: true, ["membrane", "blueprint", "enrol", "repository", "map"]),
            ]),
            group("general", "deviceLinks", "Devices", [
                toggle("deviceLinkListen", "Accept device links (LAN only)",
                       "Lets another of your computers on this local network pair and link with this one. Nothing listens while this is off.",
                       on: false, ["device", "link", "pair", "lan"]),
                toggle("deviceMeshLocal", "Find my devices on this network",
                       "Finds your other computers on the local network, in addition to any relay.", on: true, ["device", "mesh", "lan"]),
                toggle("sendOverMesh", "Send over the mesh",
                       "Sends files to your other computers over the RightKit mesh as well as nearby sharing.", on: true, ["send", "mesh", "files"]),
                toggle("agentFileAutoAccept", "Accept agent files from my computers",
                       "Files an agent chat on one of your own paired computers sends a chat here (pulse chat send --file) are saved in <Save folder>/agents/<chat> without asking. Never from a guest, server or unclassified device. At most 20 files and 2 GiB a send and 10 GiB an hour; refused if under 5 GiB or 5% of the disk would stay free. Off: the usual Accept prompt (devices set to Allow or Deny skip it, as with any send). On by default.",
                       on: true, ["agent", "chat", "files", "send", "accept"]),
                text("sendDefaultDevice", "Default computer for paste",
                     "The paired device a paste on the Send cell goes to (its device id, shown on the Devices page). Empty: the only other Mac or Windows computer paired.",
                     .string(""), "Device id", ["send", "paste", "default", "device"]),
                text("deviceLinkRelayUrl", "Device link relay URL",
                     "The relay used when the other computer is not on this network. Empty: no relay.", .string(""), "https://", ["device", "relay", "link"]),
                toggle("chatOverDeviceLink", "Chat with paired computers over the device link",
                       "On: Pulse Chat uses a paired computer's device link beside ssh links, and accepts chat from paired computers you admitted or that hold a reversible grant. Off: both directions refuse and only ssh links carry chat.",
                       on: true, ["chat", "device", "link", "ssh"]),
                toggle("devicePhoneChats", "Let paired phones read and message my chats",
                       "Lets the Pulse iPhone app read and message your chats over the device link. Does nothing until a phone is paired.",
                       on: true, ["phone", "iphone", "chat"]),
                choice("devicePhoneRead", "New phones may read my chats", "What a phone paired from now on may do with your chat list.", "allow", ctx, ["phone", "read"]),
                choice("devicePhoneSend", "New phones may send to my chats",
                       "Ask holds a phone's message up to 60 s until you approve it on this computer.", "ask", ctx, ["phone", "send"]),
                toggle("devicePhoneTypeIntoChat", "Type my phone's messages into the chat",
                       "A message from one of your own paired phones is typed into the chat's composer in Claude Desktop and sent, so it arrives as your own message. Needs Accessibility. Never overwrites a draft; otherwise it sends the usual way.",
                       on: true, ["phone", "type", "composer", "accessibility", "chat"]),
                toggle("devicePhoneReceive", "Let paired phones pick up items I send them",
                       "Send lists your paired phones, and a phone can pick up the items sent to it from this computer. Off, Send lists no phone.",
                       on: true, ["phone", "send", "receive"]),
                text("devicePhoneReceiveTtlMinutes", "Minutes an item waits for a phone",
                     "An item the phone has not picked up in this time is deleted from this computer.", .number(60), "60", ["phone", "send", "minutes"]),
                choice("chatClaudeHookDelivery", "Deliver chat messages to Claude through the Pulse plugin",
                       "auto: on once a Pulse Claude plugin hook ran on this computer within 7 days; on or off overrides.", "auto", ctx, ["chat", "claude", "hook", "plugin"]),
            ]),
            group("general", "nearby", "Nearby sharing", [
                toggle("nearbyEnabled", "Send and receive files nearby",
                       "Works with the LocalSend app on iPhone, Android, \(ctx.windows ? "Mac" : "Windows") and Linux. Pulse can be found on your network while this is on.",
                       on: true, ["localsend", "airdrop", "send", "share", "files"]),
                text("nearbyAlias", "Device name", "How other devices list this \(pc). Empty uses the \(pc)'s name.", .string(""), "This \(pc)", ["alias", "name"]),
                text("nearbySaveFolder", "Save received files to", "Where received files go.", .string(""), "Downloads", ["folder", "downloads"]),
                HubSetting.choice(
                    key: "nearby.newDevices", title: "New devices",
                    detail: "What a device gets the first time it is seen. Allow sends and receives with no question; Ask asks every time; Deny refuses.",
                    defaultValue: "ask", choices: [HubChoice(value: "ask", title: "Ask"), HubChoice(value: "deny", title: "Deny")],
                    keywords: ["trust", "allow", "ask", "deny", "devices"]),
                toggle("chat.enabled", "Chat",
                       "Lets Claude and Codex chats on this computer message chats on linked computers over ssh. Link one with: pulse chat link <device> <ssh-host>",
                       on: true, ["chat", "agents", "ssh", "messages"]),
                toggle("chatRingLight", "Light the Send ring on Chat activity",
                       "The Send ring pulses for a few seconds when a Chat message is sent or arrives.", on: true, ["chat", "ring"]),
                toggle("chatPluginAutoInstall", "Install Pulse Chat into the agent apps it finds",
                       "Finds Claude Code, Codex and Muse here and installs Pulse's Chat integration where it is missing. Muse asks once on a notch card.",
                       on: true, ["chat", "plugin", "install", "claude", "codex", "muse"]),
                toggle("chat.claudeHookDelivery", "Deliver chat messages to Claude through the Pulse plugin",
                       "The message arrives as added context labelled as coming from another agent, after the chat's next tool call, at the end of its turn, or with the user's next prompt. An idle chat reads it only on its next turn. Requires the Pulse plugin installed in Claude Code.",
                       on: false, ["chat", "claude", "hook", "plugin"]),
                toggle("chat.terminalChats", "List terminal chats active in the last 6 hours",
                       "Lists interactive terminal chats (Claude Code and Codex in a terminal) alongside the app chats, but only while they have been active in the last 6 hours, so old terminal sessions do not clutter the list. Off, only app chats are listed. App chats are always listed.",
                       on: true, ["chat", "terminal", "cli", "claude", "codex", "active", "recent"]),
                toggle("chat.claudeProfiles", "Claude profiles in other home folders",
                       "Lists and messages Claude chats in other Claude profiles (~/ClaudeProfiles), labelled by profile. On by default.",
                       on: true, ["chat", "claude", "profile", "home", "folder"]),
                toggle("chat.muse", "Muse chats",
                       "Lists, reads and messages Muse Code chats, and lets Pulse's Muse plugin register them and hand them messages. Off, no Muse chat is listed or reached and the plugin's hooks do nothing. Install the plugin with pulse chat install --muse. On by default.",
                       on: true, ["chat", "muse", "plugin", "hook"]),
                toggle("chat.stampUserWords", "Stamp my words into messages",
                       "Each message a chat sends carries your last three typed turns, read by Pulse from that chat's transcript, so the receiving chat can check what you asked. Up to 3 KB, only to your own linked computers. On by default.",
                       on: true, ["chat", "stamp", "words", "turns", "transcript", "said"]),
                toggle("chat.codexWakeIdle", "Start a turn in an idle Codex chat when a message arrives",
                       "On, an idle Codex chat is started on the message without anyone pressing Steer, using Codex's own command line, with that chat's normal permissions, and it uses the account's Codex allowance. Only a chat that is positively idle is started, and never one the Codex app may hold: then the message waits for the chat's next prompt or tool call. Off, the message waits for the owner's next prompt. On by default. The app may not show the new turn until the chat is reopened.",
                       on: true, ["chat", "codex", "wake", "idle"]),
                toggle("chat.claudeWakeIdle", "Start a turn in an idle Claude chat when a message arrives",
                       "On, a message for an idle Claude Desktop chat (default profile) opens that chat in Claude Desktop so it starts a turn, and it uses the account's Claude allowance. A chat in another profile waits for its next turn. Off, the message waits in the chat's inbox. On by default.",
                       on: true, ["chat", "claude", "wake", "idle", "desktop"]),
                toggle("chat.museWakeIdle", "Start a turn in an idle Muse chat when a message arrives",
                       "On, Pulse starts a turn in an idle Muse chat on the message; takes effect when Muse offers a supported entry. Until then the message waits in the chat's inbox for its next prompt. On by default.",
                       on: true, ["chat", "muse", "wake", "idle"]),
                toggle("chat.receiptNotices", "Show receipts of messages you sent in chat hooks",
                       "On, a chat whose hook runs is told in a short line when a message it sent was seen by the receiving chat, picked up, released, refused or expired. At most one such notice a minute, batched. Off, the receipts are still kept and shown by pulse chat receipts.",
                       on: true, ["chat", "receipt", "receipts", "delivered", "seen", "notice"]),
            ]),
            group("general", "keyboardShortcuts", "Keyboard shortcuts", [
                toggle("mac_shortcuts", "Mac-style editing", "Left Alt+A, C, V, X, Z, S, F, T and W act as Ctrl+the same letter; Left Alt+Shift+Z redoes.", on: true, ["alt", "ctrl", "copy", "paste"]),
                toggle("altRemapSaveFindTabClose", "Alt+S, F, T and W too",
                       "Includes Save, Find, new Tab and Close among the Alt shortcuts. Off leaves those four keys to the app, for programs that use Alt+letter for their own menus. Needs Mac-style editing.",
                       on: true, ["alt", "save", "find", "tab", "close"]),
                toggle("screenshot_shortcuts", "Screenshot shortcuts", "Alt+Shift+4 drags a region; Alt+Shift+5 opens the screenshot toolbar.", on: true, ["snip", "capture"]),
                toggle("screenshot_to_desktop", "Save screenshots to the Desktop", "Off keeps them on the clipboard only. The Alt+Shift+5 toolbar changes this too.", on: false, ["snip", "capture"]),
            ]),
            group("general", "installers", "Installers", [
                toggle("convDiskImageInstaller", ctx.windows ? "Watch Downloads for installers" : "Install apps from disk images",
                       ctx.windows
                           ? "A new installer in Downloads brings up a card on the notch. Off, Pulse does not look at Downloads."
                           : "When a disk image holding an app mounts, the notch installs it to Applications and ejects the image, or asks first. Needs no permission.",
                       on: true, ["dmg", "disk image", "install", "msi", "msix", "downloads"]),
                toggle("installer_auto", "Install signed installers automatically",
                       "Watches Downloads and installs signed MSIX and MSI packages, with Undo. Unsigned ones always ask.", on: false, ["msi", "msix"]),
            ]),
            group("general", "updates", "Updates", [
                toggle("autoUpdateCheck", "Automatically check", "Looks for a new release at most every six hours. Nothing installs until you choose Update.", on: true, ["update", "release", "version"]),
            ]),
            group("general", "tools", "Tools", toolSwitches),
            group("general", "wheel", "Wheel", wheel.map { item in
                toggle("wheel.\(item.id)", item.name, item.note, on: item.on, ["wheel", "middle click"])
            }),
            group("general", "claudeAccounts", "Claude accounts", [
                toggle("claudeChatMirror", "Keep every account's chat list complete",
                       "Copies the signed-in Claude account's chats into the other accounts' lists whenever they change. Off: press Sync chats on the Claude card after you switch account; it closes Claude, syncs every account and opens Claude again.",
                       on: false, ["sync", "mirror"]),
                toggle("claudeChatRemember", "Remember which chats were running", "Lets a restart or an account switch reopen the chats that were open.", on: true, ["restore"]),
                toggle("claudeAccountSwitcher", "Claude account switcher",
                       "Account chips on the Claude card: switch Claude Desktop between your accounts (one data folder each, one open at a time) with the chats merged; Add account quits Claude for a one-time sign-in.",
                       on: true, ["claude", "account", "switch"]),
            ]),
            group("general", "background", "Background", [
                toggle("hubDriveHealth", "Sample drive health", "Reads each drive's health every two minutes for alerts and history.", on: true, ["smart", "disk"]),
                toggle("hubMetricsHistory", "Keep CPU, memory and network history", "Samples every two seconds and keeps 30 minutes for Monitor's charts.", on: true, ["monitor", "cpu", "memory"]),
                toggle("hubFileIndex", "Index file names", "Builds a file-name index of each disk for fast search in Storage. Applies the next time the hub starts.", on: true, ["storage", "search"]),
                toggle("hubFileIndexWholeDisk", "Index the whole disk (default: home, Applications and chosen volumes)", "Off: the index covers your home folder, /Applications and the extra volumes below. On: every mounted volume. Applies the next time the hub starts.", on: false, ["index", "scope", "whole disk", "volumes"]),
                text("hubFileIndexVolumes", "Extra volumes to index", "Volumes the index covers as well, one mount path per line, for example /Volumes/Archive. Empty by default.", .other, "/Volumes/Archive", ["index", "scope", "volumes"]),
                text("hubFileIndexExcludes", "Excluded folders (names or paths; also read from .pulseignore)", "Folders the index skips: a name skips it wherever it appears; a path such as Library/Caches skips that folder. Default: target, node_modules, .git/objects, DerivedData, .build, build, dist, Library/Caches, Library/Logs, .Trash, .gradle, .cache, __pycache__, venv, .venv, .rightkit-managed, .rustright, .build-home. Windows adds AppData\\Local\\Temp, $Recycle.Bin and System Volume Information.", .other, "target", ["index", "exclude", "ignore", "folders", "whole disk"]),
                toggle("mediaCompression", "Compress images and videos", "Adds Compress a copy to a JPEG, PNG, MP4 or MOV file in Storage. It writes a smaller copy in a Pulse Compressed folder beside the file and never changes the original.", on: false, ["storage", "image", "video", "compress"]),
                toggle("hubFileIndexAutoPause", "Pause the file index when it misbehaves", "Pauses the index for a while when it stays unhealthy, and switches it off on the third trip.", on: true, ["pause", "trips", "unhealthy", "switch off"]),
                text("hubFileIndexMaxTrips", "Trips before the index is switched off", "Trips before the file index is switched off.", .number(3), "3", ["pause", "trips"]),
                text("hubFileIndexPauseMinutes", "First pause, in minutes", "The first pause, in minutes; each later pause doubles it.", .number(5), "5", ["pause", "minutes"]),
                text("hubFileIndexMaxCpuPercent", "Index CPU limit (percent of one core)", "Hub CPU above which the file index counts as unhealthy.", .number(100), "100", ["cpu", "unhealthy"]),
                text("hubFileIndexMaxFootprintMb", "Index memory limit (MB)", "Hub memory above which the file index counts as unhealthy.", .number(2048), "2048", ["memory", "unhealthy"]),
                text("hubFileIndexMaxWriteMbPerHour", "Index write limit (MB per hour)", "Disk writes above which the file index counts as unhealthy.", .number(2048), "2048", ["writes", "unhealthy"]),
                toggle("chromeSnapshotCount", "Count Chrome's signing copies", "Once a day, counts the leftover code-signing copies Chrome keeps, for Cleanup's history line.", on: true, ["cleanup", "chrome"]),
                toggle("cleanupDeveloperItems", "Developer items in Cleanup", "Cleanup also offers developer caches, build output and old release builds (newest two kept).", on: true, ["cleanup", "developer", "caches"]),
                toggle("engineMemoryGuard", "Restart the engine if its memory runs away", "Restarts the Pulse engine when its memory passes the cap below.", on: true, ["engine", "memory", "restart"]),
                text("engineMemoryCapMb", "Engine memory cap (MB)", "The memory the engine may use before the guard restarts it (512 to 65536).", .number(1024), "1024", ["engine", "memory", "cap"]),
                toggle("hubAutoScan", "Scan when Storage or Cleanup opens", "Scans by itself when there is no saved result or it is old. Off shows what was saved until you press Rescan.", on: true, ["rescan", "storage", "cleanup"]),
            ]),
            group("general", "hubHealth", "Hub health", [
                toggle("hubWatchdog", "Hub resource watchdog", "Checks the hub's own CPU, memory, threads and disk writes, and reports a sustained overrun. It never acts.", on: true, ["watchdog", "cpu", "memory", "threads", "writes", "health"]),
                toggle("hubDiagReports", "Watch macOS reports about Pulse", "Lists new Pulse crash and resource reports in the macOS diagnostic folders.", on: true, ["crash", "diagnostic", "report", "ips"]),
                toggle("hubLog", "Keep a hub log", "Keeps the hub's lifecycle in its log. No file names or contents.", on: true, ["log", "diagnostics"]),
                text("hubWatchdogCpuPercent", "Watchdog CPU limit (percent)", "CPU budget for the watchdog, in percent of one core.", .number(5), "5", ["cpu", "watchdog"]),
                text("hubWatchdogMinutes", "Watchdog window (minutes)", "How long an overrun must last before the watchdog reports it.", .number(10), "10", ["minutes", "watchdog"]),
                text("hubWatchdogFootprintMb", "Watchdog memory limit (MB)", "Memory budget for the watchdog.", .number(1024), "1024", ["memory", "footprint", "watchdog"]),
                text("hubWatchdogThreads", "Watchdog thread limit", "Thread budget for the watchdog.", .number(200), "200", ["threads", "watchdog"]),
                text("hubWatchdogWriteMbPerHour", "Watchdog write limit (MB per hour)", "Disk write budget for the watchdog.", .number(500), "500", ["writes", "watchdog"]),
            ]),
            group("general", "remoteAccess", "Remote access", [
                toggle("remoteTools", "Remote tools", "Read-only access to this computer's files and chats by agents through the pulse CLI.", on: false, ["remote", "agents", "files", "chat", "cli"]),
                text("remoteRoots", "Approved folders", "Folders remote file tools may read.", .other, "/path/to/folder", ["folders", "files", "roots"]),
                text("remoteBlockedApps", "Blocked apps", "Apps Pulse's agents may never open, see or control.", .other, "com.example.app", ["blocked", "apps", "block list", "launch", "screen"]),
                text("remoteChatRead", "Chats a remote reader may read", "Chats pulse chat read may return, by chat id.", .other, "chat id", ["chat", "read", "chats"]),
                toggle("phoneChatReadAll", "My paired phones may read every chat", "Your own paired phones read every chat on this computer; off limits them to the chats selected here. Never applies to cloud agents or the gateway.", on: true, ["phone", "chat", "read", "all chats"]),
                text("remoteReadMaxBytes", "Largest file read", "The most bytes one file read returns.", .number(1_048_576), "1048576", ["bytes", "read", "limit"]),
                text("remoteFindMaxResults", "Most find results", "The most results one file search returns.", .number(200), "200", ["find", "results", "limit"]),
                text("remoteChatReadMaxTurns", "Most chat turns", "The most turns one chat read returns.", .number(50), "50", ["turns", "chat", "limit"]),
                text("remoteChatReadMaxBytes", "Most chat text", "The most bytes of turn text one chat read returns.", .number(262_144), "262144", ["bytes", "chat", "limit"]),
                text("remoteImageMaxBytes", "Largest image", "The most bytes of source image the image reader would accept.", .number(10_485_760), "10485760", ["image", "bytes", "limit"]),
                toggle("remoteGateway", "ChatGPT gateway", "Lets ChatGPT reach this computer's read-only remote tools through OpenAI's Secure MCP Tunnel. Needs Remote tools.", on: false, ["chatgpt", "gateway", "openai", "mcp"]),
            ]),
            group("general", "launcher", "Launcher", launcher(ctx)),
            group("general", "keyboard", "Keyboard", [
                text("settingsHotkey", "Open Settings with a shortcut", "A global key that opens Settings. Off until you set one.", .string(""), "ctrl+opt+s", ["hotkey", "shortcut"]),
                toggle("convFnCommand", "Fn works as Command", "Fn+C/V/X/A/Z/S/F/T/W; Fn+arrows move by word.", on: false, ["fn", "command", "keyboard"]),
            ]),
            group("general", "autoQuit", "Auto Quit", [
                toggle("convAutoQuit", "Auto Quit", ctx.windows
                       ? "Closes the apps below when their last window closes. Only apps you add; an app with a minimized window, or one that keeps running in the tray, is left open."
                       : "Quits the apps below when their last window closes. Only apps you add; windows on other Spaces or minimized keep an app open.",
                       on: false, ["quit", "close", "apps"]),
            ]),
            group("general", "readings", "Readings", [
                toggle("gpuTemperature", "GPU temperature", "Shows the NVIDIA graphics card's temperature on the System card. It is read only while that card is open.", on: true, ["nvidia"]),
                toggle("asksProviderOnLook", "Ask the provider every time you look", "Spends a request each time. Useful to check against a provider's own page.", on: false, ["refresh", "usage"]),
            ]),
            group("general", "conveniences", "Conveniences", [
                toggle("convFinderCutPaste", "Cut and paste in Finder", "⌘X (or Fn X) marks the selected items; ⌘V in a Finder window moves them there. Finder asks before replacing anything.", on: true, ["finder", "move"]),
                toggle("finderPathField", "Path field in Finder", "Shows the front Finder window's folder in a small field above it. Type or paste a path and press Return to go there. Needs Accessibility and Finder Automation (macOS asks the first time).", on: false, ["finder", "path", "folder"]),
                toggle("convWindowMaximizer", "Green button maximizes", "Fills the screen without a full-screen Space. Option-click keeps the usual behaviour.", on: false, ["zoom", "window"]),
                toggle("convDockClickMinimize", "Dock click minimizes", "Clicking the Dock icon of the frontmost app minimizes its windows.", on: false, ["dock"]),
                toggle("convDiskImageAuto", "Install apps from disk images automatically", "Only signed, notarized apps that aren't installed yet; others ask in the notch.", on: true, ["dmg"]),
                toggle("convDiskImageAutoUpdate", "Also update apps that are already installed", "Replaces an older installed version; Undo restores it from the Trash.", on: false, ["dmg"]),
                toggle("convDiskImageTrashDownload", "Move the downloaded disk image to the Trash", "After a successful install, the .dmg you opened goes to the Trash.", on: false, ["dmg"]),
                toggle("finderMenuCut", "Finder menu: Cut", "Cut in Finder's right-click menu, for selected items. Shown while Pulse runs.", on: true, ["finder", "context menu"]),
                toggle("finderMenuCopyPath", "Finder menu: Copy Path", "Copy Path in Finder's right-click menu. Shown while Pulse runs.", on: true, ["finder", "context menu"]),
                toggle("finderMenuTerminal", "Finder menu: Open in Terminal", "Open in Terminal in Finder's right-click menu. Shown while Pulse runs.", on: true, ["finder", "context menu"]),
            ]),
            group("general", "windowManagement", "Window management", [
                toggle("windowManagementEnabled", "Move windows with shortcuts",
                       ctx.windows
                           ? "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Each shortcut can be turned off or changed. On a keyboard with AltGr, a Ctrl+Alt shortcut takes the place of the character AltGr types with that key."
                           : "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Each shortcut can be turned off or changed. Needs Accessibility permission.",
                       on: false, ["snap", "tile", "halves", "quarters", "thirds", "maximize"]),
            ]),
        ]
        // The Hover cards note and similar wording follow the platform; nothing else varies here.
        groups.removeAll { $0.settings.isEmpty }
        return groups
    }

    private static func toolSettings(_ ctx: SettingsCatalogContext) -> [HubSetting] {
        let wheelNote = ctx.windows
            ? "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links in programs that report them."
            : "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links. Needs Accessibility permission."
        var out: [HubSetting] = [
            toggle("toolWheelEnabled", "Middle-click wheel", wheelNote, on: true, ["mouse", "tools", "wheel"]),
        ]
        for item in tools {
            out.append(toggle("tools.\(item.id)", item.name, item.note, on: item.on, ["tool", "cell"]))
        }
        return out
    }

    private static func launcher(_ ctx: SettingsCatalogContext) -> [HubSetting] {
        [
            toggle("launcherEnabled", "Enable the launcher",
                   ctx.windows ? "Alt+Space opens a search of your apps, running programs and Pulse's pages." : "Search apps, files and Pulse commands, and calculate.",
                   on: false, ["spotlight", "search", "alfred", "raycast"]),
            choice("launcherHotkey", "Shortcut", "The global key that opens the launcher. Command space only works after Spotlight's shortcut is turned off in System Settings.", "optionSpace", ctx, ["hotkey", "space"]),
            choice("launcherHotkeyConflict", "Who keeps the shortcut",
                   "When another app such as ChatGPT also uses the launcher's shortcut, Pulse asks once. Pulse takes it ahead of the other app (needs Accessibility), or the other app keeps it. Choosing a different shortcut asks again.",
                   "", ctx, ["conflict", "chatgpt", "raycast", "alfred", "spotlight", "override"]),
            choice("launcherAskTarget", "Ask row",
                   "Where the launcher's Ask row sends your text: answered in the launcher by Pulse's assistant, opened in ChatGPT (the app, or chatgpt.com when it is not installed), or no Ask row.",
                   "inLauncher", ctx, ["ai", "assistant", "chat", "chatgpt"]),
            toggle("launcherWebSearch", "Search the web",
                   "A row under the results that searches the web with your text in your default browser.",
                   on: true, ["google", "bing", "duckduckgo"]),
            choice("launcherWebSearchEngine", "Search engine", "The search the web row uses.", "google", ctx, ["google", "bing", "duckduckgo"]),
            toggle("launcherDictionary", "Dictionary", "“define word” looks words up in the Mac's dictionaries.", on: true, ["define"]),
            toggle("launcherShortcuts", "Apple Shortcuts", "Search and run the shortcuts in the Shortcuts app.", on: true, ["automation"]),
            toggle("launcherCalculator", "Calculator and unit conversion", "Answers sums and conversions such as “12 km in mi”.", on: true, ["math", "units"]),
            toggle("launcherPulseCommands", "Pulse commands", "Open Storage, Cleanup, Monitor, Apps and Settings, and clear clipboard history, from the launcher.", on: true),
            toggle("launcherSnippetExpansion", "Expand snippet keywords as you type",
                   "Watches what you type to spot a snippet's keyword and replaces it. Nothing typed is stored or sent. Needs Accessibility. A keyword must start with a symbol (such as ;sig or /addr) and be 2 to 32 characters. It expands only in a text field, never in password fields, in Pulse's own windows, or while a password manager's item is on the clipboard. Snippets that use {argument} are skipped.",
                   on: false, ["text expander", "keyboard"]),
            toggle("launcherCurrency", "Currency and crypto rates", "Fetches exchange rates once a day from open.er-api.com and CoinGecko.", on: false, ["exchange", "bitcoin"]),
            toggle("launcherClipboard", "Clipboard history",
                   "Keeps what you copy on this \(ctx.windows ? "PC" : "Mac"): text, links, colours, code, images and files, searchable with “clip” and in the clipboard strip. It works with the launcher on or off. Turning it off keeps the history and reads nothing from the clipboard; Clear clipboard history deletes it.",
                   on: false, ["clip", "copy", "paste", "history"]),
            toggle("clipboardStripEnabled", "Show the clipboard strip",
                   "Allows the hot key, the Tools cell item and the wheel item to open the strip of cards. It works with the launcher on or off. Off: history is still kept, the hot key is released, and the items open the launcher's “clip” when the launcher is on. Only matters once Clipboard history is on.",
                   on: true, ["clipboard", "strip"]),
            text("clipboardHotkey", "Clipboard strip shortcut",
                 "The global key that opens the clipboard strip. Needs at least one of \(ctx.windows ? "Ctrl, Alt or Shift" : "Control, Option, Shift or Command"). Escape cancels.",
                 .string(ctx.windows ? "alt+shift+v" : "cmd+shift+v"), "cmd+shift+v", ["clipboard", "hotkey"]),
            choice("clipboardCardSize", "Card size", "Small, Medium or Large clipboard cards. The size is multiplied by the notch size scale, clamped to 0.8 to 1.25.", "medium", ctx, ["clipboard"]),
            choice("clipboardStripPlacement", "Strip position", "Where the strip appears. Notch: out of the notch, on its edge. Bottom: along the bottom of the display under the pointer, with no neck.", "notch", ctx, ["clipboard"]),
            toggle("clipboardShowNumbers", "Show numbers on cards", "Shows the 1 to 9 quick-paste number on clipboard cards.", on: true, ["clipboard"]),
            choice("clipboardPasteMode", "When I choose a clip",
                   "Paste the chosen clipboard item into the app you were in, or only copy it.\(ctx.windows ? "" : " Paste needs Accessibility.")", "paste", ctx, ["clipboard"]),
            toggle("clipboardPlainDefault", "Paste as plain text by default", "In the clipboard strip, Enter pastes plain text and Shift+Enter pastes as copied (swaps the two).", on: false, ["clipboard"]),
            toggle("clipboardWriteBackExcluded", "Keep pasted clips out of Windows history", "Windows only. When Pulse pastes a clipboard item it marks it so Windows' own Win+V history does not record it again.", on: true, ["clipboard", "win+v"]),
            toggle("clipboardCaptureImages", "Keep images", "Stores copied images and screenshots in the clipboard history. Off: image copies are skipped.", on: true, ["clipboard"]),
            toggle("clipboardCaptureFiles", "Keep copied files", "Stores copied file lists (paths only) in the clipboard history. Off: file copies are skipped.", on: true, ["clipboard"]),
            toggle("clipboardCaptureRich", "Keep formatting", "Stores HTML and RTF with the text in the clipboard history. Off: rich copies are stored as plain text.", on: true, ["clipboard", "html", "rtf"]),
            text("clipboardMaxItems", "Keep at most (items)", "Cap on clipboard items in the main lane (50 to 5000). Pinned and board items stay.", .number(500), "500", ["clipboard", "limit"]),
            text("clipboardMaxMB", "Keep at most (MB)", "Cap on the clipboard history's total size in MB (20 to 4096).", .number(200), "200", ["clipboard", "limit", "size"]),
            text("clipboardMaxAgeDays", "Keep for (days)", "Days since last use before an unpinned clipboard item goes (1 to 365; 0 is forever).", .number(30), "30", ["clipboard", "expire"]),
            text("clipboardScreenshotHours", "Keep screenshots for (hours)", "Hours before a clipboard screenshot goes (1 to 720; 0 is until a size cap).", .number(24), "24", ["clipboard", "expire"]),
            toggle("clipboardSkipAutoGenerated", "Skip copies made by programs", "Ignores clipboard copies that apps mark as made by a program (AutoGeneratedType). Mac only.", on: true, ["clipboard"]),
            toggle("clipboardSkipRemote", "Skip copies from other devices", "Ignores Universal Clipboard copies from your iPhone or iPad. Mac only.", on: true, ["clipboard", "universal clipboard"]),
            toggle("clipboardSecretFilter", "Skip keys and tokens", "Ignores a clipboard copy that is only a private key, access key, token or password-like string from a known pattern.", on: true, ["clipboard", "secrets", "passwords"]),
            text("clipboardExcludedApps", "Apps to ignore", "Clipboard copies from these apps are never kept. One app id per line. Password managers by default.", .string(""), "", ["clipboard", "exclude", "password manager"]),
            text("clipboardIgnoredTypes", "Types to ignore", "Extra pasteboard types that make the clipboard history skip a copy. One per line. Mac only.", .string(""), "", ["clipboard", "pasteboard"]),
            text("clipboardShiftPasteApps", "Terminals", "Windows only. Terminals where the clipboard strip pastes with Ctrl+Shift+V: app names, one per line. Windows Terminal, WezTerm, Alacritty and kitty by default.", .string(""), "", ["clipboard", "terminal"]),
            toggle("clipboardRememberBoard", "Remember the last board", "Opens the clipboard strip on the board you left it on.", on: false, ["clipboard"]),
        ]
    }

    // MARK: Search entries beyond the settings

    /// What global search finds that is not a switch: the Permissions and Accounts pages, the gauges,
    /// the launcher's lists, and the actions.
    static func entries(_ kind: SettingsPageKind) -> [HubSearchEntry] {
        switch kind {
        case .permissions:
            return [
                HubSearchEntry(id: "permissions", title: "Permissions", detail: "Which permission each switched-on feature needs", keywords: ["privacy", "access", "allow"]),
                HubSearchEntry(id: "helper", title: "Background helper", detail: "Lets Pulse move apps owned by root to the Trash without a password", keywords: ["uninstall", "root", "password"]),
                HubSearchEntry(id: "accessibility", title: "Accessibility", detail: "Lets Pulse watch the keyboard, mouse and Dock, and press keys and move windows", keywords: ["permission"]),
                HubSearchEntry(id: "screenRecording", title: "Screen Recording", detail: "Lets the Snip, Screen and Window tools capture the screen", keywords: ["permission", "screenshot"]),
                HubSearchEntry(id: "fullDiskAccess", title: "Full Disk Access", detail: "Lets the Storage scan open folders macOS protects", keywords: ["permission", "fda", "storage"]),
                HubSearchEntry(id: "finderMenu", title: "Finder menu", detail: "Adds Cut, Copy Path and Open in Terminal to Finder's right-click menu", keywords: ["permission", "extension"]),
                HubSearchEntry(id: "localNetwork", title: "Local network", detail: "Lets Pulse find computers on your network and send files to them", keywords: ["permission", "firewall", "nearby"]),
                HubSearchEntry(id: "automation", title: "Automation of Finder", detail: "Reads Finder selections and destination folders for Cut and Paste", keywords: ["permission", "apple events"]),
                HubSearchEntry(id: "login", title: "Launch at login", detail: "Starts Pulse automatically each time you sign in", keywords: ["permission", "startup"]),
                HubSearchEntry(id: "staleGrants", title: "Clean up old apps", detail: "Finds permissions still granted to apps that are no longer installed", keywords: ["tcc", "tccutil", "reset", "old permissions", "stale"]),
            ]
        case .accounts:
            return [
                HubSearchEntry(id: "accounts", title: "Logins Pulse reads", detail: "It never signs in itself; each login can be hidden, forgotten or reordered", keywords: ["accounts", "sign in", "claude", "codex", "providers"]),
                HubSearchEntry(id: "order", title: "Gauge order", detail: "Order of the rings in the notch; move a login up or down", keywords: ["order", "rings", "up", "down"]),
                HubSearchEntry(id: "forget", title: "Forget reading", detail: "Clears what Pulse read; you stay signed in to the provider itself", keywords: ["sign out", "clear"]),
                HubSearchEntry(id: "claudeAccounts", title: "Claude accounts", detail: "Each account folder Claude Desktop has, with session and weekly use", keywords: ["rename", "forget", "session", "weekly"]),
            ]
        case .appearance:
            return [
                HubSearchEntry(id: "gauges", title: "Gauges", detail: "Show or hide each notch cell: Claude, Codex, CPU, Memory, Disks, Send, Tools", keywords: ["rings", "cells", "claude", "codex", "cpu", "memory", "disks", "send", "tools"]),
                HubSearchEntry(id: "resetPosition", title: "Reset position", detail: "Puts the notch back in the middle of its edge", keywords: ["notch", "move", "centre"]),
            ]
        case .notifications:
            return [
                HubSearchEntry(id: "tryAlerts", title: "Try the alerts", detail: "Send a test notification, or preview the reset, session limit and weekly limit alerts", keywords: ["test", "preview"]),
            ]
        case .general:
            return [
                HubSearchEntry(id: "updatesCheck", title: "Check now", detail: "Check for a new version of Pulse, and install it", keywords: ["update", "version", "release", "install"]),
                HubSearchEntry(id: "nearbyDevices", title: "Devices", detail: "Allow, Ask or Deny each nearby device; rename or forget one", keywords: ["trust", "nearby", "sharing", "localsend", "fingerprint"]),
                HubSearchEntry(id: "pulseSkill", title: "Pulse skill", detail: "Install the Pulse skill for Claude and Codex chats", keywords: ["chat", "skill", "install"]),
                HubSearchEntry(id: "uninstallHelper", title: "Uninstall without password", detail: "Lets Pulse move root-owned apps and their files to the Trash with no administrator password", keywords: ["uninstall", "helper", "root", "login items"]),
                HubSearchEntry(id: "launcherFolders", title: "Folders for file search", detail: "Pulse searches file names itself, with no Spotlight index, only inside these folders; empty turns file search off; Command space stays taken until Spotlight's shortcut is turned off in System Settings", keywords: ["launcher", "files", "spotlight"]),
                HubSearchEntry(id: "launcherPins", title: "Pinned apps", detail: "Pinned apps show first in the launcher when nothing is typed", keywords: ["launcher"]),
                HubSearchEntry(id: "launcherAppHotkeys", title: "App hotkeys", detail: "A key that shows an app, or hides it when it is in front", keywords: ["launcher", "shortcut"]),
                HubSearchEntry(id: "launcherQuicklinks", title: "Quicklinks", detail: "A keyword that opens a web address with your text filled in", keywords: ["launcher", "bookmarks", "url"]),
                HubSearchEntry(id: "launcherSnippets", title: "Snippets", detail: "Markdown text pasted into the app you were using", keywords: ["launcher", "text", "expansion"]),
                HubSearchEntry(id: "launcherAliases", title: "Shortcut aliases", detail: "Type an alias in the launcher to put an Apple Shortcut first", keywords: ["launcher", "shortcuts"]),
                HubSearchEntry(id: "launcherNotes", title: "Notes in the launcher", detail: "Type notes in the launcher to list, search or start a note", keywords: ["launcher", "markdown"]),
                HubSearchEntry(id: "launcherCommands", title: "Custom commands", detail: "Run a shell command from the launcher and show its output", keywords: ["launcher", "script", "zsh"]),
                HubSearchEntry(id: "clearClipboard", title: "Clear clipboard history", detail: "Deletes every clipboard item, including pinned and board items", keywords: ["clipboard", "delete", "wipe"]),
                HubSearchEntry(id: "windowShortcuts", title: "Window shortcuts", detail: "Each window action has a shortcut that can be turned off or changed; reset to defaults", keywords: ["window management", "snap", "tile", "left half", "right half", "maximize"]),
                HubSearchEntry(id: "autoQuitApps", title: "Auto Quit apps", detail: "Add a running app, or remove one from the list", keywords: ["quit", "close"]),
                HubSearchEntry(id: "refreshNow", title: "Refresh now", detail: "Reads every provider again", keywords: ["readings", "usage"]),
                HubSearchEntry(id: "hubProcess", title: "Hub process", detail: "Whether the hub is running, starting or restarting; Retry starts it again when it is stopped", keywords: ["hub", "retry", "restart", "latched", "process"]),
            ]
        }
    }
}

/// How an option the notch publishes reads in a choice row (Settings.tsx `label`, `ACCENT_NAMES`
/// and `LANGUAGE_NAMES`).
enum SettingsLabels {
    static let languageNames: [String: String] = [
        "system": "Follow System", "en": "English", "fr": "Français", "de": "Deutsch", "id": "Bahasa Indonesia",
        "ja": "日本語", "ko": "한국어", "pt-BR": "Português (Brasil)", "ru": "Русский", "zh-Hans": "简体中文",
        "zh-Hant": "繁體中文", "uk": "Українська", "uz": "Oʻzbekcha", "tr": "Türkçe",
    ]

    static func accentNames(windows: Bool) -> [String: String] {
        [
            "system": windows ? "Windows accent colour" : "Device accent color",
            "ff33e1": "Pink", "eb4236": "Red", "eb8436": "Orange", "ffd400": "Yellow", "00ff88": "Green",
            "00e5cc": "Teal", "36a8eb": "Blue", "6c5ce7": "Indigo", "b026ff": "Purple", "f7f6f5": "Off-white",
        ]
    }

    static func name(key: String, option: String, windows: Bool) -> String {
        if key == "accentColor", let name = accentNames(windows: windows)[option] { return name }
        if key == "language", let name = languageNames[option] { return name }
        if key == "launcherHotkeyConflict" { return option == "pulse" ? "Pulse takes it" : "The other app keeps it" }
        if key == "launcherAskTarget" {
            switch option {
            case "inLauncher": return "In the launcher"
            case "chatgpt": return "ChatGPT"
            default: return "Off"
            }
        }
        if key == "launcherWebSearchEngine" { return option == "duckduckgo" ? "DuckDuckGo" : label(option, windows: windows) }
        if key == "launcherHotkey" {
            switch option {
            case "optionShiftSpace": return "Option Shift Space"
            case "controlOptionSpace": return "Control Option Space"
            default: break
            }
        }
        return label(option, windows: windows)
    }

    /// "darkGlass" becomes "Dark glass"; the Windows notch's "mac" channel reads "Windows".
    static func label(_ raw: String, windows: Bool) -> String {
        if windows && raw == "mac" { return "Windows" }
        var spaced = ""
        var previous: Character?
        for character in raw {
            if let before = previous, before.isLowercase, character.isUppercase { spaced.append(" ") }
            spaced.append(character)
            previous = character
        }
        var out = ""
        var first = true
        var afterSpace = false
        for character in spaced {
            if first {
                out.append(contentsOf: String(character).uppercased())
                first = false
            } else if afterSpace && character.isUppercase {
                out.append(contentsOf: String(character).lowercased())
            } else {
                out.append(character)
            }
            afterSpace = character == " "
        }
        return out
    }
}
#endif
