import Foundation
import CoreGraphics
import ScreenCaptureKit
// Non-prompting TCC probe: CGPreflightScreenCaptureAccess never prompts.
print("preflight_screen_capture=\(CGPreflightScreenCaptureAccess())")
let sem = DispatchSemaphore(value: 0)
SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { c, e in
    if let e { print("sck_error=\((e as NSError).domain) \((e as NSError).code)") }
    else { print("sck_windows=\(c?.windows.count ?? -1) displays=\(c?.displays.count ?? -1)") }
    sem.signal()
}
_ = sem.wait(timeout: .now() + 10)
