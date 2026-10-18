import Foundation

// Native-side timeout conversion, used by SupervisedEngine and NativeBridge.
extension Duration {
    /// Whole milliseconds, at least 1 and capped so the arithmetic cannot overflow. The ABI takes
    /// `timeout_ms` as `u64` and treats zero as one.
    var nativeMilliseconds: UInt64 {
        let parts = components
        if parts.seconds <= 0 && parts.attoseconds <= 0 { return 1 }
        // One year in milliseconds is a far larger wait than any engine call; cap there.
        let cap: Int64 = 365 * 24 * 3600
        if parts.seconds >= cap { return UInt64(cap) * 1000 }
        let millis = parts.seconds * 1000 + parts.attoseconds / 1_000_000_000_000_000
        return UInt64(max(millis, 1))
    }
}
