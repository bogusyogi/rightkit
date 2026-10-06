import Foundation
import RightKitLicense

/// Where a resolved grant came from. The store is preferred when both exist: it is local,
/// immediate, and what a server bridge publishes from, so the two never disagree about which
/// purchase "is" this device's access.
public enum AccessSource: Equatable, Sendable {
    case none
    case store
    case license
}

public struct ResolvedAccess: Equatable, Sendable {
    public let isPro: Bool
    public let source: AccessSource
    /// Union of store entitlements and signed-license features.
    public let features: Set<String>
    public func has(_ feature: String) -> Bool { features.contains(feature) }
}

/// Combines the two entitlement authorities — App Store ownership and a verified `RightKitLicense`
/// token — into one answer. Neither can revoke the other: a refund does not disable a valid signed
/// license, and an expired license does not disable an owned purchase.
public enum AccessResolver {
    /// - Parameters:
    ///   - requiredEntitlement: store entitlement that counts as Pro; `nil` means "owning any
    ///     access product counts" (single-tier apps).
    ///   - claims: output of ``LicenseVerifier/verify(token:app:deviceId:now:)``; pass `nil` when
    ///     there is no token or it failed verification. Unverified claims cannot be constructed.
    public static func resolve(
        purchases: PurchaseSnapshot,
        license claims: LicenseClaims?,
        requiredEntitlement: String? = nil,
    ) -> ResolvedAccess {
        let storePro = requiredEntitlement.map(purchases.hasEntitlement) ?? purchases.hasAccess
        let licensePro = claims?.isPro ?? false
        var features = purchases.entitlements
        if let claims { features.formUnion(claims.features) }
        let source: AccessSource = storePro ? .store : (licensePro ? .license : .none)
        return ResolvedAccess(isPro: storePro || licensePro, source: source, features: features)
    }
}
