import Foundation
import Observation

/*
 Provenance (exact donor revision):
 - baryhuang/claude-code-by-agents @ 7655042eb986bf146079032726a7347ac4547c0e
 - OpenAgents/Notifications/PushSink.swift (current-visible channel,
   user-tapped deep-link intent, and deferred channel consumption)
 - OpenAgents/OpenAgentsApp.swift (`.onOpenURL` and scenePhase lifecycle)

 This CodeRight-named lifecycle adapter keeps routing user-directed and
 validation deferred until a daemon session list is available. It has no
 Firebase, UIKit, or donor-module dependency; callers inject target validation
 and map scene refresh requests into the existing app lifecycle.
 */

/// Minimal target seam for validating a session before navigation.
public protocol DeepLinkSessionValidating: Sendable {
    func sessionExists(id: String) async throws -> Bool
}

@MainActor
@Observable
public final class DeepLinkLifecycle {
    public private(set) var pendingRoute: DeepLinkRoute?
    public private(set) var errorMessage: String?
    public private(set) var currentVisibleSessionID: String?
    public private(set) var pendingDeepLinkSessionID: String?
    public private(set) var isAppActive = false
    private var refreshRequested = false

    public weak var appRouter: AppRouter?

    public init(router: AppRouter? = nil) {
        appRouter = router
    }

    /// Handles both CodeRight links and OS file URLs delivered to the app.
    @discardableResult
    public func receive(_ url: URL) -> DeepLinkRoute? {
        if url.isFileURL {
            appRouter?.ingestExternalURL(url)
            return nil
        }

        guard let route = DeepLinkRouteParser.parse(url) else {
            pendingRoute = nil
            errorMessage = "That isn't a valid CodeRight link."
            return nil
        }

        errorMessage = nil
        if case .pairing = route {
            // Pairing remains owned by DaemonStore; this adapter never stores
            // or activates credentials as a side effect of URL delivery.
            pendingRoute = nil
        } else {
            pendingRoute = route
        }
        return route
    }

    @discardableResult
    public func handle(_ url: URL) -> DeepLinkRoute? {
        receive(url)
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

    /// Resolves a session route only after the active daemon confirms the ID.
    public func resolvePending(
        using validator: any DeepLinkSessionValidating
    ) async -> String? {
        guard let route = pendingRoute, case .session(let id) = route else { return nil }

        do {
            guard try await validator.sessionExists(id: id) else {
                guard pendingRoute == route else { return nil }
                pendingRoute = nil
                errorMessage = "Session " + id + " isn't available on this daemon."
                return nil
            }
            guard pendingRoute == route else { return nil }
            pendingRoute = nil
            return id
        } catch {
            guard pendingRoute == route else { return nil }
            pendingRoute = nil
            errorMessage = "Couldn't validate that session link. Check daemon connection and try again."
            return nil
        }
    }

    /// Push/notification equivalent of the donor's channel deep-link method.
    /// A workspace hint may switch the isolated AppRouter before the target
    /// session view consumes the pending ID.
    public func deepLinkToSession(_ sessionID: String, workspaceHint: String? = nil) {
        guard let id = DeepLinkRouteParser.validatedID(sessionID) else {
            errorMessage = "That session link contains an invalid session ID."
            return
        }

        if let workspaceHint,
           let entry = DeepLinkWorkspaceHistory.shared.entries()
            .first(where: { $0.workspaceId == workspaceHint }),
           let appRouter,
           case .workspace(let active) = appRouter.route,
           active.workspaceId != entry.workspaceId {
            appRouter.connect(
                workspaceId: entry.workspaceId,
                name: entry.name,
                appURL: URL(string: entry.appURL ?? ""),
                apiURL: URL(string: entry.apiURL ?? "")
            )
        }

        errorMessage = nil
        pendingDeepLinkSessionID = id
        pendingRoute = .session(id: id)
    }

    /// Marks the transcript currently visible to foreground notification logic.
    public func setCurrentVisibleSession(_ id: String?) {
        currentVisibleSessionID = id
    }

    public func consumePendingDeepLinkSession() -> String? {
        defer { pendingDeepLinkSessionID = nil }
        return pendingDeepLinkSessionID
    }

    /// Foreground notifications for the visible session are redundant.
    public func shouldSuppressForeground(sessionID: String?) -> Bool {
        guard let sessionID else { return false }
        return isAppActive && currentVisibleSessionID == sessionID
    }

    /// Mirrors donor `scenePhase == .active`: request a refresh, but leave the
    /// actual refresh operation with the target store.
    public func sceneDidBecomeActive() {
        isAppActive = true
        refreshRequested = true
    }

    public func sceneDidEnterBackground() {
        isAppActive = false
    }

    public func consumeRefreshRequest() -> Bool {
        defer { refreshRequested = false }
        return refreshRequested
    }
}
