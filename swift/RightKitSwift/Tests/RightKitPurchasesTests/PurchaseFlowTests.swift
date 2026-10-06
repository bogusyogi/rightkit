import Foundation
import Testing

@testable import RightKitLicense
@testable import RightKitPurchases

// A two-tier catalogue shaped like a real one: a base purchase, an upgrade that only makes sense on
// top of it (and must carry the base purchase's account token), a standalone full purchase, and a
// subscription. Nothing here is StoreKit — the fake backend stands in for the store.
private enum P {
    static let base = "pro.base"
    static let upgrade = "pro.upgrade"
    static let full = "pro.full"
    static let sub = "pro.sub"
}

private func makeConfig() -> PurchaseConfig {
    PurchaseConfig(
        products: [
            ProductDefinition(id: P.base, entitlements: ["pro", "mobile"], accountToken: .stored),
            ProductDefinition(id: P.upgrade, entitlements: ["full"], accountToken: .inherited(from: P.base)),
            ProductDefinition(id: P.full, entitlements: ["pro", "mobile", "full"]),
            ProductDefinition(id: P.sub, entitlements: ["pro"]),
        ],
        // The upgrade alone unlocks nothing: it is deliberately absent from access precedence.
        accessPrecedence: [P.full, P.base, P.sub],
        proofRules: [
            ProofRule(primary: P.full),
            ProofRule(primary: P.upgrade, companion: P.base),
            ProofRule(primary: P.base),
        ],
        storageNamespace: "test.purchases",
    )
}

private final class FakeBackend: PurchaseBackend, @unchecked Sendable {
    private let lock = NSLock()
    private var entitlements: [PurchaseTransaction] = []
    private var nextOutcome: PurchaseOutcome = .cancelled
    private(set) var finished: [UInt64] = []
    private(set) var purchaseTokens: [UUID?] = []
    private var continuation: AsyncStream<PurchaseTransaction>.Continuation?
    private lazy var stream = AsyncStream<PurchaseTransaction> { self.continuation = $0 }

    func setEntitlements(_ list: [PurchaseTransaction]) { lock.withLock { entitlements = list } }
    func setOutcome(_ outcome: PurchaseOutcome) { lock.withLock { nextOutcome = outcome } }
    func deliver(_ tx: PurchaseTransaction) { _ = stream; continuation?.yield(tx) }

    func loadProducts(ids: [String]) async throws -> [ProductInfo] {
        ids.reversed().map { ProductInfo(id: $0, displayName: $0, displayPrice: "$1", description: "", kind: .nonConsumable) }
    }
    func purchase(productID: String, accountToken: UUID?) async throws -> PurchaseOutcome {
        lock.withLock { purchaseTokens.append(accountToken); return nextOutcome }
    }
    func currentEntitlements() async -> [PurchaseTransaction] { lock.withLock { entitlements } }
    func updates() -> AsyncStream<PurchaseTransaction> { stream }
    func sync() async throws {}
    func finish(_ transaction: PurchaseTransaction) async { lock.withLock { finished.append(transaction.id) } }
    func presentOfferCodeRedemption() async throws { throw PurchaseBackendError.unsupportedPlatform }
    func requestRefund(for transaction: PurchaseTransaction) async throws -> RefundOutcome { .unsupported }
}

private func tx(_ id: UInt64, _ product: String, token: UUID? = nil, revoked: Bool = false,
                expires: Date? = nil) -> PurchaseTransaction {
    PurchaseTransaction(
        id: id, productID: product, purchaseDate: Date(timeIntervalSince1970: 1_000),
        revocationDate: revoked ? Date(timeIntervalSince1970: 2_000) : nil, expirationDate: expires,
        appAccountToken: token, signedJWS: "jws.\(product).\(id)",
    )
}

@MainActor
private func controller(
    _ backend: FakeBackend, store: InMemoryStore = InMemoryStore(),
    tokens: InMemoryAccountTokenStore = InMemoryAccountTokenStore(),
    now: @escaping @Sendable () -> Date = { Date(timeIntervalSince1970: 5_000) },
) -> PurchaseController {
    PurchaseController(config: makeConfig(), backend: backend, store: store, accountTokens: tokens, now: now)
}

/// Lets the listener task drain what the test just delivered.
@MainActor private func settle(_ condition: @MainActor () -> Bool) async {
    for _ in 0..<200 where !condition() { try? await Task.sleep(nanoseconds: 5_000_000) }
}

@Test @MainActor func refundRevokesAccessEverywhereAndCannotBeReplayed() async {
    let backend = FakeBackend()
    backend.setEntitlements([tx(1, P.base)])
    let store = InMemoryStore()
    let purchases = controller(backend, store: store)
    await purchases.configure()
    #expect(purchases.hasAccess)
    #expect(purchases.snapshot.hasEntitlement("pro"))

    backend.deliver(tx(1, P.base, revoked: true))
    await settle { !purchases.hasAccess }
    #expect(!purchases.hasAccess)
    #expect(purchases.snapshot.proof == nil)
    #expect(AccessCacheStore(store: store, namespace: "test.purchases").load().productID == nil)

    // A stale re-delivery of the original, un-revoked transaction must not resurrect the purchase.
    backend.deliver(tx(1, P.base))
    await settle { purchases.hasAccess }
    #expect(!purchases.hasAccess)
    #expect(backend.finished.contains(1))
}

@Test @MainActor func upgradeNeedsBasePurchaseAndCarriesItsTokenAndProof() async {
    let backend = FakeBackend()
    let tokens = InMemoryAccountTokenStore()
    let purchases = controller(backend, tokens: tokens)
    await purchases.configure()

    // Upgrade is refused until the base purchase is owned: nothing reaches the store.
    await purchases.purchase(productID: P.upgrade)
    #expect(backend.purchaseTokens.isEmpty)
    #expect(purchases.message == PurchaseMessages().inheritedTokenMissing)

    // Base purchase uses (and persists) a stored account token.
    backend.setOutcome(.purchased(tx(10, P.base, token: nil)))
    await purchases.purchase(productID: P.base)
    let baseToken = try? #require(backend.purchaseTokens.first ?? nil)
    #expect(baseToken != nil)
    #expect(tokens.load() == baseToken)

    // Store echoes the token on the owned base transaction; the upgrade then inherits that exact token.
    backend.setEntitlements([tx(10, P.base, token: baseToken)])
    await purchases.refreshEntitlements()
    backend.setOutcome(.purchased(tx(11, P.upgrade, token: baseToken)))
    await purchases.purchase(productID: P.upgrade)
    #expect(backend.purchaseTokens.last == baseToken)

    let snapshot = purchases.snapshot
    #expect(snapshot.hasEntitlement("full") && snapshot.hasEntitlement("mobile"))
    #expect(snapshot.activeProductID == P.base)
    #expect(snapshot.proof == PurchaseProof(signedTransaction: "jws.pro.upgrade.11", companionSignedTransaction: "jws.pro.base.10"))
    #expect(backend.finished == [10, 11])
}

@Test @MainActor func coldLaunchReportsAccessFromHintUntilStoreSaysOtherwise() async {
    let backend = FakeBackend()
    let store = InMemoryStore()
    backend.setEntitlements([tx(1, P.full)])
    let first = controller(backend, store: store)
    await first.configure()
    #expect(first.activeProductID == P.full)

    // Next launch: the purchase was refunded while the app was closed.
    backend.setEntitlements([])
    let second = controller(backend, store: store)
    #expect(second.hasAccess && !second.isResolved) // hint, before StoreKit answers
    await second.refreshEntitlements()
    #expect(!second.hasAccess && second.isResolved)
    let third = controller(backend, store: store)
    #expect(!third.hasAccess) // the hint was cleared, not left to resurrect access
}

@Test @MainActor func nonGrantingOutcomesGrantNothingAndSubscriptionsExpire() async {
    let backend = FakeBackend()
    let purchases = controller(backend)
    await purchases.configure()
    for outcome in [PurchaseOutcome.pending, .cancelled, .unverified] {
        backend.setOutcome(outcome)
        await purchases.purchase(productID: P.full)
        #expect(!purchases.hasAccess)
    }
    #expect(backend.finished.isEmpty)

    // A subscription valid at purchase time stops granting once the clock passes its expiry.
    let clock = ClockBox(Date(timeIntervalSince1970: 5_000))
    backend.setEntitlements([tx(7, P.sub, expires: Date(timeIntervalSince1970: 6_000))])
    let sub = controller(backend, now: { clock.value })
    await sub.refreshEntitlements()
    #expect(sub.hasAccess)
    clock.value = Date(timeIntervalSince1970: 6_001)
    await sub.refreshEntitlements()
    #expect(!sub.hasAccess)
}

private final class ClockBox: @unchecked Sendable {
    private let lock = NSLock()
    private var date: Date
    init(_ date: Date) { self.date = date }
    var value: Date {
        get { lock.withLock { date } }
        set { lock.withLock { date = newValue } }
    }
}

@Test func licenseAndStoreCombineWithoutRevokingEachOther() {
    let claims = LicenseClaims(
        version: 2, id: "l1", app: "app", tier: "pro", features: ["wake_word"],
        deviceId: "d", issuedAt: 0, expiresAt: 9_999_999_999,
    )
    var ledger = EntitlementLedger(config: makeConfig())
    ledger.apply(tx(1, P.base), now: Date(timeIntervalSince1970: 5_000))
    let owned = ledger.snapshot(now: Date(timeIntervalSince1970: 5_000))

    let both = AccessResolver.resolve(purchases: owned, license: claims, requiredEntitlement: "pro")
    #expect(both.isPro && both.source == .store)
    #expect(both.has("wake_word") && both.has("mobile"))

    let licenseOnly = AccessResolver.resolve(purchases: .empty, license: claims)
    #expect(licenseOnly.isPro && licenseOnly.source == .license)

    let neither = AccessResolver.resolve(purchases: .empty, license: nil)
    #expect(!neither.isPro && neither.source == .none)
}
