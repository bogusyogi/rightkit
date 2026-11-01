import Foundation

/*
 Provenance (exact donor revision):
 - openmausbot @ 6951c40156ba0916a10b6e49e5051b7785198b93
 - ios/App/ChatView.swift:554-565, 644-703
 - ios/App/Session.swift:604-618

 `OpenMausComposerMessageSending` is the only submission seam. The donor sends
 through `Session.send` and lets the event stream remain authoritative; the
 result here lets CodeRight retain a failed draft while clearing only after a
 confirmed send.
 */

/// Outcome returned by a the app adapter after it attempts the daemon send.
public enum OpenMausComposerSendResult: Sendable, Equatable {
    case sent
    case failed(message: String)
}

/// Thin adapter around the app's session/client send operation. The port does
/// not know `SessionViewModel`, daemon payloads, nonce generation, or routes.
@MainActor
public protocol OpenMausComposerMessageSending {
    func send(text: String) async -> OpenMausComposerSendResult
}

/// Small pure submission rules object for integrations that keep their own
/// SwiftUI state. It centralizes donor trimming and empty-message behavior.
public enum OpenMausComposerSubmission {
    public static func trimmedText(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public static func canSend(_ text: String, isBusy: Bool = false) -> Bool {
        !isBusy && !trimmedText(text).isEmpty
    }
}
