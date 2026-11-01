import Foundation
import Observation

/// Routes accepted by the `coderight` URL scheme.
public enum AppRoute: Equatable, Sendable {
    case pairing
    case sessions
    case newSession
    case session(id: String)
    /// Opens a local file in the editor surface without changing session route payloads.
    case codeEditor(fileURL: URL)

    public init?(url: URL) {
        guard let route = DeepLinkRouteParser.parse(url) else { return nil }
        switch route {
        case .pairing: self = .pairing
        case .sessions: self = .sessions
        case .newSession: self = .newSession
        case let .session(id): self = .session(id: id)
        }
    }

    public static func parse(_ url: URL) -> AppRoute? {
        AppRoute(url: url)
    }

    private static func querySessionID(from components: URLComponents) -> String? {
        let names = ["id", "session", "sessionid", "session_id"]
        for name in names {
            guard let raw = components.queryItems?.first(where: { $0.name.lowercased() == name })?.value else {
                continue
            }
            if let id = validID(raw) { return id }
        }
        return nil
    }

    private static func route(from components: URLComponents) -> AppRoute? {
        let parts = components.path
            .split(separator: "/", omittingEmptySubsequences: true)
            .map(String.init)
        if let host = components.host?.lowercased(), !host.isEmpty {
            return hostRoute(host, parts: parts, components: components)
        }
        return pathRoute(parts: parts, components: components)
    }

    private static func hostRoute(
        _ host: String,
        parts: [String],
        components: URLComponents
    ) -> AppRoute? {
        switch host {
        case "new-session", "new":
            return parts.isEmpty && components.queryItems?.isEmpty != false ? .newSession : nil
        case "sessions":
            return parts.isEmpty ? .sessions : nil
        case "session":
            guard parts.count <= 1 else { return nil }
            let id = querySessionID(from: components) ?? validID(parts.first)
            return id.map(AppRoute.session)
        default:
            return nil
        }
    }

    private static func pathRoute(parts: [String], components: URLComponents) -> AppRoute? {
        if parts.isEmpty {
            return querySessionID(from: components).map(AppRoute.session)
        }
        let first = parts[0].lowercased()
        let hasNoQuery = components.queryItems?.isEmpty != false
        if first == "sessions", parts.count == 1, hasNoQuery { return .sessions }
        if first == "new-session", parts.count == 1, hasNoQuery { return .newSession }
        guard first == "session", parts.count <= 2 else { return nil }
        let id = querySessionID(from: components) ?? validID(parts.count == 2 ? parts[1] : nil)
        return id.map(AppRoute.session)
    }

    private static func validID(_ raw: String?) -> String? {
        guard let raw, let decoded = raw.removingPercentEncoding else { return nil }
        let id = decoded.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !id.isEmpty,
              id == decoded,
              id.rangeOfCharacter(from: .controlCharacters) == nil,
              !id.contains("/"),
              !id.contains("\\")
        else {
            return nil
        }
        return id
    }
}

/// Holds URL intent until the active daemon can validate a target session.
@MainActor
@Observable
public final class AppRouteRouter {
    public private(set) var pendingRoute: AppRoute?
    public private(set) var errorMessage: String?

    public init() {}

    @discardableResult
    public func handle(_ url: URL) -> AppRoute? {
        guard let route = AppRoute(url: url) else {
            pendingRoute = nil
            errorMessage = "That isn't a valid CodeRight link."
            return nil
        }

        errorMessage = nil
        pendingRoute = route
        return route
    }

    @discardableResult
    public func receive(_ url: URL) -> AppRoute? {
        handle(url)
    }

    public func clearPendingRoute() {
        pendingRoute = nil
    }

    public func clearError() {
        errorMessage = nil
    }

    public func reportError(_ message: String) {
        errorMessage = message
    }

    /// Returns a target only after the current daemon's session list confirms it exists.
    public func resolvePending(using client: any DaemonClienting) async -> String? {
        guard let route = pendingRoute, case .session(let id) = route else { return nil }

        do {
            let sessions = try await client.listSessions()
            guard pendingRoute == route else { return nil }
            guard sessions.contains(where: { $0.id == id }) else {
                pendingRoute = nil
                errorMessage = "Session \(id) isn't available on this daemon."
                return nil
            }
            pendingRoute = nil
            return id
        } catch {
            guard pendingRoute == route else { return nil }
            pendingRoute = nil
            errorMessage = "Couldn't validate that session link. Check daemon connection and try again."
            return nil
        }
    }
}
