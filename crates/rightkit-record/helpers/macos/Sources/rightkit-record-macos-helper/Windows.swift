import AppKit
import Foundation
import RecordWire
import ScreenCaptureKit

enum Windows {
    /// Every window ScreenCaptureKit knows, including ones fully off every display
    /// (`onScreenWindowsOnly: false`).
    static func shareable() throws -> [SCWindow] {
        let semaphore = DispatchSemaphore(value: 0)
        var windows: [SCWindow] = []
        var failure: Error?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, error in
            windows = content?.windows ?? []
            failure = error
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + 10) != .success {
            throw HelperError.helper("window discovery timed out")
        }
        if let failure {
            let ns = failure as NSError
            // -3801: the user declined (or never granted) screen recording for this identity.
            if ns.domain == SCStreamErrorDomain && ns.code == SCStreamError.userDeclined.rawValue {
                throw HelperError.permissionDenied("screen recording is not granted to this helper identity")
            }
            throw HelperError.helper("window discovery failed: \(ns.localizedDescription)")
        }
        return windows
    }

    /// The window addressed by (pid, window id). A pid of 0 means "do not check the owner".
    static func find(windowId: UInt64, pid: UInt32) throws -> SCWindow {
        guard windowId > 0, windowId <= UInt64(UInt32.max) else {
            throw HelperError.argInvalid("window id \(windowId) is not a CGWindowID")
        }
        let windows = try shareable()
        guard let window = windows.first(where: { UInt64($0.windowID) == windowId }) else {
            throw HelperError.notFound("no window \(windowId) is known to ScreenCaptureKit")
        }
        if pid != 0, let owner = window.owningApplication?.processID, UInt32(owner) != pid {
            throw HelperError.notFound("window \(windowId) belongs to pid \(owner), not pid \(pid)")
        }
        return window
    }

    /// The visibility the take would run in. Hidden and minimized windows deliver no frames, so the
    /// recorder refuses them rather than degrade.
    static func visibility(of window: SCWindow) -> Visibility {
        if !window.isOnScreen {
            let pid = window.owningApplication?.processID ?? 0
            return NSRunningApplication(processIdentifier: pid)?.isHidden == true ? .appHidden : .minimized
        }
        return isOffEveryDisplay(window.frame) ? .offScreenVisible : .onScreenVisible
    }

    static func sources() throws -> [SourceInfo] {
        try shareable().compactMap { window in
            guard window.windowLayer == 0, let app = window.owningApplication,
                  window.frame.width > 0, window.frame.height > 0 else { return nil }
            return SourceInfo(
                windowId: UInt64(window.windowID), pid: UInt32(app.processID), appId: app.bundleIdentifier,
                title: window.title ?? "", boundsPt: rect(window.frame), visibility: visibility(of: window))
        }
    }

    static func rect(_ r: CGRect) -> Rect {
        Rect(x: Double(r.minX), y: Double(r.minY), width: Double(r.width), height: Double(r.height))
    }
}
