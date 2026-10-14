import AppKit
import Foundation
import RecordWire

// rightkit-record-macos-helper: the out-of-process macOS capture helper for rightkit-record.
// Protocol: length-prefixed JSON frames on stdin/stdout (crates/rightkit-record/src/protocol.rs).
// Nothing else may write to stdout; diagnostics go to stderr.

suppressCrashDialogs()
_ = NSApplication.shared  // connects to the window server for ScreenCaptureKit; never activated, no UI

let server = Server()

// SIGTERM (rightkit-child-process termination): finalize the take, then exit.
signal(SIGTERM, SIG_IGN)
let terminate = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
terminate.setEventHandler { server.abortAndExit(code: 0) }
terminate.resume()

Thread.detachNewThread { server.run() }
RunLoop.main.run()
