import Foundation

/// Display scaling only; never changes recorded audio or capture settings.
enum WatchRecordingMeter {
    static func level(powerDB: Float) -> Double {
        guard powerDB.isFinite else { return 0 }
        // Recorder power is logarithmic. Preserve useful movement across
        // quiet speech instead of flattening everything below -36 dB.
        return min(1, max(0, (Double(powerDB) + 60) / 54))
    }

    /// Fixed spatial weights: current energy changes height, never position.
    static func levels(level: Double, count: Int = 17) -> [Double] {
        guard count > 0 else { return [] }
        let energy = level.isFinite ? min(1, max(0, level)) : 0
        let middle = Double(count - 1) / 2
        return (0..<count).map { index in
            let envelope = 1 - 0.35 * abs(Double(index) - middle) / max(1, middle)
            return energy * envelope
        }
    }

    static func barHeight(level: Double) -> Double {
        guard level.isFinite else { return 3 }
        return 3 + 21 * min(1, max(0, level))
    }
}
