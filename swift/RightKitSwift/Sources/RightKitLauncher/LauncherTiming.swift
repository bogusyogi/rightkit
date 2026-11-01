import Foundation
import os

/// Where the launcher's time goes: three intervals the owner feels, each with a signpost
/// (Instruments, subsystem dev.orthic.pulse, category launcher) and a small rolling sample
/// list that the CI bench reads and a slow-event log line.
///
/// - `hotkeyToVisible`: the chord is seen (the event tap's thread, or Carbon's handler) until
///   the panel has been ordered in (`makeKeyAndOrderFront` returned): Pulse's own share of
///   getting the panel on screen. WindowServer's compositing after that is not in it.
/// - `keystrokeToRows`: the query text changes until the model holds the new rows. Synchronous
///   work only: apps, answers, commands and every other in-memory source.
/// - `keystrokeToView`: the same keystroke until SwiftUI has drawn those rows.
/// - `keystrokeToFiles`: the keystroke that started a file search until its rows landed.
///
/// Recording never blocks: one lock around an array append.
enum LauncherTiming {
    enum Stage: String, CaseIterable {
        case hotkeyToVisible, keystrokeToRows, keystrokeToView, keystrokeToFiles

        /// A sample above this is logged as slow.
        var budgetMs: Double {
            switch self {
            case .hotkeyToVisible: return 50
            case .keystrokeToRows: return 16
            case .keystrokeToView: return 33
            case .keystrokeToFiles: return 400
            }
        }
    }

    struct Summary: Equatable {
        let count: Int
        let p50: Double
        let p95: Double
        let max: Double
    }

    private static let signposter = OSSignposter(subsystem: "dev.orthic.pulse", category: "launcher")
    private static let log = Logger(subsystem: "dev.orthic.pulse", category: "launcher")
    private static let lock = NSLock()
    private static var samples: [Stage: [Double]] = [:]
    private static var hotkeyStamp: UInt64 = 0
    private static let keep = 512

    /// Monotonic nanoseconds.
    static func now() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

    static func milliseconds(from start: UInt64, to end: UInt64 = now()) -> Double {
        end >= start ? Double(end - start) / 1_000_000 : 0
    }

    /// Called on whatever thread sees the chord first, before the hop to the main thread.
    static func markHotkey() {
        lock.lock(); hotkeyStamp = now(); lock.unlock()
        signposter.emitEvent("hotkey")
    }

    /// The stamp `markHotkey` left, once, when it is under a second old; zero when the panel
    /// was opened another way.
    static func takeHotkey() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        let stamp = hotkeyStamp
        hotkeyStamp = 0
        guard stamp != 0, milliseconds(from: stamp) < 1_000 else { return 0 }
        return stamp
    }

    static func record(_ stage: Stage, since start: UInt64, to end: UInt64 = now()) {
        let ms = milliseconds(from: start, to: end)
        lock.lock()
        var list = samples[stage, default: []]
        list.append(ms)
        if list.count > keep { list.removeFirst(list.count - keep) }
        samples[stage] = list
        lock.unlock()
        if ms > stage.budgetMs {
            log.notice("launcher.slow stage=\(stage.rawValue, privacy: .public) ms=\(ms, format: .fixed(precision: 1))")
        }
    }

    static func beginKeystroke() -> OSSignpostIntervalState {
        signposter.beginInterval("keystroke", id: signposter.makeSignpostID())
    }

    static func endKeystroke(_ state: OSSignpostIntervalState) {
        signposter.endInterval("keystroke", state)
    }

    static func beginShow() -> OSSignpostIntervalState {
        signposter.beginInterval("show", id: signposter.makeSignpostID())
    }

    static func endShow(_ state: OSSignpostIntervalState) {
        signposter.endInterval("show", state)
    }

    static func summary(_ stage: Stage) -> Summary? {
        lock.lock()
        let list = samples[stage] ?? []
        lock.unlock()
        return summarize(list)
    }

    static func summarize(_ list: [Double]) -> Summary? {
        guard !list.isEmpty else { return nil }
        let sorted = list.sorted()
        func percentile(_ p: Double) -> Double {
            let rank = Int((Double(sorted.count) * p).rounded(.up)) - 1
            return sorted[Swift.min(sorted.count - 1, Swift.max(0, rank))]
        }
        return Summary(count: sorted.count, p50: percentile(0.5), p95: percentile(0.95), max: sorted[sorted.count - 1])
    }

    static func reset() {
        lock.lock(); samples = [:]; lock.unlock()
    }
}
