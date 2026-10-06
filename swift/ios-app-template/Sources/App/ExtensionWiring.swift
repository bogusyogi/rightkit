import ActivityKit
import Foundation
import RightKitCloudSync
import RightKitImportQueue
import RightKitLiveActivity
import RightKitPurchases
import RightKitWatchBridge

/// One place that shows how each extension target meets its RightKitSwift product. Replace the
/// placeholder handling with product code; keep the shape.
enum ExtensionWiring {
    // Share extension -> app: foreground drain, acknowledge only after the app handled the item.
    static func drainShareQueue() {
        let queue = SharedImportQueue(configuration: TemplateConfig.shareIntake())
        guard let items = try? queue.pending() else { return }
        for item in items {
            // Product code imports `item` here; on failure call queue.markFailed(item.id, error:).
            try? queue.acknowledge(item.id)
        }
    }

    // Widgets extension renders; the app owns the activity through LiveActivityHost.
    static let liveActivity = LiveActivityHost<TemplateActivityAttributes>()

    // StoreKit config (Config/Template.storekit) -> RightKitPurchases.
    @MainActor
    static func makePurchaseController() -> PurchaseController {
        let config = PurchaseConfig(
            products: [ProductDefinition(id: "com.rightsuite.template.pro", entitlements: ["pro"])],
            storageNamespace: "template.purchases",
        )
        return PurchaseController(
            config: config,
            backend: StoreKit2Backend(),
            accountTokens: InMemoryAccountTokenStore(),
        )
    }

    // CloudKit (iCloud container entitlement) -> RightKitCloudSync transport. Pair it with a
    // SyncAdapter and CloudSyncEngine/CloudSyncCoordinator in product code.
    static func makeCloudTransport() -> CloudKitSyncTransport {
        CloudKitSyncTransport(
            containerIdentifier: TemplateConfig.iCloudContainerID,
            zoneName: "TemplateZone",
            subscriptionID: "template-zone-changes",
        )
    }

    // Watch app -> RightKitWatchBridge (receiving side lives in Sources/WatchApp).
    static let watchKeys = WatchBridgeKeys()

    static var status: String {
        "share queue, live activity, purchases, cloud, watch wired"
    }
}
