#if os(macOS)
import CoreImage
import Foundation

struct MotionSample: Codable { let scale: Double; let blur: Double; let aberration: Double }

/// Framework-neutral motion envelope; Rust graph remains timing authority.
final class MotionCompositor {
  func sample(_ time: Double, start: Double, end: Double, startScale: Double, endScale: Double) -> MotionSample {
    guard end > start, time >= start, time <= end else { return MotionSample(scale: endScale, blur: 0, aberration: 0) }
    let p = max(0, min(1, (time - start) / (end - start)))
    let eased = p * p * (3 - 2 * p)
    let scale = startScale + (endScale - startScale) * eased
    let envelope = sin(Double.pi * p)
    return MotionSample(scale: scale, blur: envelope, aberration: envelope)
  }
}
#endif
