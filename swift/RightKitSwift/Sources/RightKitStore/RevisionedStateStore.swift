import Foundation

/// A cross-process state snapshot with a monotonic revision, giving observers deterministic change
/// detection without timestamp guessing.
public protocol RevisionedState: Codable, Equatable, Sendable {
    var revision: UInt64 { get set }
    var updatedAt: Date { get set }
    init()
}

/// Read/write access to one `RevisionedState` in the App Group. Every `write` bumps `revision` and
/// stamps `updatedAt`. Sole-writer discipline belongs to the caller: extensions observe, one owning
/// process writes.
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
    public func write(now: Date = Date(), _ mutate: (inout State) -> Void) -> State {
        var current = snapshot ?? State()
        mutate(&current)
        current.revision &+= 1
        current.updatedAt = now
        if let data = try? JSONEncoder().encode(current) {
            configuration.defaults?.set(data, forKey: key)
        }
        return current
    }
}
