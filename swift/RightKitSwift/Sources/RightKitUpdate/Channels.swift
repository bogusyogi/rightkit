import Foundation
import RightKitHTTP

/// Failure resolving an update channel.
public enum UpdateChannelError: Error, Equatable, Sendable {
    case noChannels
    case invalidName(String)
    case noEndpoints(channel: String)
    case invalidEndpoint(channel: String)
    /// An endpoint that is not HTTPS (plain HTTP is allowed only for a loopback host).
    case insecureEndpoint(channel: String)
    case unknownChannel(String)
    case undefinedDefault(String)
}

/// Channel name -> updater endpoints, validated like `validateChannels` in `@rightkit/app-update`:
/// names are `^[a-z][a-z0-9-]{0,31}$`, every channel has at least one endpoint, and every endpoint
/// is HTTPS (or loopback HTTP).
public struct UpdateChannels: Sendable, Equatable {
    public let endpoints: [String: [URL]]

    public init(_ channels: [String: [String]]) throws {
        guard !channels.isEmpty else { throw UpdateChannelError.noChannels }
        var parsed: [String: [URL]] = [:]
        for (name, raw) in channels {
            guard Self.validName(name) else { throw UpdateChannelError.invalidName(name) }
            guard !raw.isEmpty else { throw UpdateChannelError.noEndpoints(channel: name) }
            parsed[name] = try raw.map { text in
                guard let url = URL(string: text), url.host != nil else { throw UpdateChannelError.invalidEndpoint(channel: name) }
                guard HTTPClient.isPermitted(url, allowInsecureLoopback: true) else { throw UpdateChannelError.insecureEndpoint(channel: name) }
                return url
            }
        }
        endpoints = parsed
    }

    private static func validName(_ name: String) -> Bool {
        let scalars = Array(name.unicodeScalars)
        guard (1...32).contains(scalars.count), let first = scalars.first, first.value >= 97, first.value <= 122 else { return false }
        return scalars.allSatisfy { ($0.value >= 97 && $0.value <= 122) || ($0.value >= 48 && $0.value <= 57) || $0 == "-" }
    }

    /// The endpoints for `requested` (or `defaultChannel` when nil). An unknown channel falls back to
    /// `defaultChannel` only when `allowFallback` is true; otherwise it throws.
    public func resolve(
        requested: String?, defaultChannel: String, allowFallback: Bool = false,
    ) throws -> (channel: String, endpoints: [URL]) {
        guard let fallback = endpoints[defaultChannel] else { throw UpdateChannelError.undefinedDefault(defaultChannel) }
        let name = requested ?? defaultChannel
        if let found = endpoints[name] { return (name, found) }
        if allowFallback { return (defaultChannel, fallback) }
        throw UpdateChannelError.unknownChannel(name)
    }
}
