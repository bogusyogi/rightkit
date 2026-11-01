#if os(macOS)
import Foundation

/// The line the Tauri page prints under a setting, word for word (hub/src/views/Settings.tsx,
/// NearbySettings.tsx and LauncherSettings.tsx). A key with no entry has no line in the Tauri page, so
/// its row shows none either. The catalog keeps its own (shorter, default-free) details for the
/// kit's global search; the documented defaults stay in docs/settings.md and in the notes below.
enum SettingsNotes {
    /// What decides the wording: the platform, and whether the notch publishes the middle-click wheel
    /// or the clipboard history (the Hover cards and the app commands notes mention them).
    struct Context: Sendable {
        var windows: Bool
        var hasToolWheel: Bool
        var hasClipboard: Bool

        init(windows: Bool = false, hasToolWheel: Bool = true, hasClipboard: Bool = true) {
            self.windows = windows
            self.hasToolWheel = hasToolWheel
            self.hasClipboard = hasClipboard
        }
    }

    static func note(_ key: String, _ context: Context) -> String? {
        if let fixed = fixed[key] { return fixed }
        let drag = context.windows ? "Alt" : "Option"
        let machine = context.windows ? "PC" : "Mac"
        switch key {
        case "notchOptionDrag":
            return "Holding \(drag) and dragging slides the notch along its edge. On by default. Off leaves the Edge row and Reset position; the grip beside the gear still moves it."
        case "showsHoverCards":
            return "The card that opens when the pointer rests on a ring. On by default. The Tools buttons and the Send device list live on those cards"
                + (context.hasToolWheel ? "; the middle-click wheel still offers the tools." : ".")
        case "convDiskImageInstaller":
            return context.windows
                ? "A new installer in Downloads brings up a card on the notch. Off, the app does not look at Downloads."
                : "When a disk image holding an app mounts, the notch installs it to Applications and ejects the image, or asks first. Needs no permission."
        case "toolWheelEnabled":
            return context.windows
                ? "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links in programs that report them. On by default."
                : "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links. On by default; needs Accessibility permission."
        case "launcherEnabled":
            return context.windows
                ? "Alt+Space opens a search of your apps, running programs and the app's pages. Off by default."
                : "Search apps, files and the app commands, and calculate. Off by default."
        case "launcherPulseCommands":
            return "Open Storage, Cleanup, Monitor, Apps and Settings\(context.hasClipboard ? ", and clear clipboard history," : "") from the launcher. On by default."
        case "launcherClipboard":
            return "Keeps what you copy on this \(machine): text, links, colours, code, images and files, searchable with “clip” and in the clipboard strip. It works with the launcher on or off. Off until you turn it on. Turning it off keeps the history and reads nothing from the clipboard; Clear clipboard history deletes it."
        case "clipboardHotkey":
            return "The global key that opens the clipboard strip. Needs at least one of \(context.windows ? "Ctrl, Alt or Shift" : "Control, Option, Shift or Command"). Escape cancels. \(context.windows ? "Alt+Shift+V" : "Command Shift V") by default."
        case "clipboardPasteMode":
            return "Paste the chosen clipboard item into the app you were in, or only copy it.\(context.windows ? "" : " Paste needs Accessibility.") Paste by default."
        case "convAutoQuit":
            return context.windows
                ? "Closes the apps below when their last window closes. Only apps you add; an app with a minimized window, or one that keeps running in the tray, is left open."
                : "Quits the apps below when their last window closes. Only apps you add; windows on other Spaces or minimized keep an app open."
        case "windowManagementEnabled":
            return context.windows
                ? "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Off by default; each shortcut can be turned off or changed. On a keyboard with AltGr, a Ctrl+Alt shortcut takes the place of the character AltGr types with that key."
                : "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Off by default; each shortcut can be turned off or changed. Needs Accessibility permission."
        case "nearbyEnabled":
            return "Works with the LocalSend app on iPhone, Android, \(context.windows ? "Mac" : "Windows") and Linux. the app can be found on your network while this is on."
        case "nearbyAlias":
            return "How other devices list this \(machine). Empty uses the \(machine)'s name."
        default:
            break
        }
        if key.hasPrefix("tools.") {
            let id = String(key.dropFirst("tools.".count))
            if let tool = SettingsCatalog.tools.first(where: { $0.id == id }) { return "\(tool.note) On by default." }
        }
        if key.hasPrefix("wheel.") {
            let id = String(key.dropFirst("wheel.".count))
            if let tool = SettingsCatalog.wheel.first(where: { $0.id == id }) {
                let base = context.windows && id == "settings" ? "Open Windows Settings (not the app's settings)." : tool.note
                return "\(base) \(tool.on ? "On" : "Off") by default."
            }
        }
        return nil
    }

    /// Notes that do not change with the platform.
    private static let fixed: [String: String] = [
        // Appearance
        "notifications":
            "Notifications turns off the app's toasts and alert sounds; cards that answer something you did, or that need an answer, still show. On by default. With Show set to Hidden, or the notch folded for a full-screen app, a toast brings the notch out and it goes again.",
        "showsSettingsHandle":
            "The gear at the end of the notch, with its update and permission dots. On by default. Off hides it; Settings stays reachable by clicking a ring, from the hub's sidebar, and with the Open Settings shortcut.",
        // Notifications
        "watchesAgentSessions":
            "Shows running Claude Code and Codex chats on the rings and opens the notch when one finishes. Off by default: nothing is watched, so there is no movement, no announcement and no sound for a finished chat.",
        "announceDriveAlerts": "The notch opens with the message when a drive is filling, full or failing. On by default.",
        "remoteApprovalAlerts": "The notch shows what ChatGPT or another remote tool wants to do while it waits for you, and a click opens General > Remote access. The card goes when the request is answered or expires. On by default.",
        "mute_claude_alerts": "No notch cards for the limits of Claude or any of its extra accounts.",
        "mute_codex_alerts": "No notch cards for the limits of Codex or any of its extra profiles.",
        // General: shortcuts and installers
        "mac_shortcuts": "Left Alt+A, C, V, X, Z, S, F, T and W act as Ctrl+the same letter; Left Alt+Shift+Z redoes.",
        "altRemapSaveFindTabClose":
            "Includes Save, Find, new Tab and Close among the Alt shortcuts. Off leaves those four keys to the app, for programs that use Alt+letter for their own menus. Needs Mac-style editing.",
        "screenshot_shortcuts": "Alt+Shift+4 drags a region; Alt+Shift+5 opens the screenshot toolbar.",
        "screenshot_to_desktop": "Off keeps them on the clipboard only. The Alt+Shift+5 toolbar changes this too.",
        "installer_auto": "Watches Downloads and installs signed MSIX and MSI packages, with Undo. Unsigned ones always ask.",
        "autoUpdateCheck": "Looks for a new release at most every six hours. Nothing installs until you choose Update.",
        "hubDiagnosticsPage":
            "the app's crash and hang reports and warnings, with only times, process names and short codes. On by default.",
        "claudeAccountScan":
            "Looks for new Claude account folders and offers to bring this computer's chats into them. On by default. Mac only: the Windows card is not built.",
        // General: tools
        // General: Claude accounts and background
        "claudeChatMirror":
            "Copies the signed-in Claude account's chats into the other accounts' lists whenever they change. Off by default: press Sync chats on the Claude card after you switch account; it closes Claude, syncs every account and opens Claude again.",
        "claudeChatRemember": "Lets a restart or an account switch reopen the chats that were open. On by default.",
        "hubDriveHealth": "Reads each drive's health every two minutes for alerts and history. On by default.",
        "hubMetricsHistory": "Samples every two seconds and keeps 30 minutes for Monitor's charts. On by default.",
        "hubFileIndex": "Builds a file-name index of each disk for fast search in Storage. On by default; applies the next time the hub starts.",
        "hubFileIndexWholeDisk": "Indexes every mounted volume instead of your home folder, /Applications and the extra volumes. Off by default; applies the next time the hub starts.",
        "hubFileIndexVolumes": "Extra volumes the file index covers as well, one mount path per line (for example /Volumes/Archive). Empty by default.",
        "hubFileIndexExcludes": "Folders the file index skips, one per line: a name skips it wherever it appears; a path such as Library/Caches skips that folder. Default: target, node_modules, .git/objects, DerivedData, .build, build, dist, Library/Caches, Library/Logs, .Trash, .gradle, .cache, __pycache__, venv, .venv, .rightkit-managed, .rustright, .build-home. Windows adds AppData\\Local\\Temp, $Recycle.Bin and System Volume Information.",
        "chromeSnapshotCount": "Once a day, counts the leftover code-signing copies Chrome keeps, for Cleanup's history line. On by default.",
        "mediaCompression": "Adds Compress a copy to a JPEG, PNG, MP4 or MOV file in Storage. It writes a smaller copy in a the app Compressed folder beside the file and never changes the original. Off by default.",
        "hubAutoScan": "Scans by itself when there is no saved result or it is old. On by default. Off shows what was saved until you press Rescan.",
        // General: nearby sharing and Chat
        "chat.enabled":
            "Lets Claude and Codex chats on this computer message chats on linked computers over ssh. Link one with: pulse chat link <device> <ssh-host>",
        "chatRingLight": "The Send ring pulses for a few seconds when a Chat message is sent or arrives. On by default.",
        "chatPluginAutoInstall": "Finds Claude Code, Codex and Muse here and installs the app's Chat integration where it is missing. Muse asks once on a notch card; a Not now is not asked again until this is turned off and on. On by default.",
        "chat.claudeHookDelivery":
            "The message arrives as added context labelled as coming from another agent, after the chat's next tool call, at the end of its turn, or with the user's next prompt. An idle chat reads it only on its next turn. Requires the the app plugin installed in Claude Code. Off by default.",
        "chat.terminalChats":
            "Lists interactive terminal chats (Claude Code and Codex in a terminal) alongside the app chats, but only while they have been active in the last 6 hours, so old terminal sessions do not clutter the list. Off, only app chats are listed. App chats are always listed. On by default.",
        "chat.claudeProfiles":
            "Lists and messages Claude chats in other Claude profiles (~/ClaudeProfiles), labelled by profile. On by default.",
        "chat.muse":
            "Lists, reads and messages Muse Code chats, and lets the app's Muse plugin register them and hand them messages. Off, no Muse chat is listed or reached and the plugin's hooks do nothing. Install the plugin with pulse chat install --muse. On by default.",
        "chat.stampUserWords":
            "Each message a chat sends carries your last three typed turns, read by the app from that chat's transcript, so the receiving chat can check what you asked. Up to 3 KB, only to your own linked computers. On by default.",
        "chat.receiptNotices":
            "On, a chat whose hook runs is told in a short line when a message it sent was seen by the receiving chat, picked up, released, refused or expired. At most one such notice a minute, batched. Off, the receipts are still kept and shown by pulse chat receipts. On by default.",
        "chat.codexWakeIdle":
            "On, an idle Codex chat is started on the message without anyone pressing Steer, using Codex's own command line, with that chat's normal permissions, and it uses the account's Codex allowance. Only a chat that is positively idle is started, and never one the Codex app may hold: then the message waits for the chat's next prompt or tool call. Off, the message waits for the owner's next prompt. On by default. The app may not show the new turn until the chat is reopened.",
        "chat.claudeWakeIdle":
            "On, a message for an idle Claude Desktop chat (default profile) opens that chat in Claude Desktop so it starts a turn, and it uses the account's Claude allowance. A chat in another profile waits for its next turn. Off, the message waits in the chat's inbox. On by default.",
        "chat.museWakeIdle":
            "On, the app starts a turn in an idle Muse chat on the message; takes effect when Muse offers a supported entry. Until then the message waits in the chat's inbox for its next prompt. On by default.",
        // General: launcher and clipboard
        "launcherDictionary": "“define word” looks words up in the Mac's dictionaries.",
        "launcherShortcuts": "Search and run the shortcuts in the Shortcuts app.",
        "launcherCalculator": "Answers sums and conversions such as “12 km in mi”. On by default.",
        "launcherSnippetExpansion":
            "Watches what you type to spot a snippet's keyword and replaces it. Nothing typed is stored or sent. Needs Accessibility. A keyword must start with a symbol (such as ;sig or /addr) and be 2 to 32 characters. It expands only in a text field, never in password fields, in the app's own windows, or while a password manager's item is on the clipboard. Snippets that use {argument} are skipped. Off until you turn it on.",
        "launcherCurrency": "Fetches exchange rates once a day from open.er-api.com and CoinGecko. Off until you turn it on.",
        "clipboardStripEnabled":
            "Allows the hot key, the Tools cell item and the wheel item to open the strip of cards. It works with the launcher on or off. Off: history is still kept, the hot key is released, and the items open the launcher's “clip” when the launcher is on. On by default (only matters once Clipboard history is on).",
        "clipboardCardSize":
            "Small, Medium or Large clipboard cards. The size is multiplied by the notch size scale, clamped to 0.8 to 1.25. Medium by default.",
        "clipboardStripPlacement":
            "Where the strip appears. Notch: out of the notch, on its edge. Bottom: along the bottom of the display under the pointer, with no neck. Notch by default.",
        "clipboardShowNumbers": "Shows the 1 to 9 quick-paste number on clipboard cards. On by default.",
        "clipboardPlainDefault":
            "In the clipboard strip, Enter pastes plain text and Shift+Enter pastes as copied (swaps the two). Off by default.",
        "clipboardWriteBackExcluded":
            "Windows only. When the app pastes a clipboard item it marks it so Windows' own Win+V history does not record it again. On by default.",
        "clipboardCaptureImages": "Stores copied images and screenshots in the clipboard history. Off: image copies are skipped. On by default.",
        "clipboardCaptureFiles": "Stores copied file lists (paths only) in the clipboard history. Off: file copies are skipped. On by default.",
        "clipboardCaptureRich":
            "Stores HTML and RTF with the text in the clipboard history. Off: rich copies are stored as plain text. On by default.",
        "clipboardMaxItems": "Cap on clipboard items in the main lane (50 to 5000). Pinned and board items stay. 500 by default.",
        "clipboardMaxMB": "Cap on the clipboard history's total size in MB (20 to 4096). 200 by default.",
        "clipboardMaxAgeDays":
            "Days since last use before an unpinned clipboard item goes (1 to 365; 0 is forever). 30 by default.",
        "clipboardScreenshotHours": "Hours before a clipboard screenshot goes (1 to 720; 0 is until a size cap). 24 by default.",
        "clipboardSkipAutoGenerated":
            "Ignores clipboard copies that apps mark as made by a program (AutoGeneratedType). Mac only. On by default.",
        "clipboardSkipRemote": "Ignores Universal Clipboard copies from your iPhone or iPad. Mac only. On by default.",
        "clipboardSecretFilter":
            "Ignores a clipboard copy that is only a private key, access key, token or password-like string from a known pattern. On by default.",
        "clipboardExcludedApps":
            "Clipboard copies from these apps are never kept. One app id per line. Password managers by default.",
        "clipboardIgnoredTypes":
            "Extra pasteboard types that make the clipboard history skip a copy. One per line. Mac only. Empty by default.",
        "clipboardShiftPasteApps":
            "Windows only. Terminals where the clipboard strip pastes with Ctrl+Shift+V: app names, one per line. Windows Terminal, WezTerm, Alacritty and kitty by default.",
        "clipboardRememberBoard": "Opens the clipboard strip on the board you left it on. Off by default.",
        // General: keyboard, readings and conveniences
        "convFnCommand": "Fn+C/V/X/A/Z/S/F/T/W; Fn+arrows move by word",
        "gpuTemperature":
            "Shows the NVIDIA graphics card's temperature on the System card. It is read only while that card is open. On by default.",
        "asksProviderOnLook": "Spends a request each time. Useful to check against a provider's own page.",
        "convFinderCutPaste":
            "⌘X (or Fn X) marks the selected items; ⌘V in a Finder window moves them there. Finder asks before replacing anything.",
        "finderPathField":
            "Shows the front Finder window's folder in a small field above it. Type or paste a path and press Return to go there. Needs Accessibility and Finder Automation (macOS asks the first time).",
        "agentRequests":
            "Lets the app's background agent answer `pulse task ask` and voice requests with one bounded run of Claude Code, which uses your Claude subscription quota. Off until you turn it on.",
        "agentActions":
            "Lets the app's background agent act for `pulse task act` and voice requests, using only the the app tools you have switched on under Remote access (files, terminals, screen). Every act waits for your approval in the app; you can cancel a run at any time. Off until you turn it on.",
        "deviceLinkListen":
            "Lets another of your computers on this local network pair and link with this one. Nothing listens while this is off. Each pairing needs a code typed on the other computer and your approval here.",
        "deviceMeshLocal": "Finds your other computers on the local network, in addition to any relay. On by default.",
        "sendDefaultDevice": "Pasting on the Send cell sends the clipboard straight to this paired device (its device id). Empty: with exactly one other computer paired, that one; otherwise Send asks.",
        "sendOverMesh": "Sends files to your other computers over the RightKit mesh as well as nearby sharing. On by default.",
        "agentFileAutoAccept": "Files an agent chat on one of your own paired computers sends a chat here (pulse chat send --file) are saved in <Save folder>/agents/<chat> without asking. Never from a guest, server or unclassified device. At most 20 files and 2 GiB a send and 10 GiB an hour; refused if under 5 GiB or 5% of the disk would stay free. Off: the usual Accept prompt (devices set to Allow or Deny skip it, as with any send). On by default.",
        "deviceLinkRelayUrl": "The relay used when the other computer is not on this network. Empty by default: no relay.",
        "convWindowMaximizer": "Fills the screen without a full-screen Space. Option-click keeps the usual behaviour.",
        "convDockClickMinimize": "Clicking the Dock icon of the frontmost app minimizes its windows.",
        "convDiskImageAuto": "Only signed, notarized apps that aren't installed yet; others ask in the notch.",
        "convDiskImageAutoUpdate": "Replaces an older installed version; Undo restores it from the Trash.",
        "convDiskImageTrashDownload": "After a successful install, the .dmg you opened goes to the Trash. Off by default.",
        "finderMenuCut": "Cut in Finder's right-click menu, for selected items. On by default. Shown while the app runs.",
        "finderMenuCopyPath": "Copy Path in Finder's right-click menu. On by default. Shown while the app runs.",
        "finderMenuTerminal": "Open in Terminal in Finder's right-click menu. On by default. Shown while the app runs.",
        // General: background (hub health and the file-index pause ladder)
        "hubFileIndexAutoPause":
            "When the file index stays unhealthy, the app pauses it for 5 minutes, then 10, then switches it off on the third trip. On by default; two healthy hours reset the count.",
        "hubFileIndexMaxTrips": "Trips before the file index is switched off (1 to 20). 3 by default.",
        "hubFileIndexPauseMinutes": "The first pause, in minutes; each later pause doubles it (1 to 1440). 5 by default.",
        "hubFileIndexMaxCpuPercent": "Hub CPU, in percent of one core, above which the file index counts as unhealthy (1 to 1000). 100 by default.",
        "hubFileIndexMaxFootprintMb": "Hub memory, in MB, above which the file index counts as unhealthy (64 to 65536). 2048 by default.",
        "hubFileIndexMaxWriteMbPerHour": "Disk writes, in MB per hour, above which the file index counts as unhealthy (1 to 100000). 2048 by default.",
        // General: hub health
        "hubWatchdog":
            "Every minute the hub checks its own CPU, memory, threads and disk writes. A sustained overrun raises a hub-health event and an Activity row. It reports and never acts. On by default.",
        "hubDiagReports":
            "Lists new the app crash and resource reports in the macOS diagnostic folders and reports each one once (names and dates only). On by default.",
        "hubLog": "Keeps the hub's lifecycle in its log: starts, stops, services, file-index passes and watchdog trips. No file names or contents. On by default.",
        "hubWatchdogCpuPercent": "CPU budget for the watchdog, in percent of one core (1 to 1000). 5 by default.",
        "hubWatchdogMinutes": "How long an overrun must last before the watchdog reports it (1 to 60 minutes). 10 by default.",
        "hubWatchdogFootprintMb": "Memory budget for the watchdog, in MB (64 to 65536). 1024 by default.",
        "hubWatchdogThreads": "Thread budget for the watchdog (10 to 10000). 200 by default.",
        "hubWatchdogWriteMbPerHour": "Disk write budget for the watchdog, in MB per hour (1 to 100000). 500 by default.",
        // Remote access (the notch publishes these; read by the pulse CLI and the engine)
        "remoteTools":
            "Read-only access to this computer by agents through the pulse CLI (files and chats on this computer only). Off by default; every call is refused while it is off, and each call is recorded in Activity.",
        "remoteRoots":
            "Folders remote file tools may read. A path must sit inside one of them; credential folders are never read, and network paths are never opened unless approved. Empty by default: no folder, so reads fail.",
        "remoteBlockedApps":
            "Apps no remote tool may open, see or act on, listed by bundle id. Add one with Add app… or drop a .app on the list. Empty by default: every other app is in reach. the app itself, System Settings and security prompts are always off limits.",
        "remoteChatRead":
            "Chats `pulse chat read` may return, by chat id (see pulse chat peers). A chat not listed is never opened. Empty by default.",
        "phoneChatReadAll":
            "On by default: your own paired phones may read every chat on this computer. Off: a phone reads only the chats selected under Chats a remote reader may read, and any other chat is refused (chat_read_off). It never applies to a cloud agent (guest), the ChatGPT gateway or agent tools, and Remote tools does not gate it. Removing a phone ends its access. Turning it back on needs your the app PIN.",
        "remoteReadMaxBytes": "The most bytes one `pulse files read` returns (1 byte to 2 MiB). 1 MiB by default.",
        "remoteFindMaxResults": "The most results one `pulse files find` returns (1 to 1000). 200 by default.",
        "remoteChatReadMaxTurns": "The most turns one `pulse chat read` returns (1 to 500). 50 by default.",
        "remoteChatReadMaxBytes": "The most bytes of turn text one `pulse chat read` returns (1 byte to 2 MiB); older turns go first. 256 KiB by default.",
        "remoteImageMaxBytes": "The most bytes of source image the image reader would accept (1 byte to 25 MiB). Stored now; no image reader exists yet. 10 MiB by default.",
        "remoteGateway":
            "Lets ChatGPT reach this computer's read-only remote tools through OpenAI's Secure MCP Tunnel (pulse-gateway). Needs Remote tools as well. Off by default.",
    ]
}
#endif
