import AppKit
import CoreGraphics
import Foundation
import RecordWire
import Security

let helperIdentity = "rightkit-record-macos-helper"

func uptimeNs() -> UInt64 { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }

func log(_ message: String) {
    FileHandle.standardError.write(Data(("rightkit-record-macos-helper: " + message + "\n").utf8))
}

/// "27.0.1 (26A434)": the product version and the kernel's build string.
func osBuild() -> String {
    let v = ProcessInfo.processInfo.operatingSystemVersion
    var size = 0
    sysctlbyname("kern.osversion", nil, &size, nil, 0)
    var buffer = [CChar](repeating: 0, count: max(size, 1))
    sysctlbyname("kern.osversion", &buffer, &size, nil, 0)
    let build = String(cString: buffer)
    return "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)" + (build.isEmpty ? "" : " (\(build))")
}

/// True when this binary carries a valid, non-ad-hoc signature with a team identifier.
func helperIsSigned() -> Bool {
    var code: SecCode?
    guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return false }
    var staticCode: SecStaticCode?
    guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return false }
    guard SecStaticCodeCheckValidity(staticCode, [], nil) == errSecSuccess else { return false }
    var info: CFDictionary?
    guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
          let dict = info as? [String: Any] else { return false }
    let flags = (dict[kSecCodeInfoFlags as String] as? UInt32) ?? 0
    let adhoc = flags & 0x2 != 0  // kSecCodeSignatureAdhoc
    return !adhoc && dict[kSecCodeInfoTeamIdentifier as String] != nil
}

/// Screen-recording permission of THIS identity, read without prompting. The helper never
/// prompts: it refuses with `permission_denied` before writing anything. A test can force the
/// refusal with RIGHTKIT_RECORD_TEST_PERMISSION=denied; the variable can only withhold access.
func screenAccessGranted() -> Bool {
    if ProcessInfo.processInfo.environment["RIGHTKIT_RECORD_TEST_PERMISSION"] == "denied" { return false }
    return CGPreflightScreenCaptureAccess()
}

/// A fatal signal in a helper on a shared desktop ends the process quietly, so ReportCrash raises
/// no "quit unexpectedly" dialog. The core sees the closed pipe and reports `helper_lost`.
func suppressCrashDialogs() {
    for sig in [SIGSEGV, SIGBUS, SIGABRT, SIGILL, SIGTRAP, SIGFPE] {
        signal(sig) { s in _exit(128 + s) }
    }
    signal(SIGPIPE, SIG_IGN)
}

/// Bounds of every active display in the global top-left coordinate space (the space SCWindow
/// and CGWindowList frames use).
func displayBounds() -> [CGRect] {
    var count: UInt32 = 0
    CGGetActiveDisplayList(0, nil, &count)
    var ids = [CGDirectDisplayID](repeating: 0, count: Int(max(count, 1)))
    CGGetActiveDisplayList(count, &ids, &count)
    return ids.prefix(Int(count)).map { CGDisplayBounds($0) }
}

func isOffEveryDisplay(_ frame: CGRect) -> Bool {
    !displayBounds().contains { $0.intersects(frame) }
}

enum WindowProbe {
    case gone
    case alive(onScreen: Bool, bounds: CGRect)
}

/// A cheap liveness probe of one window by CGWindowID (no ScreenCaptureKit round trip).
func probeWindow(_ id: UInt32) -> WindowProbe {
    guard let list = CGWindowListCopyWindowInfo([.optionIncludingWindow], CGWindowID(id)) as? [[String: Any]],
          let info = list.first(where: { ($0[kCGWindowNumber as String] as? UInt32) == id }) else { return .gone }
    let onScreen = (info[kCGWindowIsOnscreen as String] as? Bool) ?? false
    var bounds = CGRect.zero
    if let dict = info[kCGWindowBounds as String] as? NSDictionary, let rect = CGRect(dictionaryRepresentation: dict as CFDictionary) {
        bounds = rect
    }
    return .alive(onScreen: onScreen, bounds: bounds)
}

/// Why a window that is not on screen delivers no frames: its app is hidden, or the window is
/// minimized (or otherwise ordered out). ScreenCaptureKit reports `isOnScreen: false` for both.
func notOnScreenFailure(pid: pid_t) -> Failure {
    NSRunningApplication(processIdentifier: pid)?.isHidden == true ? .windowHidden : .windowMinimized
}
