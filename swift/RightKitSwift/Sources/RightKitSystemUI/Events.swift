import RightKitEngineClient
import Foundation
import RightKitHub

/// Live engine events for the sections: a bounded long-poll over `pulse.events.poll`
/// (`engine/src/events.rs`).
///
/// Why a poll: RightKit's push convention (`events.subscribe`) is not published, and the engine's C
/// ABI has no cancel entry point, so a call that never ends could not be stopped from Swift. The
/// engine keeps its recent events in a bounded log; one poll asks for what is newer than the last
/// number seen and waits at most ``waitMs`` for it. Cancelling the consuming task detaches the wait
/// (`SupervisedEngine` throws `CancellationError` at once), and the engine call ends on its own
/// within ``waitMs``. When RightKit publishes `events.subscribe`, `stream(engine:topics:)` switches
/// to `EngineClient.subscribe` and nothing that consumes it changes.
///
/// What a consumer sees:
/// - The first event is always `pulse.resync`, yielded once the stream is live (the cursor is set
///   at the engine's current position, so older events are never replayed). A section reads its
///   state then, so nothing that happened before the cursor is missed and nothing after it is.
/// - Further `pulse.resync` events mean events may have been missed (the consumer fell behind the
///   engine's log, or the engine restarted): read the state again.
/// - Every other event is one engine event, in order, with its topic and JSON payload.
enum Events {
    /// The synthetic topic of "read your state again".
    static let resyncTopic = "pulse.resync"

    /// How long the engine may hold one poll when nothing happens, in milliseconds (its cap).
    static let waitMs = 25_000

    /// A poll that answers with nothing much faster than this was not held by the engine; the stream
    /// waits `idlePause` before asking again so it can never spin.
    private static let heldAtLeast = Duration.milliseconds(500)
    private static let idlePause = Duration.seconds(1)
    private static let backoffStart = Duration.seconds(1)
    private static let backoffCap = Duration.seconds(10)

    /// The engine's events on `topics` (all topics when empty). Ends when the consuming task is
    /// cancelled. One poll is in flight per stream at any time, never two.
    static func stream(engine: any EngineClient, topics: [String]) -> AsyncStream<EngineEvent> {
        AsyncStream { continuation in
            let task = Task {
                await pump(engine: engine, topics: topics, continuation: continuation)
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private static func pump(
        engine: any EngineClient,
        topics: [String],
        continuation: AsyncStream<EngineEvent>.Continuation
    ) async {
        var cursor: UInt64?
        var backoff = backoffStart
        let clock = ContinuousClock()
        while !Task.isCancelled {
            do {
                guard let after = cursor else {
                    // Where the engine is now. Its answer to a cursor of 0 is "dropped" with no
                    // events and the newest number, which is all this needs.
                    let head = try await poll(engine, after: 0, topics: topics, waitMs: 0, timeout: .seconds(10))
                    cursor = head.next
                    backoff = backoffStart
                    continuation.yield(resync())
                    continue
                }
                let began = clock.now
                let reply = try await poll(engine, after: after, topics: topics, waitMs: waitMs, timeout: .seconds(40))
                backoff = backoffStart
                cursor = reply.next
                if reply.dropped {
                    continuation.yield(resync())
                } else {
                    for item in reply.events {
                        continuation.yield(EngineEvent(topic: item.topic, payload: item.payload))
                    }
                }
                if reply.events.isEmpty, !reply.dropped, clock.now - began < heldAtLeast {
                    try await Task.sleep(for: idlePause)
                }
            } catch is CancellationError {
                return
            } catch {
                if Task.isCancelled { return }
                // The engine is booting, restarting or failed this call: wait, then try again. The
                // cursor stays; the engine reports it as dropped when it is no longer valid.
                try? await Task.sleep(for: backoff)
                backoff = min(backoff * 2, backoffCap)
            }
        }
    }

    private static func resync() -> EngineEvent {
        EngineEvent(topic: resyncTopic, payload: Data("{}".utf8))
    }

    private struct PollParams: Encodable {
        let after: UInt64
        let topics: [String]
        let waitMs: Int
    }

    private struct Polled {
        let topic: String
        let payload: Data
    }

    private struct Reply {
        let events: [Polled]
        let next: UInt64
        let dropped: Bool
    }

    private struct Wire: Decodable {
        struct Item: Decodable {
            let topic: String
            let payload: JSONValue
        }

        let events: [Item]
        let next: UInt64
        let dropped: Bool
    }

    private static func poll(
        _ engine: any EngineClient,
        after: UInt64,
        topics: [String],
        waitMs: Int,
        timeout: Duration
    ) async throws -> Reply {
        let wire = try await engine.call(
            "pulse.events.poll",
            PollParams(after: after, topics: topics, waitMs: waitMs),
            as: Wire.self,
            timeout: timeout
        )
        let encoder = JSONEncoder()
        let events = try wire.events.map { Polled(topic: $0.topic, payload: try encoder.encode($0.payload)) }
        return Reply(events: events, next: wire.next, dropped: wire.dropped)
    }
}

/// Any JSON value, so an event's payload can be carried as the bytes it came in without knowing its shape.
private enum JSONValue: Codable, Sendable {
    case null
    case bool(Bool)
    case int(Int64)
    case uint(UInt64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: any Decoder) throws {
        let single = try decoder.singleValueContainer()
        if single.decodeNil() {
            self = .null
        } else if let value = try? single.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? single.decode(Int64.self) {
            self = .int(value)
        } else if let value = try? single.decode(UInt64.self) {
            self = .uint(value)
        } else if let value = try? single.decode(Double.self) {
            self = .double(value)
        } else if let value = try? single.decode(String.self) {
            self = .string(value)
        } else if let value = try? single.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try single.decode([String: JSONValue].self))
        }
    }

    func encode(to encoder: any Encoder) throws {
        var single = encoder.singleValueContainer()
        switch self {
        case .null: try single.encodeNil()
        case .bool(let value): try single.encode(value)
        case .int(let value): try single.encode(value)
        case .uint(let value): try single.encode(value)
        case .double(let value): try single.encode(value)
        case .string(let value): try single.encode(value)
        case .array(let value): try single.encode(value)
        case .object(let value): try single.encode(value)
        }
    }
}
