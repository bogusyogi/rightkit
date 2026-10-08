import Foundation

/// The raw frame hash the core records (`rawBlake3`): BLAKE3 over the frame's pixels as packed
/// BGRA, rows top to bottom, `width * 4` bytes per row with any row padding removed. This is the
/// same definition the Windows backend hashes, so one bundle verifier covers both.
public final class FrameHasher {
    private var scratch = [UInt8]()
    public init() {}

    /// `base` points at row 0. `bytesPerRow` may exceed `width * 4` (row padding).
    public func hashPackedBGRA(base: UnsafeRawPointer, width: Int, height: Int, bytesPerRow: Int) -> String {
        let rowBytes = width * 4
        if bytesPerRow == rowBytes {
            return Blake3.text(UnsafeRawBufferPointer(start: base, count: rowBytes * height))
        }
        if scratch.count < rowBytes * height { scratch = [UInt8](repeating: 0, count: rowBytes * height) }
        scratch.withUnsafeMutableBytes { dst in
            for row in 0..<height {
                memcpy(dst.baseAddress! + row * rowBytes, base + row * bytesPerRow, rowBytes)
            }
        }
        return scratch.withUnsafeBytes { Blake3.text(UnsafeRawBufferPointer(start: $0.baseAddress, count: rowBytes * height)) }
    }
}
