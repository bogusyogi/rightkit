import Foundation
import Network
import Observation

/// One candidate daemon advertised on the local network. Discovery only — this app never
/// auto-connects to a candidate. The user still confirms via `PairingView`, matching the
/// existing manual/QR-first pairing philosophy: this service only shortens "type an IP" to
/// "tap a name," it never substitutes for the bearer-token handshake.
public struct NearbyDaemonCandidate: Identifiable, Equatable, Sendable {
    public var id: String { "\(name)|\(host ?? "")|\(port ?? 0)" }
    public let name: String
    public let host: String?
    public let port: Int?

    public init(name: String, host: String?, port: Int?) {
        self.name = name
        self.host = host
        self.port = port
    }
}

/// Bonjour-based candidate discovery for the "Connected Computer" pairing flow (Atom 4).
///
/// Browses for `_coderight._tcp` services on the local network using `NWBrowser`. Publishes
/// discovered candidates for the picker UI to list; resolution to a host/port happens lazily
/// (only when a candidate is found) so the browse itself stays cheap. Never opens a connection
/// to a candidate and never persists one — that remains `DaemonStore.add`, gated on the user
/// tapping "Pair" and confirming the token in `PairingView`.
@MainActor
@Observable
public final class NearbyDiscoveryService {
    /// Service type this app's desktop daemon should (eventually) advertise. Kept as a
    /// constant so a future daemon-side Bonjour publisher and this browser agree on the name.
    public static let serviceType = "_coderight._tcp"

    public private(set) var candidates: [NearbyDaemonCandidate] = []
    public private(set) var isBrowsing: Bool = false
    public private(set) var lastError: String?

    private var browser: NWBrowser?
    private var resolvers: [String: NWConnection] = [:]

    public init() {}

    /// Start browsing. Safe to call repeatedly; a running browse is left alone.
    public func start() {
        guard browser == nil else { return }
        lastError = nil
        candidates = []

        let descriptor = NWBrowser.Descriptor.bonjour(type: Self.serviceType, domain: nil)
        let parameters = NWParameters()
        parameters.includePeerToPeer = true
        let browser = NWBrowser(for: descriptor, using: parameters)
        self.browser = browser
        isBrowsing = true

        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                guard let self else { return }
                switch state {
                case let .failed(error):
                    self.lastError = error.localizedDescription
                    self.isBrowsing = false
                case .cancelled:
                    self.isBrowsing = false
                default:
                    break
                }
            }
        }

        browser.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor [weak self] in
                self?.apply(results: results)
            }
        }

        browser.start(queue: .main)
    }

    /// Stop browsing and drop all discovered candidates. Called when the picker sheet closes
    /// so a stale candidate list never lingers into a later pairing attempt.
    public func stop() {
        browser?.cancel()
        browser = nil
        for (_, connection) in resolvers { connection.cancel() }
        resolvers.removeAll()
        isBrowsing = false
        candidates = []
    }

    private func apply(results: Set<NWBrowser.Result>) {
        // A Bonjour `NWBrowser.Result.endpoint` is `.service(name:type:domain:interface:)`.
        // Resolving that to a concrete host/port requires opening an `NWConnection` per
        // candidate, which this discovery-only service deliberately does not do — publishing
        // the service name is enough for the user to recognize their own computer in the
        // picker, and the actual connection still goes through the confirmed pairing flow
        // (manual base URL + token, or a scanned QR payload) exactly as before.
        var next: [NearbyDaemonCandidate] = []
        for result in results {
            guard case let .service(name, _, _, _) = result.endpoint else { continue }
            next.append(NearbyDaemonCandidate(name: name, host: nil, port: nil))
        }
        candidates = next.sorted { $0.name < $1.name }
    }
}
