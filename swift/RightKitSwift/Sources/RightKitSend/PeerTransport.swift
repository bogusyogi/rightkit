#if canImport(Network) && canImport(Security) && canImport(CryptoKit)
import CryptoKit
import Foundation
import Network
import Security

/// Bonjour service type and TLS certificate pinning for peer-to-peer send (rightkit#762).
public enum PeerTransport {
    public static let serviceType = "_pulse-send._tcp"
    /// TXT key that carries the advertiser's certificate fingerprint.
    public static let fingerprintKey = "fp"

    /// Lowercase hex SHA-256 of a certificate's DER bytes.
    public static func fingerprint(of certificate: SecCertificate) -> String {
        let der = SecCertificateCopyData(certificate) as Data
        return SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    }

    /// TLS parameters that accept a peer only when its leaf certificate fingerprint is trusted.
    /// `identity` is this device's certificate and key. A listener also requires the client to present a certificate.
    public static func parameters(identity: SecIdentity, trust: PeerTrustStore, isServer: Bool) -> NWParameters {
        let tls = NWProtocolTLS.Options()
        let security = tls.securityProtocolOptions
        if let local = sec_identity_create(identity) {
            sec_protocol_options_set_local_identity(security, local)
        }
        sec_protocol_options_set_min_tls_protocol_version(security, .TLSv12)
        if isServer {
            sec_protocol_options_set_peer_authentication_required(security, true)
        }
        sec_protocol_options_set_verify_block(security, { _, secTrust, complete in
            let trustRef = sec_trust_copy_ref(secTrust).takeRetainedValue()
            guard let chain = SecTrustCopyCertificateChain(trustRef) as? [SecCertificate],
                  let leaf = chain.first else {
                complete(false)
                return
            }
            complete(trust.isTrusted(certificateSHA256: fingerprint(of: leaf)))
        }, DispatchQueue(label: "rightkit.send.verify"))
        return NWParameters(tls: tls, tcp: NWProtocolTCP.Options())
    }
}

/// Advertises this device on the local network with its fingerprint in the TXT record.
public final class PeerAdvertiser: @unchecked Sendable {
    private let listener: NWListener

    public init(name: String?, identity: SecIdentity, fingerprint: String, trust: PeerTrustStore) throws {
        let parameters = PeerTransport.parameters(identity: identity, trust: trust, isServer: true)
        listener = try NWListener(using: parameters)
        let txt = NWTXTRecord([PeerTransport.fingerprintKey: fingerprint])
        listener.service = NWListener.Service(name: name, type: PeerTransport.serviceType, txtRecord: txt)
    }

    /// Starts advertising. `onConnection` receives each accepted, pinned connection.
    public func start(queue: DispatchQueue, onConnection: @escaping @Sendable (NWConnection) -> Void) {
        listener.newConnectionHandler = { connection in
            connection.start(queue: queue)
            onConnection(connection)
        }
        listener.start(queue: queue)
    }

    public func stop() {
        listener.cancel()
    }
}

/// Browses for advertised peers and reports each with the fingerprint it advertises.
public final class PeerBrowser: @unchecked Sendable {
    private let browser: NWBrowser

    public init() {
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: PeerTransport.serviceType, domain: nil)
        browser = NWBrowser(for: descriptor, using: NWParameters())
    }

    /// Calls `onPeers` with the current peers whose TXT record carries a fingerprint. Nothing is trusted here:
    /// pass a peer to `PeerTrustStore.addResolved` to consent, and the TLS verify block still pins the certificate.
    public func start(queue: DispatchQueue, onPeers: @escaping @Sendable ([ResolvedPeer]) -> Void) {
        browser.browseResultsChangedHandler = { results, _ in
            var peers: [ResolvedPeer] = []
            for result in results {
                guard case let .service(name, _, _, _) = result.endpoint,
                      case let .bonjour(txt) = result.metadata,
                      let fingerprint = txt.dictionary[PeerTransport.fingerprintKey] else { continue }
                peers.append(ResolvedPeer(serviceName: name, certificateSHA256: fingerprint))
            }
            onPeers(peers)
        }
        browser.start(queue: queue)
    }

    public func stop() {
        browser.cancel()
    }
}

/// Opens a pinned outbound connection to a discovered endpoint.
public enum PeerConnector {
    public static func connect(
        to endpoint: NWEndpoint, identity: SecIdentity, trust: PeerTrustStore, queue: DispatchQueue
    ) -> NWConnection {
        let parameters = PeerTransport.parameters(identity: identity, trust: trust, isServer: false)
        let connection = NWConnection(to: endpoint, using: parameters)
        connection.start(queue: queue)
        return connection
    }
}
#endif
