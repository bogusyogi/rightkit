import Foundation

/// A cross-process state snapshot with a monotonic revision, giving observers deterministic change
/// detection without timestamp guessing.
public protocol RevisionedState: Codable, Equatable, Sendable {
    var revision: UInt64 { get set }
    var updatedAt: Date { get set }
    init()
}

/// Read/write access to one `RevisionedState` in the App Group. Every `write` bumps `revision` and
/// stamps `updatedAt`, under the cross-process lock, and throws unless the stored bytes read back.
/// The revision therefore never advances for a write that did not persist.
public struct RevisionedStateStore<State: RevisionedState>: @unchecked Sendable {
    private let configuration: AppGroupConfiguration
    private let key: String

    public init(configuration: AppGroupConfiguration, key: String) {
        self.configuration = configuration
        self.key = key
    }

    public var snapshot: State? {
        guard let data = configuration.defaults?.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(State.self, from: data)
    }

    @discardableResult
    public func write(now: Date = Date(), _ mutate: (inout State) -> Void) throws -> State {
        try configuration.locked {
            var current = snapshot ?? State()
            mutate(&current)
            current.revision &+= 1
            current.updatedAt = now
            try configuration.durableSet(try JSONEncoder().encode(current), forKey: key)
            return current
        }
    }
}
