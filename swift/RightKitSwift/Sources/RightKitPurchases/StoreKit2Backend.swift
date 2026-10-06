#if canImport(StoreKit)
import Foundation
import StoreKit
#if canImport(UIKit)
import UIKit
#endif

/// The real StoreKit 2 backend. All Apple API use in the module lives in this file.
public actor StoreKit2Backend: PurchaseBackend {
    private var products: [String: Product] = [:]
    /// `Transaction` objects awaiting `finish()`, by transaction id. StoreKit needs the original
    /// object back; the value type handed to callers deliberately does not carry it.
    private var pending: [UInt64: Transaction] = [:]

    public init() {}

    public func loadProducts(ids: [String]) async throws -> [ProductInfo] {
        let loaded = try await Product.products(for: ids)
        for product in loaded { products[product.id] = product }
        return loaded.map(Self.info)
    }

    public func purchase(productID: String, accountToken: UUID?) async throws -> PurchaseOutcome {
        guard let product = products[productID] else { throw PurchaseBackendError.productNotLoaded(productID) }
        var options: Set<Product.PurchaseOption> = []
        if let accountToken { options.insert(.appAccountToken(accountToken)) }
        switch try await product.purchase(options: options) {
        case let .success(verification):
            guard case let .verified(transaction) = verification else { return .unverified }
            pending[transaction.id] = transaction
            return .purchased(Self.convert(transaction, jws: verification.jwsRepresentation))
        case .pending: return .pending
        case .userCancelled: return .cancelled
        @unknown default: return .pending
        }
    }

    public func currentEntitlements() async -> [PurchaseTransaction] {
        var result: [PurchaseTransaction] = []
        for await entitlement in Transaction.currentEntitlements {
            // Unverified entitlements are dropped: an unsigned claim must never grant access.
            guard case let .verified(transaction) = entitlement else { continue }
            pending[transaction.id] = transaction
            result.append(Self.convert(transaction, jws: entitlement.jwsRepresentation))
        }
        return result
    }

    public nonisolated func updates() -> AsyncStream<PurchaseTransaction> {
        AsyncStream { continuation in
            let task = Task {
                for await update in Transaction.updates {
                    guard case let .verified(transaction) = update else { continue }
                    await self.remember(transaction)
                    continuation.yield(Self.convert(transaction, jws: update.jwsRepresentation))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func sync() async throws { try await AppStore.sync() }

    public func finish(_ transaction: PurchaseTransaction) async {
        guard let original = pending.removeValue(forKey: transaction.id) else { return }
        await original.finish()
    }

    public func presentOfferCodeRedemption() async throws {
        #if os(iOS)
        guard let scene = await Self.foregroundScene() else { throw PurchaseBackendError.unsupportedPlatform }
        try await AppStore.presentOfferCodeRedeemSheet(in: scene)
        #else
        // On other platforms use SwiftUI's `.offerCodeRedemption(isPresented:)`; results still
        // arrive through `updates()`.
        throw PurchaseBackendError.unsupportedPlatform
        #endif
    }

    public func requestRefund(for transaction: PurchaseTransaction) async throws -> RefundOutcome {
        #if os(iOS)
        guard let scene = await Self.foregroundScene() else { return .unsupported }
        switch try await Transaction.beginRefundRequest(for: transaction.id, in: scene) {
        case .success: return .submitted
        case .userCancelled: return .cancelled
        @unknown default: return .cancelled
        }
        #else
        return .unsupported
        #endif
    }

    private func remember(_ transaction: Transaction) { pending[transaction.id] = transaction }

    #if os(iOS)
    @MainActor
    private static func foregroundScene() -> UIWindowScene? {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive }
    }
    #endif

    private static func info(_ product: Product) -> ProductInfo {
        let kind: ProductKind = switch product.type {
        case .consumable: .consumable
        case .nonConsumable: .nonConsumable
        case .autoRenewable: .autoRenewable
        case .nonRenewable: .nonRenewable
        default: .nonConsumable
        }
        return ProductInfo(
            id: product.id, displayName: product.displayName, displayPrice: product.displayPrice,
            description: product.description, kind: kind,
        )
    }

    private static func convert(_ transaction: Transaction, jws: String) -> PurchaseTransaction {
        var offerCode = false
        if #available(iOS 17.2, macOS 14.2, *) { offerCode = transaction.offer?.type == .code }
        return PurchaseTransaction(
            id: transaction.id, originalID: transaction.originalID, productID: transaction.productID,
            purchaseDate: transaction.purchaseDate, revocationDate: transaction.revocationDate,
            expirationDate: transaction.expirationDate, appAccountToken: transaction.appAccountToken,
            signedJWS: jws, environment: transaction.environment.rawValue, redeemedWithOfferCode: offerCode,
        )
    }
}
#endif
