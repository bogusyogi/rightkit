#if os(macOS)
// The accessibility read: this process inspects itself the way an assistive client does, through
// RightKitAXElement. QA only; AppKit host code, outside the view lint.
//
// In-process AX calls run directly on the CALLER's thread. Call everything here from the main actor,
// synchronously, between awaits: a SwiftUI Button's action asserts it runs on the main actor, so a
// press from a background thread crashes. Messaging timeouts stay short in case a call does go through IPC.

import AppKit
import ApplicationServices
import RightKitAccessibility

enum AXClient {
    struct Node: Sendable {
        var depth: Int
        var role: String
        var subrole: String
        var label: String
        var value: String
        var enabled: Bool
        var frame: CGRect
        /// Child indices from the application element.
        var path: [Int]
    }

    struct Walk: Sendable {
        var nodes: [Node] = []
        /// The AXError of the first read of the application element's children (0 is success).
        var rootError: Int32 = 0
        var appChildren = 0
        var timedOut = false
    }

    // MARK: Reads (attribute names are the AX constants' string values)

    private static func string(_ element: RightKitAXElement, _ attribute: String) -> String {
        guard let raw = try? element.value(attribute) else { return "" }
        if let text = raw as? String { return text }
        if let number = raw as? NSNumber { return number.stringValue }
        return ""
    }

    /// The AXError raw value of a RightKit failure (0 is success), for the diagnostic text.
    private static func code(_ failure: RightKitAXFailure) -> Int32 {
        switch failure {
        case .failure: return -25200
        case .illegalArgument: return -25201
        case .invalidElement: return -25202
        case .invalidObserver: return -25203
        case .cannotComplete: return -25204
        case .attributeUnsupported: return -25205
        case .actionUnsupported: return -25206
        case .notificationUnsupported: return -25207
        case .notImplemented: return -25208
        case .notificationAlreadyRegistered: return -25209
        case .notificationNotRegistered: return -25210
        case .apiDisabled: return -25211
        case .noValue: return -25212
        case .other(let raw): return raw
        }
    }

    private static func children(_ element: RightKitAXElement) -> (error: Int32, children: [RightKitAXElement]) {
        do {
            let raw = try element.value("AXChildren")
            return (0, ((raw as? [AXUIElement]) ?? []).map(RightKitAXElement.init))
        } catch {
            return (code(error), [])
        }
    }

    private static func application(_ pid: pid_t) -> RightKitAXElement {
        let app = RightKitAXElement.application(pid: pid)
        app.setMessagingTimeout(2)
        return app
    }

    // MARK: Walk

    /// Walks the whole tree of process `pid`, pre-order. Call on the main actor.
    static func walk(pid: pid_t, maxNodes: Int, seconds: TimeInterval) -> Walk {
        let app = application(pid)
        // The modes some frameworks wait for before they build a tree. Best effort.
        for attribute in ["AXEnhancedUserInterface", "AXManualAccessibility"] {
            try? app.set(attribute, to: kCFBooleanTrue)
        }
        var walk = Walk()
        let deadline = Date().addingTimeInterval(seconds)

        func visit(_ element: RightKitAXElement, depth: Int, path: [Int]) {
            guard depth < 60 else { return }
            if walk.nodes.count >= maxNodes || Date() >= deadline {
                walk.timedOut = true
                return
            }
            var label = string(element, "AXDescription")
            if label.isEmpty { label = string(element, "AXTitle") }
            let enabled = element.bool("AXEnabled") ?? true
            walk.nodes.append(
                Node(
                    depth: depth, role: string(element, "AXRole"), subrole: string(element, "AXSubrole"),
                    label: label, value: string(element, "AXValue"), enabled: enabled,
                    frame: CGRect(origin: element.position ?? .zero, size: element.size ?? .zero), path: path
                )
            )
            let found = children(element)
            if depth == 0 {
                walk.rootError = found.error
                walk.appChildren = found.children.count
            }
            for (index, child) in found.children.enumerated() {
                visit(child, depth: depth + 1, path: path + [index])
            }
        }

        visit(app, depth: 0, path: [])
        return walk
    }

    // MARK: Actions (call on the main actor)

    private static func resolve(_ app: RightKitAXElement, path: [Int]) -> RightKitAXElement? {
        var element = app
        for index in path {
            let found = children(element).children
            guard index < found.count else { return nil }
            element = found[index]
        }
        return element
    }

    /// AXPress on the element at `path`. Returns the AXError raw value, or -1 when the path no longer resolves.
    static func press(pid: pid_t, path: [Int]) -> Int32 {
        guard let element = resolve(application(pid), path: path) else { return -1 }
        do { try element.perform("AXPress"); return 0 } catch { return code(error) }
    }

    /// Focuses the element at `path`. Returns the AXError raw value, or -1 when the path no longer resolves.
    static func focus(pid: pid_t, path: [Int]) -> Int32 {
        guard let element = resolve(application(pid), path: path) else { return -1 }
        do { try element.set("AXFocused", to: kCFBooleanTrue); return 0 } catch { return code(error) }
    }
}
#endif
