import AppKit
import Foundation

// rk-fixture-window: a tiny AppKit window that sits fully outside every display, for live tests of
// the rightkit-record macOS backend. It is never shown on a screen: it is created with alpha 0 at a
// frame disjoint from every display, ordered front, and made visible only after reading back that
// the window server kept it off every display. If it did not, the window is ordered out at once and
// the process exits with status 3. It prints one JSON line with its pid and window id, and exits by
// itself after --seconds (default 120) so a test that dies cannot leave it running.
//
// Usage: rk-fixture-window [--seconds N] [--size WxH] [--title T]

for sig in [SIGSEGV, SIGBUS, SIGABRT, SIGILL, SIGTRAP, SIGFPE] { signal(sig) { s in _exit(128 + s) } }

func argument(_ name: String) -> String? {
    let args = CommandLine.arguments
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}

let lifetime = Double(argument("--seconds") ?? "120") ?? 120
let sizeParts = (argument("--size") ?? "480x270").split(separator: "x").compactMap { Double($0) }
let size = NSSize(width: sizeParts.first ?? 480, height: sizeParts.count > 1 ? sizeParts[1] : 270)
let title = argument("--title") ?? "rk-fixture-window"

func emitLine(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else { return }
    FileHandle.standardOutput.write(data + Data("\n".utf8))
}

/// A borderless window that AppKit never pulls back onto a display.
final class OffscreenWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Animated content: a frame counter, a moving bar, and a 24-cell bit strip of the counter, so a
/// decoded video shows motion in every frame.
final class FixtureView: NSView {
    var tick: UInt64 = 0
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedRed: 0.06, green: 0.08, blue: 0.14, alpha: 1).setFill()
        bounds.fill()
        let barX = CGFloat(tick % 240) / 240 * (bounds.width - 40)
        NSColor.systemOrange.setFill()
        NSRect(x: barX, y: bounds.height - 60, width: 40, height: 24).fill()
        for bit in 0..<24 {
            ((tick >> UInt64(23 - bit)) & 1 == 1 ? NSColor.white : NSColor(white: 0.15, alpha: 1)).setFill()
            NSRect(x: 8 + CGFloat(bit) * 8, y: 8, width: 8, height: 8).fill()
        }
        let text = "FRAME \(tick)" as NSString
        text.draw(at: NSPoint(x: 12, y: 40), withAttributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 34, weight: .bold), .foregroundColor: NSColor.white,
        ])
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)  // no Dock icon, never activated

// Frame far to the right of every display. AppKit y is bottom-left based; it does not matter here.
let screens = NSScreen.screens
let farX = (screens.map { $0.frame.maxX }.max() ?? 0) + 4000
let frame = NSRect(x: farX, y: 0, width: size.width, height: size.height)
if screens.contains(where: { $0.frame.intersects(frame) }) {
    emitLine(["ok": false, "error": "intended frame intersects a display"])
    exit(3)
}

// Keep App Nap away: a non-frontmost app with no visible window is napped within about 30 s, which
// cuts display-link callbacks (RK2 Spike A). Hold the activity for the life of the process.
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.userInitiatedAllowingIdleSystemSleep, .latencyCritical], reason: "rightkit-record fixture")

let window = OffscreenWindow(contentRect: frame, styleMask: [.borderless], backing: .buffered, defer: false)
window.title = title
window.isReleasedWhenClosed = false
window.hidesOnDeactivate = false
window.isOpaque = true
window.hasShadow = false
let view = FixtureView(frame: NSRect(origin: .zero, size: size))
window.contentView = view
window.alphaValue = 0  // invisible until the position is verified
window.setFrame(frame, display: false)
window.orderFrontRegardless()

let placed = window.frame
if screens.contains(where: { $0.frame.intersects(placed) }) || NSScreen.screens.contains(where: { $0.frame.intersects(window.frame) }) {
    window.orderOut(nil)
    emitLine(["ok": false, "error": "the window server placed the window on a display", "x": placed.minX])
    exit(3)
}
window.alphaValue = 1

emitLine([
    "ok": true, "pid": ProcessInfo.processInfo.processIdentifier, "windowId": window.windowNumber,
    "frame": ["x": placed.minX, "y": placed.minY, "width": placed.width, "height": placed.height],
    "screenIsNil": window.screen == nil,
])

Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { _ in
    view.tick += 1
    view.setNeedsDisplay(view.bounds)
    view.displayIfNeeded()
}
Timer.scheduledTimer(withTimeInterval: lifetime, repeats: false) { _ in
    ProcessInfo.processInfo.endActivity(activity)
    exit(0)
}
signal(SIGTERM) { _ in _exit(0) }
app.run()
