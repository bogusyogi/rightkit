#if canImport(StoreKitTest)
import Foundation
import StoreKit
import StoreKitTest
import Testing
import RightKitPurchases

// Real StoreKit, served by a StoreKitTest session from `StoreKit/RightKitPurchasesTests.storekit`.
//
// Runs headless on an iOS simulator through the package's test-only scheme (README, "Real
// StoreKit tests"): `xcodebuild test -scheme RightKitPurchasesStoreKitTests -destination
// 'platform=iOS Simulator,...'`. No host app and no scheme-level StoreKit configuration are
// needed; the programmatic `SKTestSession` is enough.
//
// Two traps, both measured:
// - The configuration must list non-consumables (and consumables) under `"products"` with a
//   `"type"`, as Xcode writes it. A top-level `"nonConsumables"` array is accepted by
//   `SKTestSession` but ignored by the local catalog: every request returns 0 products. That,
//   not the headless simulator, is what produced "0 products" before.
// - macOS test processes get no catalog: `swift test` runs inside `swiftpm-testing-helper`,
//   which has no bundle identifier (StoreKit throws `StoreKitInternalError.invalidApp`), and
//   `xcodebuild test -destination platform=macOS` returns 0 products. The suite is therefore
//   enabled only on iOS (reported as skipped elsewhere, never as a pass).

private let productID = "org.rightkit.test.pro"

/// The local StoreKitTest catalog serves the iOS-simulator `xctest` agent (or an app host),
/// never a macOS test process.
private var storeKitTestCatalogAvailable: Bool {
    #if os(iOS)
    Bundle.main.bundleIdentifier != nil
    #else
    false
    #endif
}

private func configurationURL() throws -> URL {
    try #require(Bundle.module.url(forResource: "RightKitPurchasesTests", withExtension: "storekit"))
}

@Suite(.serialized, .enabled(if: storeKitTestCatalogAvailable, "StoreKitTest serves no catalog to macOS test processes; run the RightKitPurchasesStoreKitTests scheme on an iOS simulator (README)"))
struct StoreKitTestSessionTests {
    private func session() throws -> SKTestSession {
        let session = try SKTestSession(contentsOf: try configurationURL())
        session.disableDialogs = true
        session.resetToDefaultState()
        session.clearTransactions()
        return session
    }

    @Test func storeKit2BackendLoadsProductsFromTheLocalConfiguration() async throws {
        let session = try session()
        defer { session.clearTransactions() }
        let backend = StoreKit2Backend()
        let products = try await backend.loadProducts(ids: [productID])
        #expect(products.map(\.id) == [productID])
        #expect(products.first?.displayName == "RightKit Test Pro")
        #expect(products.first?.kind == .nonConsumable)
        #expect(products.first?.displayPrice.isEmpty == false)
    }

    @Test @MainActor func purchaseControllerLoadsProductsThroughRealStoreKit() async throws {
        let session = try session()
        defer { session.clearTransactions() }
        let config = PurchaseConfig(
            products: [ProductDefinition(id: productID, entitlements: ["pro"])],
            storageNamespace: "rightkit.storekittest.\(UUID().uuidString)"
        )
        let controller = PurchaseController(
            config: config, backend: StoreKit2Backend(), store: InMemoryStore(),
            accountTokens: InMemoryAccountTokenStore()
        )
        await controller.retry()
        #expect(controller.loadState == .loaded)
        #expect(controller.product(productID)?.displayName == "RightKit Test Pro")
        #expect(controller.isResolved && !controller.hasAccess, "a fresh session owns nothing")
    }
}
#endif
