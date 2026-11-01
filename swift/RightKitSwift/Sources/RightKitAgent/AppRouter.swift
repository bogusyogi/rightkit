import Foundation
import Observation
import UniformTypeIdentifiers

/*
 Provenance (exact donor revision):
 - baryhuang/claude-code-by-agents @ 7655042eb986bf146079032726a7347ac4547c0e
 - OpenAgents/State/AppRouter.swift (complete route state, workspace connect /
   switch / disconnect lifecycle, and security-scoped URL ingestion)

 This direct port is isolated under CodeRight/Porting/DeepLinks. It deliberately
 does not replace AppRouteRouter or DaemonStore; integration maps its route
 state into those existing target-owned seams.
 */

/// Donor AppRouter route state with CodeRight-only type names.
@MainActor
@Observable
public final class AppRouter {
    public enum Route: Equatable, Sendable {
        /// Selector view, optionally retaining the workspace to return to.
        case selector(returnTo: DeepLinkWorkspaceEntry?)
        /// Connected workspace, recreated when its identity changes.
        case workspace(DeepLinkWorkspaceEntry)
    }

    public private(set) var route: Route

    /// Files handed to the app before a composer is mounted.
    public var pendingExternalAttachments: [DeepLinkPendingAttachment] = []

    public init() {
        if let current = DeepLinkWorkspaceHistory.shared.current() {
            route = .workspace(current)
        } else {
            route = .selector(returnTo: nil)
        }
    }

    /// Read an OS-delivered file URL while its security-scoped permission is live.
    public func ingestExternalURL(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer {
            if scoped { url.stopAccessingSecurityScopedResource() }
        }
        guard let data = try? Data(contentsOf: url) else { return }
        let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
            ?? "application/octet-stream"
        pendingExternalAttachments.append(
            DeepLinkPendingAttachment(
                filename: url.lastPathComponent,
                contentType: mime,
                data: data
            )
        )
    }

    public func connect(
        workspaceId: String,
        name: String? = nil,
        appURL: URL? = nil,
        apiURL: URL? = nil
    ) {
        let displayName = name ?? workspaceId
        DeepLinkWorkspaceHistory.shared.touch(
            workspaceId: workspaceId,
            name: displayName,
            appURL: appURL?.absoluteString,
            apiURL: apiURL?.absoluteString
        )
        let entry = DeepLinkWorkspaceEntry(
            workspaceId: workspaceId,
            name: displayName,
            lastUsed: Date(),
            appURL: appURL?.absoluteString,
            apiURL: apiURL?.absoluteString
        )
        DeepLinkWorkspaceHistory.shared.setCurrent(entry)
        route = .workspace(entry)
    }

    /// Enter selector switch mode while retaining the current workspace.
    public func switchWorkspace() {
        if case .workspace(let current) = route {
            route = .selector(returnTo: current)
        } else {
            route = .selector(returnTo: nil)
        }
    }

    /// Disconnect and clear the current workspace.
    public func disconnect() {
        DeepLinkWorkspaceHistory.shared.setCurrent(nil)
        route = .selector(returnTo: nil)
    }

    /// Cancel selector switch mode.
    public func returnToCurrent() {
        if case .selector(let returnTo) = route, let entry = returnTo {
            route = .workspace(entry)
        }
    }

    public var isSwitching: Bool {
        if case .selector(let returnTo) = route { return returnTo != nil }
        return false
    }
}
