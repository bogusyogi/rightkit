import Combine
import Foundation

public enum ProductLoadState: Equatable, Sendable {
    case idle
    case loading
    case loaded
    case failed(String)
}

/// Observable purchase state for SwiftUI. Owns the product list, the transaction listener, the
/// ledger, and the persisted hints; everything Apple-specific is behind ``PurchaseBackend``.
///
/// StoreKit verification is the entitlement authority on this device. A server rejecting the
/// purchase (``markServerRejected()``) is recorded for retry and diagnostics but never revokes
/// local access — only a refund or revocation from the store does.
@MainActor
public final class PurchaseController: ObservableObject {
    @Published public private(set) var products: [ProductInfo] = []
    @Published public private(set) var snapshot: PurchaseSnapshot = .empty
    @Published public private(set) var loadState: ProductLoadState = .idle
    @Published public private(set) var isBusy = false
    @Published public var message = ""
    @Published public private(set) var lastError: String?
    @Published public private(set) var serverRejected = false
    /// False until the first entitlement refresh finishes. Until then access is answered from the
    /// persisted hint, so a cold launch does not flash "free" for an owner.
    @Published public private(set) var isResolved = false

    public let config: PurchaseConfig
    private let backend: any PurchaseBackend
    private let cache: AccessCacheStore
    private let keyValues: any KeyValueStore
    private let tokens: any AccountTokenStore
    private let now: @Sendable () -> Date
    private var ledger: EntitlementLedger
    private var cachedProductID: String?
    nonisolated(unsafe) private var updatesTask: Task<Void, Never>?

    private var blockedKey: String { "\(config.storageNamespace).blockedOriginalTransactionID" }

    public init(
        config: PurchaseConfig,
        backend: any PurchaseBackend,
        store: any KeyValueStore = UserDefaultsStore(),
        accountTokens: any AccountTokenStore,
        now: @escaping @Sendable () -> Date = { Date() },
    ) {
        self.config = config
        self.backend = backend
        self.keyValues = store
        self.cache = AccessCacheStore(store: store, namespace: config.storageNamespace)
        self.tokens = accountTokens
        self.now = now
        self.ledger = EntitlementLedger(config: config)
        self.cachedProductID = cache.load().productID
        self.serverRejected = store.string(forKey: "\(config.storageNamespace).blockedOriginalTransactionID") != nil
    }

    deinit { updatesTask?.cancel() }

    // MARK: Derived state

    /// Verified access, or the persisted hint until the first refresh completes.
    public var activeProductID: String? {
        snapshot.activeProductID ?? (isResolved ? nil : cachedProductID)
    }
    public var hasAccess: Bool { activeProductID != nil }
    public func owns(_ productID: String) -> Bool { snapshot.owns(productID) }
    public func product(_ id: String) -> ProductInfo? { products.first { $0.id == id } }

    // MARK: Lifecycle

    /// Call once at launch: starts the transaction listener (so nothing delivered while the app was
    /// closed is missed), loads products, and refreshes entitlements.
    public func configure() async {
        startListening()
        await retry()
    }

    public func retry() async {
        await loadProducts()
        await refreshEntitlements()
    }

    public func startListening() {
        guard updatesTask == nil else { return }
        let stream = backend.updates()
        updatesTask = Task { [weak self] in
            for await transaction in stream {
                await self?.handle(update: transaction)
            }
        }
    }

    public func loadProducts() async {
        loadState = .loading
        lastError = nil
        do {
            let loaded = try await backend.loadProducts(ids: config.productIDs)
            let order = Dictionary(uniqueKeysWithValues: config.productIDs.enumerated().map { ($1, $0) })
            products = loaded.sorted { (order[$0.id] ?? .max) < (order[$1.id] ?? .max) }
            if products.isEmpty {
                fail(config.messages.noProducts)
                loadState = .failed(config.messages.noProducts)
            } else {
                loadState = .loaded
            }
        } catch {
            fail(error.localizedDescription)
            loadState = .failed(error.localizedDescription)
        }
    }

    public func refreshEntitlements() async {
        let current = await backend.currentEntitlements()
        // Built off to the side and swapped in, so observers never see an empty intermediate state.
        var next = EntitlementLedger(config: config)
        next.replaceAll(with: current, now: now())
        ledger = next
        for tx in current { rememberToken(from: tx) }
        isResolved = true
        recompute()
    }

    // MARK: Purchase / restore / offer codes / refunds

    public func purchase(productID: String) async {
        guard product(productID) != nil else {
            message = config.messages.productUnavailable
            await retry()
            return
        }
        let token: UUID?
        switch config.definition(for: productID)?.accountToken ?? .none {
        case .none: token = nil
        case .stored: token = tokens.loadOrCreate()
        case let .inherited(source):
            guard let inherited = snapshot.accountToken(for: source) else {
                message = config.messages.inheritedTokenMissing
                return
            }
            token = inherited
        }

        isBusy = true
        message = ""
        defer { isBusy = false }
        do {
            switch try await backend.purchase(productID: productID, accountToken: token) {
            case let .purchased(transaction):
                await grant(transaction)
                message = config.messages.purchased
            case .unverified: message = config.messages.unverified
            case .pending: message = config.messages.pending
            case .cancelled: message = config.messages.cancelled
            }
        } catch {
            fail(error.localizedDescription)
        }
    }

    public func restore() async {
        isBusy = true
        message = ""
        defer { isBusy = false }
        do {
            try await backend.sync()
            await refreshEntitlements()
            message = hasAccess ? config.messages.restored : config.messages.nothingToRestore
        } catch {
            fail(error.localizedDescription)
        }
    }

    /// Present Apple's offer-code sheet. The redeemed transaction arrives through the listener,
    /// like any other update, and grants through the same path as a purchase.
    public func redeemOfferCode() async {
        do {
            try await backend.presentOfferCodeRedemption()
        } catch PurchaseBackendError.unsupportedPlatform {
            message = config.messages.offerCodeUnsupported
        } catch {
            fail(error.localizedDescription)
        }
    }

    /// Open Apple's refund sheet for an owned product. Apple decides; if it refunds, the
    /// revocation arrives through the listener and access ends.
    public func requestRefund(productID: String) async {
        guard let transaction = snapshot.transaction(for: productID) else { return }
        do {
            switch try await backend.requestRefund(for: transaction) {
            case .submitted: message = config.messages.refundSubmitted
            case .cancelled: break
            case .unsupported: message = config.messages.refundUnsupported
            }
        } catch PurchaseBackendError.unsupportedPlatform {
            message = config.messages.refundUnsupported
        } catch {
            fail(error.localizedDescription)
        }
    }

    // MARK: Server reconciliation (optional)

    /// The app's server accepted this purchase (lease, bridge, account link).
    public func markServerAccepted() {
        serverRejected = false
        keyValues.set(nil, forKey: blockedKey)
        recompute()
    }

    /// The server refused it. Local access stays; the purchase is flagged so the app can retry.
    public func markServerRejected() {
        serverRejected = true
        if let original = snapshot.originalTransactionID { keyValues.set(original, forKey: blockedKey) }
        recompute()
    }

    // MARK: Internals

    func handle(update transaction: PurchaseTransaction) async {
        guard config.definition(for: transaction.productID) != nil else { return }
        await grant(transaction)
    }

    /// Apply a delivered transaction, then finish it — only after it is reflected in state, so a
    /// crash in between re-delivers rather than loses the purchase.
    private func grant(_ transaction: PurchaseTransaction) async {
        ledger.apply(transaction, now: now())
        rememberToken(from: transaction)
        recompute()
        await backend.finish(transaction)
    }

    private func rememberToken(from transaction: PurchaseTransaction) {
        guard transaction.revocationDate == nil, let token = transaction.appAccountToken,
              config.definition(for: transaction.productID)?.accountToken == .stored
        else { return }
        tokens.save(token)
    }

    private func recompute() {
        snapshot = ledger.snapshot(now: now())
        if let productID = snapshot.activeProductID {
            cachedProductID = productID
            if let original = snapshot.originalTransactionID {
                serverRejected = keyValues.string(forKey: blockedKey) == original
            }
            cache.save(productID: productID, at: now())
        } else if isResolved {
            cachedProductID = nil
            serverRejected = false
            keyValues.set(nil, forKey: blockedKey)
            cache.clear()
        }
    }

    private func fail(_ text: String) {
        lastError = text
        message = text
    }
}

extension PurchaseSnapshot {
    func transaction(for productID: String) -> PurchaseTransaction? { transactions[productID] }
}
