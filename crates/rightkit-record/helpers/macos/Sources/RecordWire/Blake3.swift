import Foundation

/// BLAKE3 (default hash mode, 32-byte output). The core verifies `blake3:<64 hex>` frame hashes,
/// so the helper needs the same function. Chunk chaining values are independent, so large inputs
/// (a raw frame is megabytes) are hashed across cores, then merged in the standard tree order.
public enum Blake3 {
    private static let iv: [UInt32] = [
        0x6A09_E667, 0xBB67_AE85, 0x3C6E_F372, 0xA54F_F53A,
        0x510E_527F, 0x9B05_688C, 0x1F83_D9AB, 0x5BE0_CD19,
    ]
    private static let flagChunkStart: UInt32 = 1
    private static let flagChunkEnd: UInt32 = 2
    private static let flagParent: UInt32 = 4
    private static let flagRoot: UInt32 = 8
    private static let chunkLen = 1024
    private static let blockLen = 64
    private static let permutation: [Int] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
    /// Chunks per parallel task. A task is 32 KiB of input.
    private static let chunksPerTask = 32

    // MARK: public

    /// `blake3:<64 lowercase hex>` of the bytes.
    public static func text(_ bytes: UnsafeRawBufferPointer) -> String {
        "blake3:" + hex(hash(bytes))
    }

    public static func text(_ data: Data) -> String {
        data.withUnsafeBytes { text($0) }
    }

    public static func hash(_ data: Data) -> [UInt8] {
        data.withUnsafeBytes { hash($0) }
    }

    public static func hex(_ bytes: [UInt8]) -> String {
        let digits = Array("0123456789abcdef".utf8)
        var out = [UInt8]()
        out.reserveCapacity(bytes.count * 2)
        for byte in bytes {
            out.append(digits[Int(byte >> 4)])
            out.append(digits[Int(byte & 15)])
        }
        return String(decoding: out, as: UTF8.self)
    }

    public static func hash(_ input: UnsafeRawBufferPointer) -> [UInt8] {
        let count = input.count
        var cv = [UInt32](repeating: 0, count: 8)
        guard let base = input.baseAddress, count > 0 else {
            withUnsafeTemporaryAllocation(of: UInt8.self, capacity: 1) { empty in
                cv.withUnsafeMutableBufferPointer { out in
                    chunkCV(UnsafeRawPointer(empty.baseAddress!), 0, 0, true, out.baseAddress!)
                }
            }
            return bytes(of: cv)
        }
        if count <= chunkLen {
            cv.withUnsafeMutableBufferPointer { chunkCV(base, count, 0, true, $0.baseAddress!) }
            return bytes(of: cv)
        }
        let chunks = (count + chunkLen - 1) / chunkLen
        // Chaining values of every chunk but the last (the last is the tail of the tree walk).
        var cvs = [UInt32](repeating: 0, count: chunks * 8)
        cvs.withUnsafeMutableBufferPointer { cvBuffer in
            let cvBase = cvBuffer.baseAddress!
            let tasks = (chunks + chunksPerTask - 1) / chunksPerTask
            let work: (Int) -> Void = { task in
                let first = task * chunksPerTask
                let last = min(chunks, first + chunksPerTask)
                for chunk in first..<last {
                    let offset = chunk * chunkLen
                    let length = min(chunkLen, count - offset)
                    chunkCV(base + offset, length, UInt64(chunk), false, cvBase + chunk * 8)
                }
            }
            if tasks > 1 {
                DispatchQueue.concurrentPerform(iterations: tasks, execute: work)
            } else {
                work(0)
            }
        }
        // Standard incremental tree: push each completed chunk, merging equal-sized subtrees.
        var stack = [[UInt32]]()
        stack.reserveCapacity(64)
        for chunk in 0..<(chunks - 1) {
            var node = Array(cvs[(chunk * 8)..<(chunk * 8 + 8)])
            var total = chunk + 1
            while total & 1 == 0 {
                let left = stack.removeLast()
                node = parentCV(left, node, false)
                total >>= 1
            }
            stack.append(node)
        }
        var node = Array(cvs[((chunks - 1) * 8)..<(chunks * 8)])
        while let left = stack.popLast() {
            node = parentCV(left, node, stack.isEmpty)
        }
        return bytes(of: node)
    }

    // MARK: internals

    private static func bytes(of words: [UInt32]) -> [UInt8] {
        var out = [UInt8]()
        out.reserveCapacity(32)
        for word in words {
            out.append(UInt8(truncatingIfNeeded: word))
            out.append(UInt8(truncatingIfNeeded: word >> 8))
            out.append(UInt8(truncatingIfNeeded: word >> 16))
            out.append(UInt8(truncatingIfNeeded: word >> 24))
        }
        return out
    }

    @inline(__always)
    private static func rotr(_ x: UInt32, _ n: UInt32) -> UInt32 { (x >> n) | (x << (32 - n)) }

    @inline(__always)
    private static func g(
        _ s: UnsafeMutablePointer<UInt32>, _ a: Int, _ b: Int, _ c: Int, _ d: Int,
        _ mx: UInt32, _ my: UInt32
    ) {
        s[a] = s[a] &+ s[b] &+ mx
        s[d] = rotr(s[d] ^ s[a], 16)
        s[c] = s[c] &+ s[d]
        s[b] = rotr(s[b] ^ s[c], 12)
        s[a] = s[a] &+ s[b] &+ my
        s[d] = rotr(s[d] ^ s[a], 8)
        s[c] = s[c] &+ s[d]
        s[b] = rotr(s[b] ^ s[c], 7)
    }

    @inline(__always)
    private static func round(_ s: UnsafeMutablePointer<UInt32>, _ m: UnsafeMutablePointer<UInt32>) {
        g(s, 0, 4, 8, 12, m[0], m[1])
        g(s, 1, 5, 9, 13, m[2], m[3])
        g(s, 2, 6, 10, 14, m[4], m[5])
        g(s, 3, 7, 11, 15, m[6], m[7])
        g(s, 0, 5, 10, 15, m[8], m[9])
        g(s, 1, 6, 11, 12, m[10], m[11])
        g(s, 2, 7, 8, 13, m[12], m[13])
        g(s, 3, 4, 9, 14, m[14], m[15])
    }

    /// One compression. `cv` is 8 words, `block` is 16 words (clobbered). Writes the 8-word
    /// chaining value to `out` (which may alias `cv`).
    private static func compress(
        _ cv: UnsafeMutablePointer<UInt32>, _ block: UnsafeMutablePointer<UInt32>,
        _ counter: UInt64, _ length: UInt32, _ flags: UInt32, _ out: UnsafeMutablePointer<UInt32>
    ) {
        withUnsafeTemporaryAllocation(of: UInt32.self, capacity: 32) { scratch in
            let s = scratch.baseAddress!
            let t = s + 16
            for i in 0..<8 { s[i] = cv[i] }
            s[8] = iv[0]; s[9] = iv[1]; s[10] = iv[2]; s[11] = iv[3]
            s[12] = UInt32(truncatingIfNeeded: counter)
            s[13] = UInt32(truncatingIfNeeded: counter >> 32)
            s[14] = length
            s[15] = flags
            for r in 0..<7 {
                round(s, block)
                if r < 6 {
                    for i in 0..<16 { t[i] = block[permutation[i]] }
                    for i in 0..<16 { block[i] = t[i] }
                }
            }
            for i in 0..<8 { out[i] = s[i] ^ s[i + 8] }
        }
    }

    /// The chaining value of one chunk (or the root output when `root` and it is the whole input).
    private static func chunkCV(
        _ data: UnsafeRawPointer, _ length: Int, _ counter: UInt64, _ root: Bool,
        _ out: UnsafeMutablePointer<UInt32>
    ) {
        withUnsafeTemporaryAllocation(of: UInt32.self, capacity: 24) { scratch in
            let cv = scratch.baseAddress!
            let m = cv + 8
            for i in 0..<8 { cv[i] = iv[i] }
            let blocks = max(1, (length + blockLen - 1) / blockLen)
            for b in 0..<blocks {
                let offset = b * blockLen
                let take = max(0, min(blockLen, length - offset))
                if take == blockLen {
                    memcpy(m, data + offset, blockLen)
                } else {
                    memset(m, 0, blockLen)
                    if take > 0 { memcpy(m, data + offset, take) }
                }
                var flags: UInt32 = 0
                if b == 0 { flags |= flagChunkStart }
                if b == blocks - 1 {
                    flags |= flagChunkEnd
                    if root { flags |= flagRoot }
                }
                compress(cv, m, counter, UInt32(take), flags, cv)
            }
            for i in 0..<8 { out[i] = cv[i] }
        }
    }

    private static func parentCV(_ left: [UInt32], _ right: [UInt32], _ root: Bool) -> [UInt32] {
        var cv = iv
        var block = left + right
        var out = [UInt32](repeating: 0, count: 8)
        let flags = flagParent | (root ? flagRoot : 0)
        cv.withUnsafeMutableBufferPointer { cvp in
            block.withUnsafeMutableBufferPointer { bp in
                out.withUnsafeMutableBufferPointer { op in
                    compress(cvp.baseAddress!, bp.baseAddress!, 0, 64, flags, op.baseAddress!)
                }
            }
        }
        return out
    }
}
