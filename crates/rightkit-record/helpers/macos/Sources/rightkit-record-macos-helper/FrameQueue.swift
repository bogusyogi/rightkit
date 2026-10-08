import CoreMedia
import Foundation

struct QueuedFrame {
    let sample: CMSampleBuffer
    let hostNs: UInt64
}

/// A bounded hand-off from the ScreenCaptureKit callback to the encode worker. A full queue
/// refuses the frame (the caller counts a backpressure drop) instead of blocking the callback or
/// holding more of ScreenCaptureKit's fixed pool of surfaces.
final class FrameQueue {
    private let capacity: Int
    private var items = [QueuedFrame]()
    private var closed = false
    private let condition = NSCondition()

    init(capacity: Int) { self.capacity = capacity }

    /// False when the queue is full or closed.
    func push(_ frame: QueuedFrame) -> Bool {
        condition.lock()
        defer { condition.unlock() }
        if closed || items.count >= capacity { return false }
        items.append(frame)
        condition.signal()
        return true
    }

    /// Blocks for the next frame. nil once closed and drained.
    func pop() -> QueuedFrame? {
        condition.lock()
        defer { condition.unlock() }
        while items.isEmpty && !closed { condition.wait() }
        return items.isEmpty ? nil : items.removeFirst()
    }

    func close() {
        condition.lock()
        closed = true
        condition.broadcast()
        condition.unlock()
    }
}
