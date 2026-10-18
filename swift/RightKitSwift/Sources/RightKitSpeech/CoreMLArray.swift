#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Accelerate
import CoreML
import Foundation

enum ParakeetCoreMLArray {

    static func float16(shape: [Int], values: [Float]) throws -> MLMultiArray {
        let array = try make(shape: shape, values: values, dataType: .float16)
        // Bulk Float32 -> Float16, same reason as readFloats: the element loop
        // dominated wall-clock on large encoder inputs.
        let pointer = array.dataPointer.assumingMemoryBound(to: Float16.self)
        var mutableValues = values
        try mutableValues.withUnsafeMutableBufferPointer { buffer in
            var src = vImage_Buffer(data: buffer.baseAddress, height: 1,
                                    width: vImagePixelCount(values.count),
                                    rowBytes: values.count * MemoryLayout<Float>.size)
            var dst = vImage_Buffer(data: pointer, height: 1,
                                    width: vImagePixelCount(values.count),
                                    rowBytes: values.count * MemoryLayout<Float16>.size)
            let status = vImageConvert_PlanarFtoPlanar16F(&src, &dst, 0)
            if status != kvImageNoError { throw SpeechError.unsupportedArrayType }
        }
        return array
    }

    /// Update an existing contiguous Float16 input without allocating another
    /// MLMultiArray or a temporary converted buffer. Core ML prediction is
    /// synchronous, so callers may mutate this input again after prediction
    /// returns.
    static func writeFloat16(_ array: MLMultiArray, values: [Float]) throws {
        guard array.dataType == .float16 else {
            throw SpeechError.unsupportedArrayType
        }
        guard array.count == values.count else {
            throw SpeechError.invalidArrayShape
        }
        guard !values.isEmpty else { return }

        let pointer = array.dataPointer.assumingMemoryBound(to: Float16.self)
        try values.withUnsafeBufferPointer { buffer in
            guard let source = buffer.baseAddress else { return }
            var src = vImage_Buffer(
                data: UnsafeMutableRawPointer(mutating: source),
                height: 1,
                width: vImagePixelCount(values.count),
                rowBytes: values.count * MemoryLayout<Float>.size
            )
            var dst = vImage_Buffer(
                data: pointer,
                height: 1,
                width: vImagePixelCount(values.count),
                rowBytes: values.count * MemoryLayout<Float16>.size
            )
            guard vImageConvert_PlanarFtoPlanar16F(&src, &dst, 0) == kvImageNoError else {
                throw SpeechError.unsupportedArrayType
            }
        }
    }

    static func int32(shape: [Int], values: [Int32]) throws -> MLMultiArray {
        let array = try make(shape: shape, values: values, dataType: .int32)
        let pointer = array.dataPointer.assumingMemoryBound(to: Int32.self)
        for index in values.indices {
            pointer[index] = values[index]
        }
        return array
    }

    /// Read an `MLMultiArray` into `[Float]`.
    ///
    /// PERFORMANCE: fetching `array.dataPointer` and re-evaluating the `dataType`
    /// switch **inside** the per-element loop, and recomputing the flat offset
    /// with a `reduce` over the index vector for every element, is expensive:
    /// `dataPointer` is an Objective-C property, so a [1, 1024, 188] encoder
    /// output would cost ~192k ObjC message sends plus ~192k reduces per
    /// window — roughly 90% of on-device decode wall-clock, versus ~30 ms for
    /// the Core ML compute itself.
    ///
    /// Instead: the pointer and data type are hoisted once, C-contiguous arrays
    /// (the overwhelmingly common case) take a bulk Accelerate conversion, and
    /// the strided path keeps the correct gather but with an incremental
    /// odometer instead of a reduce. Core ML outputs are NOT guaranteed
    /// contiguous — reading `dataPointer` linearly without checking strides
    /// silently scrambles layout, which is why the strided fallback stays.
    static func readFloats(_ array: MLMultiArray) throws -> [Float] {
        let shape = array.shape.map(\.intValue)
        let strides = array.strides.map(\.intValue)
        let count = shape.reduce(1, *)
        guard count > 0 else { return [] }

        // Hoisted once — these were the per-element costs.
        let dataType = array.dataType
        let base = array.dataPointer

        var isContiguous = true
        var expected = 1
        for dimension in stride(from: shape.count - 1, through: 0, by: -1) {
            if strides[dimension] != expected { isContiguous = false; break }
            expected *= shape[dimension]
        }

        if !isContiguous,
           let rowPadded = try readRowPadded(
               dataType: dataType,
               base: base,
               shape: shape,
               strides: strides,
               count: count
           ) {
            return rowPadded
        }

        if isContiguous {
            switch dataType {
            case .float16:
                var out = [Float](repeating: 0, count: count)
                var src = vImage_Buffer(data: base, height: 1, width: vImagePixelCount(count),
                                        rowBytes: count * MemoryLayout<Float16>.size)
                try out.withUnsafeMutableBufferPointer { buffer in
                    var dst = vImage_Buffer(data: buffer.baseAddress, height: 1,
                                            width: vImagePixelCount(count),
                                            rowBytes: count * MemoryLayout<Float>.size)
                    let status = vImageConvert_Planar16FtoPlanarF(&src, &dst, 0)
                    if status != kvImageNoError { throw SpeechError.unsupportedArrayType }
                }
                return out
            case .float32:
                let pointer = base.assumingMemoryBound(to: Float.self)
                return Array(UnsafeBufferPointer(start: pointer, count: count))
            case .double:
                let pointer = base.assumingMemoryBound(to: Double.self)
                var out = [Float](repeating: 0, count: count)
                vDSP_vdpsp(pointer, 1, &out, 1, vDSP_Length(count))
                return out
            default:
                throw SpeechError.unsupportedArrayType
            }
        }

        // Strided gather: same semantics as before, without the per-element
        // ObjC call, dataType switch, or reduce.
        var values = [Float](repeating: 0, count: count)
        var indices = Array(repeating: 0, count: shape.count)
        var offset = 0
        switch dataType {
        case .float16:
            let pointer = base.assumingMemoryBound(to: Float16.self)
            for position in 0..<count {
                values[position] = Float(pointer[offset])
                offset = Self.advance(&indices, shape: shape, strides: strides, offset: offset)
            }
        case .float32:
            let pointer = base.assumingMemoryBound(to: Float.self)
            for position in 0..<count {
                values[position] = pointer[offset]
                offset = Self.advance(&indices, shape: shape, strides: strides, offset: offset)
            }
        case .double:
            let pointer = base.assumingMemoryBound(to: Double.self)
            for position in 0..<count {
                values[position] = Float(pointer[offset])
                offset = Self.advance(&indices, shape: shape, strides: strides, offset: offset)
            }
        default:
            throw SpeechError.unsupportedArrayType
        }
        return values
    }

    /// Core ML commonly pads the innermost encoder row to an aligned pitch.
    /// vImage can consume that pitch directly; genuinely scattered layouts
    /// return `nil` and stay on the general odometer path.
    private static func readRowPadded(
        dataType: MLMultiArrayDataType,
        base: UnsafeMutableRawPointer,
        shape: [Int],
        strides: [Int],
        count: Int
    ) throws -> [Float]? {
        guard shape.count >= 2, strides.last == 1 else { return nil }
        let rowLength = shape[shape.count - 1]
        let rowPitch = strides[shape.count - 2]
        let rows = count / max(rowLength, 1)
        guard rowPitch >= rowLength, rows > 0 else { return nil }

        var expectedOuter = rowPitch
        for dimension in stride(from: shape.count - 2, through: 1, by: -1) {
            expectedOuter *= shape[dimension]
            guard strides[dimension - 1] == expectedOuter else { return nil }
        }

        switch dataType {
        case .float16:
            var output = [Float](repeating: 0, count: count)
            var source = vImage_Buffer(
                data: base,
                height: vImagePixelCount(rows),
                width: vImagePixelCount(rowLength),
                rowBytes: rowPitch * MemoryLayout<Float16>.size
            )
            try output.withUnsafeMutableBufferPointer { buffer in
                var destination = vImage_Buffer(
                    data: buffer.baseAddress,
                    height: vImagePixelCount(rows),
                    width: vImagePixelCount(rowLength),
                    rowBytes: rowLength * MemoryLayout<Float>.size
                )
                guard vImageConvert_Planar16FtoPlanarF(&source, &destination, 0) == kvImageNoError else {
                    throw SpeechError.unsupportedArrayType
                }
            }
            return output
        case .float32:
            let source = base.assumingMemoryBound(to: Float.self)
            var output = [Float](repeating: 0, count: count)
            output.withUnsafeMutableBufferPointer { buffer in
                guard let destination = buffer.baseAddress else { return }
                for row in 0..<rows {
                    (destination + row * rowLength).update(
                        from: source + row * rowPitch,
                        count: rowLength
                    )
                }
            }
            return output
        default:
            return nil
        }
    }

    /// Odometer step that maintains the flat offset incrementally.
    private static func advance(_ indices: inout [Int], shape: [Int], strides: [Int], offset: Int) -> Int {
        var offset = offset
        for dimension in stride(from: indices.count - 1, through: 0, by: -1) {
            indices[dimension] += 1
            offset += strides[dimension]
            if indices[dimension] < shape[dimension] { return offset }
            offset -= strides[dimension] * shape[dimension]
            indices[dimension] = 0
        }
        return offset
    }

    static func setFloat16(_ array: MLMultiArray, at index: Int, value: Float) throws {
        guard array.dataType == .float16 else {
            throw SpeechError.unsupportedArrayType
        }
        guard index >= 0, index < array.shape.map(\.intValue).reduce(1, *) else {
            throw SpeechError.invalidArrayShape
        }
        array.dataPointer.assumingMemoryBound(to: Float16.self)[index] = Float16(value)
    }

    static func setInt32(_ array: MLMultiArray, at index: Int, value: Int32) throws {
        guard array.dataType == .int32 else {
            throw SpeechError.unsupportedArrayType
        }
        guard index >= 0, index < array.shape.map(\.intValue).reduce(1, *) else {
            throw SpeechError.invalidArrayShape
        }
        array.dataPointer.assumingMemoryBound(to: Int32.self)[index] = value
    }

    /// Reinterpret `array` under a new shape **without copying**, when its
    /// memory is already C-contiguous. Used to feed the decoder's rank-3
    /// embedding straight into the joint's rank-2 `decoder_step` input: the
    /// bytes are identical, only the declared shape differs. Returns nil when
    /// the source is strided, so the caller can fall back to a real copy —
    /// Core ML outputs are not guaranteed contiguous.
    static func reshapedNoCopy(_ array: MLMultiArray, to shape: [Int]) -> MLMultiArray? {
        let sourceShape = array.shape.map(\.intValue)
        let sourceStrides = array.strides.map(\.intValue)
        let count = sourceShape.reduce(1, *)
        guard count == shape.reduce(1, *) else { return nil }

        var expected = 1
        for dimension in stride(from: sourceShape.count - 1, through: 0, by: -1) {
            if sourceStrides[dimension] != expected { return nil }
            expected *= sourceShape[dimension]
        }

        var strides = [Int](repeating: 1, count: shape.count)
        var running = 1
        for dimension in stride(from: shape.count - 1, through: 0, by: -1) {
            strides[dimension] = running
            running *= shape[dimension]
        }
        // The closure captures `array`, keeping the backing buffer alive for
        // exactly as long as the reshaped view exists.
        return try? MLMultiArray(
            dataPointer: array.dataPointer,
            shape: shape.map { NSNumber(value: $0) },
            dataType: array.dataType,
            strides: strides.map { NSNumber(value: $0) },
            deallocator: { _ in _ = array }
        )
    }

    private static func make<T>(shape: [Int], values: [T], dataType: MLMultiArrayDataType) throws -> MLMultiArray {
        guard shape.reduce(1, *) == values.count else {
            throw SpeechError.invalidArrayShape
        }
        return try MLMultiArray(shape: shape.map { NSNumber(value: $0) }, dataType: dataType)
    }
}
#endif
