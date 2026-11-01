#if os(iOS)
import Foundation
import UIKit

// The seam between the app and the Send transport. The real engine will be the
// RightKit Swift product `RightKitSend` (LocalSend v2.2 over TLS with Pulse's
// additions); it will conform to `SendEngine`. Until it exists, `StubSendEngine`
// stands in. No networking, Keychain or Bonjour code lives behind this pass.

/// A Pulse computer (or other device) discovered on the local network.
struct SendPeer: Identifiable, Hashable, Sendable {
    /// SHA-256 of the device certificate, 64 lowercase hex characters.
    let fingerprint: String
    let alias: String
    /// desktop, mobile, web, headless, server.
    let deviceType: String
    let host: String
    let port: Int

    var id: String { fingerprint }
}

/// One item that arrived from another device.
struct ReceivedItem: Identifiable, Hashable, Sendable {
    enum Kind: Hashable, Sendable {
        case text(String)
        case link(URL)
        case file(url: URL, name: String, size: Int64, mime: String)
        /// An image file. Saved to Photos automatically only when the sender is
        /// certificate-proven and trusted; otherwise it behaves like a file.
        case picture(url: URL, name: String)
    }

    let id: UUID
    let senderAlias: String
    let senderFingerprint: String
    let kind: Kind
    let receivedAt: Date
    /// The sender was certificate-proven and trusted when this arrived. Only
    /// such items may be acted on without asking (pictures go to Photos).
    let fromTrustedProvenSender: Bool

    /// A file or picture from a sender that is not proven and trusted needs one
    /// more tap (a confirmation) before it is saved; trusted senders save directly.
    var needsSaveConfirmation: Bool { !fromTrustedProvenSender }
}

/// A transfer that needs the user's decision. Carries no sender content on
/// purpose: nothing a sender wrote (text, link, file name) may be shown before
/// Accept. A sender that is both certificate-proven and on the trusted list is
/// accepted by the engine without asking and never appears here; a matching
/// name or fingerprint alone is not proof.
struct IncomingOffer: Identifiable, Hashable, Sendable {
    let id: UUID
    let senderAlias: String
    let senderFingerprint: String
    /// The sender presented its certificate in the TLS handshake.
    let certificateProven: Bool
    /// The sender is on this phone's trusted list.
    let trusted: Bool

    var autoAccepted: Bool { certificateProven && trusted }
}

enum SendResult: Equatable, Sendable {
    case sent
    case failed(String)
}

@MainActor
protocol SendEngine: AnyObject {
    /// Emits the current peer list immediately on subscription, then again on
    /// every change.
    func peerUpdates() -> AsyncStream<[SendPeer]>
    /// Items received from other devices while the engine is running: those
    /// auto-accepted from proven, trusted senders, and those the user accepted.
    /// Contract the app relies on and cannot check itself: nothing from an
    /// unaccepted offer is ever emitted, and `fromTrustedProvenSender` is true
    /// only when the sender proved its certificate in the handshake AND is on
    /// the trusted list. A real engine must carry both as named parity tests.
    func receivedItems() -> AsyncStream<ReceivedItem>
    /// Offers waiting for Accept / Decline. Never contains an auto-accepted one.
    func incomingOffers() -> AsyncStream<IncomingOffer>
    /// Accept: the engine then delivers the content through `receivedItems()`.
    func accept(_ offer: IncomingOffer) async
    func decline(_ offer: IncomingOffer) async

    /// Begin discovery and make this device visible to others.
    func start() async
    func stop() async
    /// App went to the background: iOS gives no background listener.
    func suspend() async
    func resume() async

    /// `clipboard: true` marks a paste, which the receiving computer puts on
    /// its clipboard. `progress` reports 0...1.
    func sendText(
        to peer: SendPeer,
        text: String,
        clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult

    func sendFiles(
        to peer: SendPeer,
        urls: [URL],
        clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult
}

/// The engine every non-DEBUG build uses until `RightKitSend` exists: no peers,
/// no received items, no offers; sends fail with a plain message. It never
/// touches a network, so the UI shows no "Send to" control and an empty Receive tab.
@MainActor
final class InertSendEngine: SendEngine {
    func peerUpdates() -> AsyncStream<[SendPeer]> {
        AsyncStream { continuation in continuation.yield([]) }
    }
    func receivedItems() -> AsyncStream<ReceivedItem> { AsyncStream { _ in } }
    func incomingOffers() -> AsyncStream<IncomingOffer> { AsyncStream { _ in } }
    func accept(_ offer: IncomingOffer) async {}
    func decline(_ offer: IncomingOffer) async {}
    func start() async {}
    func stop() async {}
    func suspend() async {}
    func resume() async {}

    func sendText(
        to peer: SendPeer, text: String, clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult {
        .failed("Sending is not available yet.")
    }

    func sendFiles(
        to peer: SendPeer, urls: [URL], clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult {
        .failed("Sending is not available yet.")
    }
}

/// The engine the app launches with: the launch-argument stub in DEBUG, the
/// inert engine everywhere else.
@MainActor
func makeDefaultSendEngine() -> SendEngine {
    #if DEBUG
    return StubSendEngine()
    #else
    return InertSendEngine()
    #endif
}

#if DEBUG
/// Placeholder engine. Zero peers by default (the "away from home" case).
/// `-SendStubPeers` exposes "Mac" and "Dell"; `-SendStubReceive` emits one text,
/// one link and one file when the engine starts. Nothing here touches a network.
@MainActor
final class StubSendEngine: SendEngine {
    static let macPeer = SendPeer(
        fingerprint: String(repeating: "a1", count: 32),
        alias: "Mac", deviceType: "desktop", host: "192.0.2.10", port: 53317
    )
    static let dellPeer = SendPeer(
        fingerprint: String(repeating: "b2", count: 32),
        alias: "Dell", deviceType: "desktop", host: "192.0.2.11", port: 53317
    )

    private let stubPeers: Bool
    private let stubReceive: Bool
    private let sendDelay: Duration
    private var peerContinuations: [UUID: AsyncStream<[SendPeer]>.Continuation] = [:]
    private var itemContinuations: [UUID: AsyncStream<ReceivedItem>.Continuation] = [:]
    private var offerContinuations: [UUID: AsyncStream<IncomingOffer>.Continuation] = [:]
    private var heldContent: [UUID: [ReceivedItem.Kind]] = [:]
    private var running = false
    private var didEmitReceive = false

    init(arguments: [String] = ProcessInfo.processInfo.arguments, sendDelay: Duration = .milliseconds(600)) {
        stubPeers = arguments.contains("-SendStubPeers")
        stubReceive = arguments.contains("-SendStubReceive")
        self.sendDelay = sendDelay
    }

    private var currentPeers: [SendPeer] {
        running && stubPeers ? [Self.macPeer, Self.dellPeer] : []
    }

    func peerUpdates() -> AsyncStream<[SendPeer]> {
        let key = UUID()
        return AsyncStream { continuation in
            peerContinuations[key] = continuation
            continuation.yield(currentPeers)
            continuation.onTermination = { [weak self] _ in
                Task { @MainActor in self?.peerContinuations[key] = nil }
            }
        }
    }

    func receivedItems() -> AsyncStream<ReceivedItem> {
        let key = UUID()
        return AsyncStream { continuation in
            itemContinuations[key] = continuation
            continuation.onTermination = { [weak self] _ in
                Task { @MainActor in self?.itemContinuations[key] = nil }
            }
        }
    }

    func incomingOffers() -> AsyncStream<IncomingOffer> {
        let key = UUID()
        return AsyncStream { continuation in
            offerContinuations[key] = continuation
            continuation.onTermination = { [weak self] _ in
                Task { @MainActor in self?.offerContinuations[key] = nil }
            }
        }
    }

    func accept(_ offer: IncomingOffer) async {
        guard let kinds = heldContent.removeValue(forKey: offer.id) else { return }
        deliver(kinds, from: offer.senderAlias, fingerprint: offer.senderFingerprint, trustedProven: false)
    }

    func decline(_ offer: IncomingOffer) async {
        heldContent[offer.id] = nil
    }

    func start() async {
        running = true
        publishPeers()
        emitStubReceiveOnce()
    }

    func stop() async {
        running = false
        publishPeers()
    }

    func suspend() async {
        running = false
        publishPeers()
    }

    func resume() async {
        running = true
        publishPeers()
    }

    func sendText(
        to peer: SendPeer, text: String, clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult {
        await simulateSend(progress: progress)
    }

    func sendFiles(
        to peer: SendPeer, urls: [URL], clipboard: Bool,
        progress: @MainActor @escaping (Double) -> Void
    ) async -> SendResult {
        await simulateSend(progress: progress)
    }

    private func simulateSend(progress: @MainActor @escaping (Double) -> Void) async -> SendResult {
        progress(0.1)
        try? await Task.sleep(for: sendDelay / 2)
        progress(0.6)
        try? await Task.sleep(for: sendDelay / 2)
        progress(1)
        return .sent
    }

    private func publishPeers() {
        let peers = currentPeers
        for continuation in peerContinuations.values { continuation.yield(peers) }
    }

    private func deliver(
        _ kinds: [ReceivedItem.Kind], from alias: String, fingerprint: String, trustedProven: Bool
    ) {
        for kind in kinds {
            let item = ReceivedItem(
                id: UUID(), senderAlias: alias, senderFingerprint: fingerprint,
                kind: kind, receivedAt: Date(), fromTrustedProvenSender: trustedProven
            )
            for continuation in itemContinuations.values { continuation.yield(item) }
        }
    }

    /// From the trusted, proven Mac: a text, a link, a picture and a file. From
    /// an unproven sender: one pending offer whose content (a link and a file)
    /// is released only on accept.
    private func emitStubReceiveOnce() {
        guard stubReceive, !didEmitReceive else { return }
        didEmitReceive = true
        let mac = Self.macPeer
        let directory = ReceiveStore.receivedDirectory
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let pictureURL = directory.appendingPathComponent("\(UUID().uuidString)-photo.png")
        let image = UIGraphicsImageRenderer(size: CGSize(width: 240, height: 160)).image { context in
            UIColor.systemOrange.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 240, height: 160))
        }
        try? image.pngData()?.write(to: pictureURL, options: .atomic)

        let fileURL = directory.appendingPathComponent("\(UUID().uuidString)-report.txt")
        let body = Data("Stub file received from the Mac.\n".utf8)
        try? body.write(to: fileURL, options: .atomic)

        deliver([
            .text("Meeting moved to 3pm. Room 4B."),
            .link(URL(string: "https://example.com/articles/local-first-software")!),
            .picture(url: pictureURL, name: "photo.png"),
            .file(url: fileURL, name: "report.txt", size: Int64(body.count), mime: "text/plain"),
        ], from: mac.alias, fingerprint: mac.fingerprint, trustedProven: true)

        let untrustedFile = directory.appendingPathComponent("\(UUID().uuidString)-notes.txt")
        try? body.write(to: untrustedFile, options: .atomic)
        let offer = IncomingOffer(
            id: UUID(), senderAlias: "Guest laptop",
            senderFingerprint: String(repeating: "c3", count: 32),
            certificateProven: false, trusted: false
        )
        heldContent[offer.id] = [
            .link(URL(string: "https://example.com/guest-link")!),
            .file(url: untrustedFile, name: "notes.txt", size: Int64(body.count), mime: "text/plain"),
        ]
        for continuation in offerContinuations.values { continuation.yield(offer) }
    }
}
#endif
#endif
