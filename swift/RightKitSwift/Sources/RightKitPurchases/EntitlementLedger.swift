import Foundation

/// A signed transaction (and an optional companion) to hand a server for verification.
public struct PurchaseProof: Equatable, Sendable {
    public let signedTransaction: String
    public let companionSignedTransaction: String?
}

/// Immutable view of what the buyer owns right now. Derived, never stored.
public struct PurchaseSnapshot: Equatable, Sendable {
    public let ownedProductIDs: Set<String>
    public let entitlements: Set<String>
    /// Highest-precedence owned product in ``PurchaseConfig/accessPrecedence``, if any.
    public let activeProductID: String?
    /// `originalID` of the active product's purchase (any owned purchase if none is active).
    public let originalTransactionID: String?
    public let proof: PurchaseProof?
    let transactions: [String: PurchaseTransaction]

    public var hasAccess: Bool { activeProductID != nil }
    public func owns(_ productID: String) -> Bool { ownedProductIDs.contains(productID) }
    public func hasEntitlement(_ name: String) -> Bool { entitlements.contains(name) }
    public func signedTransaction(for productID: String) -> String? { transactions[productID]?.signedJWS }
    public func accountToken(for productID: String) -> UUID? { transactions[productID]?.appAccountToken }

    public static let empty = PurchaseSnapshot(
        ownedProductIDs: [], entitlements: [], activeProductID: nil,
        originalTransactionID: nil, proof: nil, transactions: [:],
    )
}

/// Pure entitlement bookkeeping: transactions in, ownership out. No StoreKit, no I/O, no clock of
/// its own — `now` is passed in, so every rule (refund, expiry, replay, precedence) is testable.
struct EntitlementLedger {
    enum Change: Equatable { case granted, revoked, ignored }

    let config: PurchaseConfig
    private var records: [String: PurchaseTransaction] = [:]
    /// Purchases already refunded/revoked. A late or replayed grant for one must not resurrect it.
    private var revokedOriginalIDs: Set<UInt64> = []

    init(config: PurchaseConfig) { self.config = config }

    private func isValid(_ tx: PurchaseTransaction, now: Date) -> Bool {
        if tx.revocationDate != nil || revokedOriginalIDs.contains(tx.originalID) { return false }
        if let expiry = tx.expirationDate, expiry <= now { return false }
        return true
    }

    /// Prefer the later-expiring (then later-purchased) transaction — a renewal replaces its predecessor.
    private static func supersedes(_ new: PurchaseTransaction, _ old: PurchaseTransaction) -> Bool {
        let newExpiry = new.expirationDate ?? .distantFuture
        let oldExpiry = old.expirationDate ?? .distantFuture
        if newExpiry != oldExpiry { return newExpiry > oldExpiry }
        return new.purchaseDate >= old.purchaseDate
    }

    /// Apply one delivered transaction. Products outside the config are ignored.
    @discardableResult
    mutating func apply(_ tx: PurchaseTransaction, now: Date) -> Change {
        guard config.definition(for: tx.productID) != nil else { return .ignored }
        if tx.revocationDate != nil {
            revokedOriginalIDs.insert(tx.originalID)
            if records[tx.productID]?.originalID == tx.originalID {
                records[tx.productID] = nil
                return .revoked
            }
            return .ignored
        }
        guard isValid(tx, now: now) else {
            // Expired (or previously revoked): drop any record of the same purchase.
            if records[tx.productID]?.originalID == tx.originalID, records[tx.productID]?.id == tx.id {
                records[tx.productID] = nil
                return .revoked
            }
            return .ignored
        }
        if let existing = records[tx.productID], !Self.supersedes(tx, existing) { return .ignored }
        records[tx.productID] = tx
        return .granted
    }

    /// Replace everything with the store's own current-entitlements list (the source of truth on
    /// launch and after a restore). Anything the store no longer lists is no longer owned.
    mutating func replaceAll(with transactions: [PurchaseTransaction], now: Date) {
        records = [:]
        for tx in transactions { apply(tx, now: now) }
    }

    func snapshot(now: Date) -> PurchaseSnapshot {
        let live = records.filter { isValid($0.value, now: now) }
        let owned = Set(live.keys)
        var entitlements: Set<String> = []
        for id in owned { entitlements.formUnion(config.definition(for: id)?.entitlements ?? []) }
        let active = config.accessPrecedence.first(where: owned.contains)
        let original = (active.flatMap { live[$0] } ?? live.values.sorted { $0.id < $1.id }.first)
            .map { String($0.originalID) }
        var proof: PurchaseProof?
        for rule in config.proofRules {
            guard let primary = live[rule.primary] else { continue }
            if let companion = rule.companion {
                guard let other = live[companion] else { continue }
                proof = PurchaseProof(signedTransaction: primary.signedJWS, companionSignedTransaction: other.signedJWS)
            } else {
                proof = PurchaseProof(signedTransaction: primary.signedJWS, companionSignedTransaction: nil)
            }
            break
        }
        return PurchaseSnapshot(
            ownedProductIDs: owned, entitlements: entitlements, activeProductID: active,
            originalTransactionID: original, proof: proof, transactions: live,
        )
    }
}
