import Foundation

public enum ProductKind: String, Equatable, Sendable {
    case consumable, nonConsumable, autoRenewable, nonRenewable
}

/// A product as the store describes it. Display strings come from the store, already localised
/// and priced for the buyer's storefront, so UI shows them verbatim.
public struct ProductInfo: Equatable, Identifiable, Sendable {
    public let id: String
    public let displayName: String
    public let displayPrice: String
    public let description: String
    public let kind: ProductKind
    public init(id: String, displayName: String, displayPrice: String, description: String, kind: ProductKind) {
        self.id = id
        self.displayName = displayName
        self.displayPrice = displayPrice
        self.description = description
        self.kind = kind
    }
}

/// A store-verified transaction, reduced to what entitlement logic needs. Only verified
/// transactions are ever turned into one — an unverified result never reaches the ledger.
public struct PurchaseTransaction: Equatable, Sendable {
    public let id: UInt64
    /// Stable across renewals and re-deliveries of one purchase; what a server keys a license by.
    public let originalID: UInt64
    public let productID: String
    public let purchaseDate: Date
    public let revocationDate: Date?
    public let expirationDate: Date?
    public let appAccountToken: UUID?
    /// Apple's signed JWS for this transaction: the proof a server verifies. Never persisted here.
    public let signedJWS: String
    public let environment: String
    public let redeemedWithOfferCode: Bool

    public init(
        id: UInt64, originalID: UInt64? = nil, productID: String,
        purchaseDate: Date = Date(timeIntervalSince1970: 0),
        revocationDate: Date? = nil, expirationDate: Date? = nil,
        appAccountToken: UUID? = nil, signedJWS: String = "",
        environment: String = "production", redeemedWithOfferCode: Bool = false,
    ) {
        self.id = id
        self.originalID = originalID ?? id
        self.productID = productID
        self.purchaseDate = purchaseDate
        self.revocationDate = revocationDate
        self.expirationDate = expirationDate
        self.appAccountToken = appAccountToken
        self.signedJWS = signedJWS
        self.environment = environment
        self.redeemedWithOfferCode = redeemedWithOfferCode
    }
}

public enum PurchaseOutcome: Equatable, Sendable {
    case purchased(PurchaseTransaction)
    case pending
    case cancelled
    /// The store returned a transaction that failed signature verification. Grant nothing.
    case unverified
}

public enum RefundOutcome: Equatable, Sendable {
    case submitted
    case cancelled
    case unsupported
}

public enum PurchaseBackendError: Error, Equatable, Sendable {
    case unsupportedPlatform
    case productNotLoaded(String)
    case transactionNotFound(UInt64)
}

/// The seam between entitlement logic and Apple's StoreKit. ``StoreKit2Backend`` is the real
/// implementation; tests and previews supply a fake, so every rule above this line is exercised
/// without a store, a sandbox account, or a device.
public protocol PurchaseBackend: Sendable {
    func loadProducts(ids: [String]) async throws -> [ProductInfo]
    func purchase(productID: String, accountToken: UUID?) async throws -> PurchaseOutcome
    /// Verified, currently-valid transactions (`Transaction.currentEntitlements`).
    func currentEntitlements() async -> [PurchaseTransaction]
    /// Verified transactions arriving outside a purchase call: Ask-to-Buy approval, offer-code
    /// redemption, purchases on another device, renewals, refunds and revocations.
    func updates() -> AsyncStream<PurchaseTransaction>
    /// Ask the store to re-sync purchases with the App Store account (`AppStore.sync()`).
    func sync() async throws
    /// Tell the store the app has delivered the content; until then it re-delivers the transaction.
    func finish(_ transaction: PurchaseTransaction) async
    func presentOfferCodeRedemption() async throws
    func requestRefund(for transaction: PurchaseTransaction) async throws -> RefundOutcome
}
