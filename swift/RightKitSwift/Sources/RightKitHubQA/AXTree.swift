#if os(macOS)
// The accessibility read of the SwiftUI hub: the whole tree of this process through
// RightKitAXElement, and from it the controls a person could use (button, checkbox, radio, switch, text
// field, link, popup, slider). QA only; AppKit host code, outside the view lint.
//
// The in-process AX calls run on the caller's thread, so they are made on the main actor, directly,
// between awaits (see AXClient). The hosting-view and window walks were tried on CI and found nothing.

import AppKit
import ApplicationServices
import RightKitAccessibility

/// One actionable element of the accessibility tree.
@MainActor
final class AXControl {
    /// The AX role as the system names it (AXButton, AXCheckBox, ...).
    let rawRole: String
    /// The role normalised to the Tauri inventory's words: button, checkbox, radio, switch,
    /// textbox, link, select, slider.
    let role: String
    let label: String
    let value: String
    let enabled: Bool
    let frame: CGRect
    /// Child indices from the application element; a press resolves the path again.
    let axPath: [Int]
    /// True for the sidebar row of a section (a button in the sidebar column named like a section).
    var isNavRow = false

    init(rawRole: String, role: String, label: String, value: String, enabled: Bool, frame: CGRect, axPath: [Int]) {
        self.rawRole = rawRole
        self.role = role
        self.label = label
        self.value = value
        self.enabled = enabled
        self.frame = frame
        self.axPath = axPath
    }
}

/// What one walk of the tree found.
@MainActor
struct AXSnapshot {
    var controls: [AXControl] = []
    /// Every static text, in tree order.
    var texts: [String] = []
    /// Every non-empty label and value of any node, lowercased (for the loading check).
    var labels: Set<String> = []
    /// One line per node, indented by depth, for the diagnostic dump.
    var dump: [String] = []
    var nodes = 0
    /// What the walk reported: trust, the first AXError, whether the budget ended it.
    var note = ""

    /// True while a `HubLoadingState` is on screen: its message ("Reading…" in Monitor, "Starting the
    /// engine" in the shell, "Loading" by default) or its hourglass symbol is in the tree.
    var isLoading: Bool {
        let messages: Set<String> = ["reading…", "reading...", "starting the engine", "loading", "loading…", "loading...", "hourglass"]
        return !labels.isDisjoint(with: messages)
    }
}

/// What a walk needs to know about the hosted hub.
@MainActor
struct AXContext {
    let window: NSWindow
    let sidebarWidth: CGFloat
    let navTitles: Set<String>
}

@MainActor
enum AXWalker {
    /// One walk of this process's accessibility tree, as an AXUIElement client sees it.
    static func read(_ context: AXContext) -> AXSnapshot {
        let trusted = RightKitAXTrust.isTrusted()
        let walk = AXClient.walk(pid: getpid(), maxNodes: 8000, seconds: 15)
        var result = AXSnapshot()
        let nodes = walk.nodes
        for (index, node) in nodes.enumerated() {
            result.nodes += 1
            if node.role == "AXStaticText" {
                let text = node.value.isEmpty ? node.label : node.value
                if !text.isEmpty { result.texts.append(text) }
            }
            for text in [node.label, node.value] where !text.isEmpty {
                result.labels.insert(text.lowercased())
            }
            if result.dump.count < 6000 {
                let tail = node.subrole.isEmpty ? "" : "/\(node.subrole)"
                result.dump.append("\(String(repeating: "  ", count: node.depth))\(node.role)\(tail) \"\(node.label)\" = \"\(node.value.prefix(80))\"")
            }
            guard let normalized = normalizedRole(raw: node.role, subrole: node.subrole) else { continue }
            var name = node.label
            if name.isEmpty {
                var below: [String] = []
                var next = index + 1
                while next < nodes.count, nodes[next].depth > node.depth {
                    let inner = nodes[next]
                    if inner.role == "AXStaticText" {
                        let text = inner.value.isEmpty ? inner.label : inner.value
                        if !text.isEmpty { below.append(text) }
                    }
                    next += 1
                }
                name = below.joined(separator: " ")
            }
            result.controls.append(
                AXControl(
                    rawRole: node.role, role: normalized, label: name, value: node.value,
                    enabled: node.enabled, frame: node.frame, axPath: node.path
                )
            )
        }
        markNavRows(&result, context)
        result.note = "AXIsProcessTrusted=\(trusted); app element children \(walk.appChildren); root AXError \(walk.rootError); \(walk.nodes.count) nodes\(walk.timedOut ? "; walk budget reached" : "")"
        return result
    }

    /// A sidebar row is a button in the sidebar column named like a section. Only the first such
    /// button of each name counts (the rows come before the footer in tree order): the footer's
    /// "Appearance" button shares its name with the Appearance settings row and is a control of
    /// every section, as the Tauri hub's footer button is. If the tree reports no frames at all,
    /// the first button of each name stands in.
    private static func markNavRows(_ result: inout AXSnapshot, _ context: AXContext) {
        let windowMinX = context.window.frame.minX
        var seen = Set<String>()
        for control in result.controls where control.role == "button" {
            let key = Match.normalize(control.label)
            guard context.navTitles.contains(key) else { continue }
            let inColumn = control.frame == .zero || control.frame.midX - windowMinX < context.sidebarWidth
            if inColumn, seen.insert(key).inserted { control.isNavRow = true }
        }
    }

    /// The Tauri inventory's role word for an AX role, or nil when the node is not a control.
    static func normalizedRole(raw: String, subrole: String) -> String? {
        switch raw {
        case "AXButton", "AXDisclosureTriangle": return subrole == "AXSwitch" ? "switch" : "button"
        case "AXCheckBox": return subrole == "AXSwitch" ? "switch" : "checkbox"
        case "AXSwitch": return "switch"
        case "AXRadioButton": return "radio"
        case "AXTextField", "AXTextArea", "AXComboBox", "AXSearchField": return "textbox"
        case "AXLink": return "link"
        case "AXPopUpButton", "AXMenuButton": return "select"
        case "AXSlider": return "slider"
        default: return nil
        }
    }
}
#endif
