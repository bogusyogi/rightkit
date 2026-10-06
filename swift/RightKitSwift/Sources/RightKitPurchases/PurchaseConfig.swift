import Foundation

/// How a product's purchases are tied to a stable Apple `appAccountToken`.
///
/// The token lets a later purchase prove it belongs to the same buyer as an earlier one (an
/// upgrade that must be linked to the purchase it upgrades). It is a random UUID, never a user id.
public enum AccountTokenPolicy: Equatable, Sendable {
    /// Purchase without a token.
    case none
    /// Use the stored token, creating and persisting one on first purchase. A token seen on an
    /// owned transaction of this product is saved too, so a reinstall or new device reuses it.
    case stored
    /// Reuse the token carried by an owned transaction of another product. Purchase is refused
    /// until that product is owned — this is the "upgrade needs the base purchase" rule.
    case inherited(from: String)
}

/// One App Store product the app sells. The app supplies these; the module never hard-codes ids.
public struct ProductDefinition: Equatable, Sendable {
    public let id: String
    /// App-defined entitlement names owning this product grants (`"pro"`, `"full"`, ...). These are
    /// what ``PurchaseSnapshot/hasEntitlement(_:)`` and the license bridge answer from.
    public let entitlements: Set<String>
    public let accountToken: AccountTokenPolicy

    public init(id: String, entitlements: Set<String> = [], accountToken: AccountTokenPolicy = .none) {
        self.id = id
        self.entitlements = entitlements
        self.accountToken = accountToken
    }
}

/// One rule for choosing the signed transaction handed to a server for verification. Rules are
/// tried in order; the first whose `primary` (and `companion`, when set) is owned wins. Example:
/// `[full]`, `[upgrade + base]`, `[base]` sends the strongest proof the buyer actually holds.
public struct ProofRule: Equatable, Sendable {
    public let primary: String
    public let companion: String?
    public init(primary: String, companion: String? = nil) {
        self.primary = primary
        self.companion = companion
    }
}

/// User-facing strings. Defaults are brand-neutral; apps override for tone or localisation.
public struct PurchaseMessages: Equatable, Sendable {
    public var productUnavailable = "That product is not available from the App Store yet."
    public var purchased = "Purchase complete."
    public var unverified = "The App Store purchase could not be verified."
    public var pending = "Purchase is pending approval."
    public var cancelled = "Purchase cancelled."
    public var unknownOutcome = "The App Store returned an unknown purchase state."
    public var restored = "Purchases restored."
    public var nothingToRestore = "No App Store purchase was found."
    public var noProducts = "No products are configured in App Store Connect."
    public var inheritedTokenMissing = "Restore the original purchase first so Apple can link the two."
    public var offerCodeUnsupported = "Offer codes are not available on this platform."
    public var refundUnsupported = "Refund requests are not available on this platform."
    public var refundSubmitted = "Refund request submitted to Apple."
    public init() {}
}

/// Everything app-specific about a store. One value, injected once.
public struct PurchaseConfig: Sendable {
    /// Products to load, in display order.
    public let products: [ProductDefinition]
    /// Product ids in descending preference for ``PurchaseSnapshot/activeProductID``: the first one
    /// owned is "the" active product. Owning any of them grants access (``PurchaseSnapshot/hasAccess``);
    /// a product omitted here (an upgrade that unlocks nothing alone) still counts for entitlements.
    public let accessPrecedence: [String]
    public let proofRules: [ProofRule]
    /// Prefix for every persisted key, so two apps (or an app and its extension) never collide.
    public let storageNamespace: String
    public let messages: PurchaseMessages

    public init(
        products: [ProductDefinition],
        accessPrecedence: [String]? = nil,
        proofRules: [ProofRule] = [],
        storageNamespace: String,
        messages: PurchaseMessages = PurchaseMessages(),
    ) {
        self.products = products
        self.accessPrecedence = accessPrecedence ?? products.map(\.id)
        self.proofRules = proofRules
        self.storageNamespace = storageNamespace
        self.messages = messages
    }

    public var productIDs: [String] { products.map(\.id) }
    public func definition(for id: String) -> ProductDefinition? { products.first { $0.id == id } }
}
