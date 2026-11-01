import Foundation

/*
 Provenance (exact donor revision):
 - baryhuang/claude-code-by-agents @ 7655042eb986bf146079032726a7347ac4547c0e
 - OpenAgents/State/AppRouter.swift (route ownership and deferred intent)
 - OpenAgents/State/WorkspaceHistory.swift (URL component parsing)

 This file is an isolated CodeRight adaptation. The donor has no dependency on
 CodeRight's daemon protocol; this target route keeps the same fail-closed
 component parsing and identifier validation while naming the target seams.
 Pairing remains an intent only: DaemonStore stays authoritative for credential
 validation and persistence.
 */

/// Intent accepted by CodeRight's custom URL scheme.
public enum DeepLinkRoute: Equatable, Sendable {
    /// Existing pairing flow; the daemon store validates its payload.
    case pairing
    /// Show the connected daemon's session list.
    case sessions
    /// Present the new-session sheet from the session surface.
    case newSession
    /// Open one daemon session after the active daemon validates its existence.
    case session(id: String)
}

/// Parser and validator for CodeRight's custom URL scheme.
public enum DeepLinkRouteParser {
    public static func parse(_ url: URL) -> DeepLinkRoute? {
        guard url.scheme?.lowercased() == "coderight",
              url.user == nil,
              url.port == nil,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else {
            return nil
        }

        guard let route = route(from: components) else { return nil }
        return route
    }

    /// Public validation seam for AppRoute adapters and notification payloads.
    /// Whitespace, controls, path separators, and encoded separators are rejected.
    public static func validatedID(_ raw: String?) -> String? {
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

    private static func querySessionID(from components: URLComponents) -> String? {
        let names = ["id", "session", "sessionid", "session_id"]
        for name in names {
            guard let raw = components.queryItems?.first(where: { $0.name.lowercased() == name })?.value else {
                continue
            }
            if let id = validatedID(raw) { return id }
        }
        return nil
    }

    private static func route(from components: URLComponents) -> DeepLinkRoute? {
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
    ) -> DeepLinkRoute? {
        switch host {
        case "pair":
            return parts.isEmpty && components.queryItems?.isEmpty != false ? .pairing : nil
        case "new-session", "new":
            return parts.isEmpty && components.queryItems?.isEmpty != false ? .newSession : nil
        case "sessions":
            return parts.isEmpty ? .sessions : nil
        case "session":
            guard parts.count <= 1 else { return nil }
            let id = querySessionID(from: components) ?? validatedID(parts.first)
            return id.map(DeepLinkRoute.session)
        default:
            return nil
        }
    }

    private static func pathRoute(
        parts: [String],
        components: URLComponents
    ) -> DeepLinkRoute? {
        if parts.isEmpty {
            return querySessionID(from: components).map(DeepLinkRoute.session)
        }

        let first = parts[0].lowercased()
        let hasNoQuery = components.queryItems?.isEmpty != false
        if first == "pair", parts.count == 1, hasNoQuery { return .pairing }
        if first == "sessions", parts.count == 1, hasNoQuery { return .sessions }
        if first == "new-session", parts.count == 1, hasNoQuery { return .newSession }
        guard first == "session", parts.count <= 2 else { return nil }
        let id = querySessionID(from: components)
            ?? validatedID(parts.count == 2 ? parts[1] : nil)
        return id.map(DeepLinkRoute.session)
    }
}

// MARK: - ENG-113: external-scheme classification

/// Classifies a URL encountered anywhere content is untrusted (embedded web content in
/// `BrowserControlSurfaceView`, agent tool output, a pasted link) as one CodeRight's own
/// `coderight://` deep-link route can handle directly, or as an external-scheme handoff that
/// must clear `ExternalSchemeConsentStore` before anything opens it.
///
/// This classifier only answers "does this need consent" — it never opens anything and never
/// records a decision. That separation matters: page content can hand this classifier any URL
/// it likes, but classifying a URL is not consenting to open it, and nothing here can turn
/// classification into action on its own.
public enum ExternalSchemeClassifier {
    /// `http`/`https` are handled by the browser surface itself and never need scheme consent;
    /// `coderight` is this app's own scheme and is routed by `DeepLinkRouteParser`
    /// instead. Every other scheme — `mailto`, `tel`, `sms`, a third-party app's custom scheme,
    /// `javascript` (rejected outright, never a legitimate handoff target) — requires consent.
    public static func requiresExternalConsent(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased(), !scheme.isEmpty else { return false }
        return !["http", "https", "coderight"].contains(scheme)
    }

    /// Normalized key `ExternalSchemeConsentStore` remembers a decision under. Lowercased so
    /// `Mailto:` and `mailto:` share one remembered decision, exactly as the scheme comparison
    /// above is case-insensitive.
    public static func consentKey(for url: URL) -> String? {
        guard let scheme = url.scheme?.lowercased(), !scheme.isEmpty else { return nil }
        return scheme
    }
}
