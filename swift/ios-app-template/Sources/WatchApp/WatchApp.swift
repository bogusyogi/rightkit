import RightKitWatchBridge
import SwiftUI
import WatchConnectivity

@main
struct TemplateWatchApp: App {
    @WKApplicationDelegateAdaptor(WatchDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup { Text("Template").accessibilityIdentifier("watch.title") }
    }
}

/// Skeleton WCSession delegate. Delivery bookkeeping belongs in RightKitWatchBridge's
/// TransferLedger; send through DualPathSender over WCSessionTransport.
final class WatchDelegate: NSObject, WKApplicationDelegate, WCSessionDelegate {
    let keys = WatchBridgeKeys()

    func applicationDidFinishLaunching() {
        guard WCSession.isSupported() else { return }
        WCSession.default.delegate = self
        WCSession.default.activate()
    }

    func session(_ session: WCSession, activationDidCompleteWith state: WCSessionActivationState, error: Error?) {}

    func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        // Product code handles `keys.acknowledgement` / `keys.rollback` replies here.
    }
}
