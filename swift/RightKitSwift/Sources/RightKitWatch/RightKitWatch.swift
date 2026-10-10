import Foundation

#if os(macOS)
import CoreServices

/// A position in a volume's FSEvents history. Persist the ID of the last event you *processed* and
/// pass it as `sinceWhen` on the next launch to replay what happened while the app was not running.
///
/// IDs are meaningful per volume and only while that volume's event journal is intact: replay
/// across a reformat, a volume swap, or a wrapped counter reports ``RightKitWatchEvent/mustRescan``.
public struct RightKitWatchEventID: RawRepresentable, Hashable, Codable, Comparable, Sendable {
    public let rawValue: UInt64
    public init(rawValue: UInt64) { self.rawValue = rawValue }

    /// Only events from now on (`kFSEventStreamEventIdSinceNow`).
    public static let sinceNow = RightKitWatchEventID(rawValue: UInt64(kFSEventStreamEventIdSinceNow))

    /// The system's current event ID (`FSEventsGetCurrentEventId`): a bookmark to persist before the
    /// first watch so a later replay starts exactly there.
    public static func current() -> RightKitWatchEventID { RightKitWatchEventID(rawValue: FSEventsGetCurrentEventId()) }

    public static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

/// What happened to one path. FSEvents coalesces, so one event often carries several kinds
/// (a freshly written file reports `created` and `modified` together).
public struct RightKitWatchItemKind: OptionSet, Hashable, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let created = Self(rawValue: UInt32(kFSEventStreamEventFlagItemCreated))
    public static let removed = Self(rawValue: UInt32(kFSEventStreamEventFlagItemRemoved))
    public static let renamed = Self(rawValue: UInt32(kFSEventStreamEventFlagItemRenamed))
    public static let modified = Self(rawValue: UInt32(kFSEventStreamEventFlagItemModified))
    public static let inodeMetadataModified = Self(rawValue: UInt32(kFSEventStreamEventFlagItemInodeMetaMod))
    public static let finderInfoModified = Self(rawValue: UInt32(kFSEventStreamEventFlagItemFinderInfoMod))
    public static let ownerChanged = Self(rawValue: UInt32(kFSEventStreamEventFlagItemChangeOwner))
    public static let extendedAttributesModified = Self(rawValue: UInt32(kFSEventStreamEventFlagItemXattrMod))
    public static let cloned = Self(rawValue: UInt32(kFSEventStreamEventFlagItemCloned))
    public static let mounted = Self(rawValue: UInt32(kFSEventStreamEventFlagMount))
    public static let unmounted = Self(rawValue: UInt32(kFSEventStreamEventFlagUnmount))
    public static let isFile = Self(rawValue: UInt32(kFSEventStreamEventFlagItemIsFile))
    public static let isDirectory = Self(rawValue: UInt32(kFSEventStreamEventFlagItemIsDir))
    public static let isSymlink = Self(rawValue: UInt32(kFSEventStreamEventFlagItemIsSymlink))
    public static let isHardlink = Self(rawValue: UInt32(kFSEventStreamEventFlagItemIsHardlink))
    public static let isLastHardlink = Self(rawValue: UInt32(kFSEventStreamEventFlagItemIsLastHardlink))

    /// Flags that describe a change, as opposed to the item's type.
    static let changes: Self = [
        .created, .removed, .renamed, .modified, .inodeMetadataModified, .finderInfoModified,
        .ownerChanged, .extendedAttributesModified, .cloned, .mounted, .unmounted,
    ]
}

/// Why the watcher can no longer vouch for incremental events under a path.
public struct RightKitWatchRescanReason: OptionSet, Hashable, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    /// Events coalesced at directory level: rescan the subtree (`MustScanSubDirs`).
    public static let subdirectories = Self(rawValue: UInt32(kFSEventStreamEventFlagMustScanSubDirs))
    /// This process was too slow and the stream dropped events (`UserDropped`).
    public static let userDropped = Self(rawValue: UInt32(kFSEventStreamEventFlagUserDropped))
    /// The kernel dropped events (`KernelDropped`).
    public static let kernelDropped = Self(rawValue: UInt32(kFSEventStreamEventFlagKernelDropped))
    /// The event ID counter wrapped; saved IDs are no longer comparable (`EventIdsWrapped`).
    public static let eventIDsWrapped = Self(rawValue: UInt32(kFSEventStreamEventFlagEventIdsWrapped))
}

/// One typed notification from a ``RightKitWatcher``.
public enum RightKitWatchEvent: Sendable, Equatable {
    /// A change to `path` (a file, directory, or link under a watched root).
    case item(path: String, kind: RightKitWatchItemKind, eventID: RightKitWatchEventID)
    /// A watched root itself was renamed, moved, or deleted (`RootChanged`). `path` is the root as
    /// it was watched. The watcher keeps running but no longer tracks that location. FSEvents gives
    /// this record no event ID (it reports 0), so none is exposed; do not persist one for it.
    case rootChanged(path: String)
    /// Replay of events since `sinceWhen` has finished; everything after this is live.
    case historyDone
    /// Incremental events under `path` are incomplete; rescan it.
    case mustRescan(path: String, reason: RightKitWatchRescanReason, eventID: RightKitWatchEventID)

    /// The event's ID, when it has one (`historyDone` and `rootChanged` carry none).
    public var eventID: RightKitWatchEventID? {
        switch self {
        case let .item(_, _, id), let .mustRescan(_, _, id): return id
        case .historyDone, .rootChanged: return nil
        }
    }

    /// Translates one raw FSEvents record. Control flags become their own events; any remaining
    /// change or type flags become an `.item`. Pure, so the mapping is testable without a stream.
    static func decode(path: String, flags: UInt32, id: UInt64) -> [RightKitWatchEvent] {
        let eventID = RightKitWatchEventID(rawValue: id)
        var events: [RightKitWatchEvent] = []
        if flags & UInt32(kFSEventStreamEventFlagHistoryDone) != 0 { events.append(.historyDone) }
        if flags & UInt32(kFSEventStreamEventFlagRootChanged) != 0 {
            events.append(.rootChanged(path: path))
        }
        let reason = RightKitWatchRescanReason(rawValue: flags).intersection(
            [.subdirectories, .userDropped, .kernelDropped, .eventIDsWrapped])
        if !reason.isEmpty { events.append(.mustRescan(path: path, reason: reason, eventID: eventID)) }
        let kind = RightKitWatchItemKind(rawValue: flags)
        // A pure control record (HistoryDone / RootChanged / rescan) has no item of its own; anything
        // else, including a record with no recognised flags, is reported so no change is swallowed.
        if events.isEmpty || !kind.isDisjoint(with: .changes) {
            events.append(.item(path: path, kind: kind, eventID: eventID))
        }
        return events
    }
}

public enum RightKitWatchError: Error, Equatable, Sendable {
    /// No paths, or a path that is empty.
    case invalidConfiguration
    /// `FSEventStreamCreate` returned no stream.
    case streamCreationFailed
    /// `FSEventStreamStart` refused to start the stream.
    case startFailed
}

/// Everything a watcher needs. Defaults suit an app that wants prompt, coalesced file events.
public struct RightKitWatchConfiguration: Sendable, Equatable {
    /// Directories or files to watch. Directories are watched recursively.
    public var paths: [String]
    /// Replay start. ``RightKitWatchEventID/sinceNow`` for live-only.
    public var sinceWhen: RightKitWatchEventID
    /// Seconds FSEvents coalesces events before delivery.
    public var latency: TimeInterval
    /// Report `rootChanged` when a watched root is renamed or removed (`WatchRoot`). On by default,
    /// because without it that case is silent.
    public var watchRoot: Bool
    /// Deliver the first event of a burst immediately (`NoDefer`).
    public var noDefer: Bool
    /// Suppress events caused by this process (`IgnoreSelf`).
    public var ignoreSelf: Bool

    public init(
        paths: [String],
        sinceWhen: RightKitWatchEventID = .sinceNow,
        latency: TimeInterval = 0.5,
        watchRoot: Bool = true,
        noDefer: Bool = false,
        ignoreSelf: Bool = false
    ) {
        self.paths = paths; self.sinceWhen = sinceWhen; self.latency = latency
        self.watchRoot = watchRoot; self.noDefer = noDefer; self.ignoreSelf = ignoreSelf
    }

    var streamFlags: FSEventStreamCreateFlags {
        var flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes)
        if watchRoot { flags |= FSEventStreamCreateFlags(kFSEventStreamCreateFlagWatchRoot) }
        if noDefer { flags |= FSEventStreamCreateFlags(kFSEventStreamCreateFlagNoDefer) }
        if ignoreSelf { flags |= FSEventStreamCreateFlags(kFSEventStreamCreateFlagIgnoreSelf) }
        return flags
    }
}

/// File-system watcher over FSEvents delivering ``RightKitWatchEvent`` values as an `AsyncStream`.
///
/// ```swift
/// let watcher = try RightKitWatcher(.init(paths: [dir], sinceWhen: savedID ?? .sinceNow))
/// for await event in watcher.events {
///     handle(event)
///     if let id = event.eventID { savedID = id }   // persist what you have processed
/// }
/// ```
///
/// The stream is live from `init`; events are buffered until iterated. Iteration ends when the
/// watcher is stopped. Cancelling or abandoning the iteration also stops the watcher. A running
/// watcher keeps itself alive, so it need not be retained by the caller. Not available on iOS.
public final class RightKitWatcher: Sendable {
    /// Typed events in delivery order. Consume from one task.
    public let events: AsyncStream<RightKitWatchEvent>
    private let engine: Engine

    public init(_ configuration: RightKitWatchConfiguration) throws {
        guard !configuration.paths.isEmpty, !configuration.paths.contains(where: \.isEmpty) else {
            throw RightKitWatchError.invalidConfiguration
        }
        let (stream, continuation) = AsyncStream<RightKitWatchEvent>.makeStream(bufferingPolicy: .unbounded)
        let engine = Engine(continuation: continuation, since: configuration.sinceWhen)
        try engine.start(configuration)
        continuation.onTermination = { _ in engine.stop() }
        self.events = stream
        self.engine = engine
    }

    /// The ID of the newest event this watcher has handed to ``events``. Persist the ID of events
    /// you have *handled* (``RightKitWatchEvent/eventID``) for exact resume; this value is the
    /// delivery high-water mark and is `sinceWhen` until the first event arrives.
    public var lastEventId: RightKitWatchEventID { engine.lastEventID }

    /// Blocks until every event the system has already seen has been delivered to ``events``.
    public func flush() { engine.flush() }

    /// Stops watching and finishes ``events``. Idempotent.
    public func stop() { engine.stop() }
}

extension RightKitWatcher {
    /// Owns the FSEventStream. All mutable state sits behind `lock`; the callback runs on `queue`.
    final class Engine: @unchecked Sendable {
        private let lock = NSLock()
        private let queue = DispatchQueue(label: "org.rightkit.watch")
        private let continuation: AsyncStream<RightKitWatchEvent>.Continuation
        private var stream: FSEventStreamRef?
        private var latest: UInt64

        init(continuation: AsyncStream<RightKitWatchEvent>.Continuation, since: RightKitWatchEventID) {
            self.continuation = continuation
            // "Since now" is a sentinel, not a position; start the high-water mark at the real now.
            self.latest = since == .sinceNow ? FSEventsGetCurrentEventId() : since.rawValue
        }

        var lastEventID: RightKitWatchEventID {
            lock.lock(); defer { lock.unlock() }
            return RightKitWatchEventID(rawValue: latest)
        }

        func start(_ configuration: RightKitWatchConfiguration) throws {
            // The stream's context retains the engine until the stream is released, so a late
            // callback can never touch freed memory.
            var context = FSEventStreamContext(
                version: 0,
                info: Unmanaged.passUnretained(self).toOpaque(),
                retain: { info in
                    guard let info else { return nil }
                    _ = Unmanaged<Engine>.fromOpaque(info).retain()
                    return UnsafeRawPointer(info)
                },
                release: { info in
                    guard let info else { return }
                    Unmanaged<Engine>.fromOpaque(info).release()
                },
                copyDescription: nil
            )
            let callback: FSEventStreamCallback = { _, info, count, eventPaths, eventFlags, eventIds in
                guard let info else { return }
                let engine = Unmanaged<Engine>.fromOpaque(info).takeUnretainedValue()
                let paths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue() as? [String] ?? []
                engine.deliver(
                    (0..<min(count, paths.count)).map { (paths[$0], UInt32(eventFlags[$0]), UInt64(eventIds[$0])) }
                )
            }
            guard let created = FSEventStreamCreate(
                kCFAllocatorDefault, callback, &context, configuration.paths as CFArray,
                FSEventStreamEventId(configuration.sinceWhen.rawValue),
                max(0, configuration.latency), configuration.streamFlags
            ) else { throw RightKitWatchError.streamCreationFailed }

            FSEventStreamSetDispatchQueue(created, queue)
            guard FSEventStreamStart(created) else {
                FSEventStreamInvalidate(created)
                FSEventStreamRelease(created)
                throw RightKitWatchError.startFailed
            }
            lock.lock(); stream = created; lock.unlock()
        }

        func deliver(_ records: [(path: String, flags: UInt32, id: UInt64)]) {
            for record in records {
                for event in RightKitWatchEvent.decode(path: record.path, flags: record.flags, id: record.id) {
                    if let id = event.eventID?.rawValue {
                        lock.lock(); latest = max(latest, id); lock.unlock()
                    }
                    continuation.yield(event)
                }
            }
        }

        func flush() {
            lock.lock(); let current = stream; lock.unlock()
            if let current { FSEventStreamFlushSync(current) }
        }

        func stop() {
            lock.lock()
            let current = stream
            stream = nil
            lock.unlock()
            if let current {
                FSEventStreamStop(current)
                FSEventStreamInvalidate(current)
                FSEventStreamRelease(current)
            }
            continuation.finish()
        }
    }
}
#endif
