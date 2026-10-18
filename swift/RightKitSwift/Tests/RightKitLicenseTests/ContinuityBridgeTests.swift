import Foundation
import Testing
@testable import RightKitLicense

/// Injected fake transport (#604): no URLProtocol, no network.
private final class FakeBridgeTransport: ContinuityBridgeTransport, @unchecked Sendable {
    struct Recorded { var url: URL; var method: String; var body: [String: String]; var headers: [String: String]; var timeout: TimeInterval }
    private let lock = NSLock()
    private var handler: (URLRequest) throws -> (Int, Data)
    private(set) var recorded: [Recorded] = []

    init(_ handler: @escaping (URLRequest) throws -> (Int, Data)) { self.handler = handler }

    func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        let body = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]) ?? [:]
        lock.withLock {
            recorded.append(Recorded(url: request.url!, method: request.httpMethod ?? "", body: body,
                                     headers: request.allHTTPHeaderFields ?? [:], timeout: request.timeoutInterval))
        }
        let (status, data) = try handler(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
        return (data, response)
    }
}

private func stubClient(_ transport: FakeBridgeTransport) -> ContinuityBridgeClient {
    ContinuityBridgeClient(
        app: "scraperight", baseURL: URL(string: "https://example.test/v1/licenses")!, appVersion: "9.9",
        transport: transport, platform: { "ios" }
    )
}

private func json(_ text: String) -> Data { Data(text.utf8) }

private let leaseJSON = """
{"leaseId":"l1","deviceIdHash":"h","pool":"mobile","issuedAt":"2026-01-01T00:00:00Z","expiresAt":"2026-02-01T00:00:00Z","graceSeconds":86400,"generation":1,"entitlement":"tok","signature":"sig"}
"""

struct ContinuityBridgeTests {
    @Test func payloadBuildersMatchWireContract() {
        #expect(ContinuityBridgePayload.fromActivation(app: "a", activationToken: "t", deviceId: "d", version: "1") ==
            ["app": "a", "activationToken": "t", "deviceId": "d", "version": "1"])
        #expect(ContinuityBridgePayload.exchange(app: "a", bridgeToken: "b", deviceId: "d", platform: "ios", version: "1") ==
            ["app": "a", "bridgeToken": "b", "deviceId": "d", "platform": "ios", "version": "1"])
        let withoutMobile = ContinuityBridgePayload.fromStoreKit(app: "a", signedTransaction: "jws", deviceId: "d", platform: "ios", publishBridge: true, version: "1")
        #expect(withoutMobile == ["app": "a", "signedTransaction": "jws", "deviceId": "d", "platform": "ios", "publishBridge": "true", "version": "1"])
        let withMobile = ContinuityBridgePayload.fromStoreKit(app: "a", signedTransaction: "jws", deviceId: "d", platform: "ios", publishBridge: false, signedMobileTransaction: "m", version: "1")
        #expect(withMobile["publishBridge"] == "false")
        #expect(withMobile["signedMobileTransaction"] == "m")
    }

    @Test func endpointURLsAreUnderBridge() {
        let client = stubClient(FakeBridgeTransport { _ in (200, Data()) })
        #expect(client.fromActivationURL.absoluteString == "https://example.test/v1/licenses/bridge/from-activation")
        #expect(client.fromStoreKitURL.absoluteString == "https://example.test/v1/licenses/bridge/from-storekit")
        #expect(client.exchangeURL.absoluteString == "https://example.test/v1/licenses/bridge/exchange")
    }

    @Test func fromActivationPostsBodyAndDecodesBridgeToken() async throws {
        let t = FakeBridgeTransport { _ in (200, json(#"{"ok":true,"bridge_token":"bt-1"}"#)) }
        let response = try await stubClient(t).publishFromActivation(activationToken: "act", deviceId: "dev")
        #expect(response.bridgeToken == "bt-1")
        let request = try #require(t.recorded.first)
        #expect(request.method == "POST")
        #expect(request.url.path == "/v1/licenses/bridge/from-activation")
        #expect(request.body == ["app": "scraperight", "activationToken": "act", "deviceId": "dev", "version": "9.9"])
    }

    @Test func fromStoreKitDecodesActivationLeaseAndSendsPlatform() async throws {
        let body = #"{"ok":true,"bridgeToken":"bt","activation":{"ok":true,"activationToken":"act","lease":\#(leaseJSON)}}"#
        let t = FakeBridgeTransport { _ in (200, json(body)) }
        let response = try await stubClient(t).publishFromStoreKit(signedTransaction: "jws", deviceId: "dev", publishBridge: true)
        #expect(response.activation?.activationToken == "act")
        #expect(response.activation?.lease?.leaseId == "l1")
        let request = try #require(t.recorded.first)
        #expect(request.url.path == "/v1/licenses/bridge/from-storekit")
        #expect(request.body["platform"] == "ios")
        #expect(request.body["publishBridge"] == "true")
        #expect(request.body["signedTransaction"] == "jws")
    }

    @Test func exchangeDecodesSnakeCaseLease() async throws {
        let body = #"{"ok":true,"activation_token":"act","lease":{"lease_id":"l2","device_id_hash":"h","pool":"mobile","issued_at":"a","expires_at":"b","grace_seconds":5,"generation":2}}"#
        let t = FakeBridgeTransport { _ in (200, json(body)) }
        let response = try await stubClient(t).exchange(bridgeToken: "bt", deviceId: "dev")
        #expect(response.activationToken == "act")
        #expect(response.lease?.leaseId == "l2")
        #expect(t.recorded.first?.body["bridgeToken"] == "bt")
    }

    private func exchangeFails(_ handler: @escaping (URLRequest) throws -> (Int, Data)) async -> ContinuityBridgeFailure? {
        do {
            _ = try await stubClient(FakeBridgeTransport(handler)).exchange(bridgeToken: "bt", deviceId: "dev")
            return nil
        } catch { return error as? ContinuityBridgeFailure }
    }

    @Test func seatLimitIsTyped() async {
        for (status, body) in [(409, #"{"message":"nope"}"#), (200, #"{"ok":false,"status":"seat_limit"}"#), (400, #"{"message":"Device pool is full"}"#)] {
            #expect(await exchangeFails { _ in (status, json(body)) } == .seatLimitReached)
        }
    }

    @Test func sourceInactiveIsTyped() async throws {
        #expect(await exchangeFails { _ in (422, json(#"{"message":"Purchase refunded"}"#)) } == .sourceInactive("Purchase refunded"))
        #expect(await exchangeFails { _ in (200, json(#"{"ok":false,"status":"revoked","message":"gone"}"#)) } == .sourceInactive("gone"))
        let t = FakeBridgeTransport { _ in (200, json(#"{"ok":false,"status":"expired","message":"x"}"#)) }
        await #expect(throws: ContinuityBridgeFailure.sourceInactive("x")) {
            _ = try await stubClient(t).publishFromActivation(activationToken: "a", deviceId: "d")
        }
    }

    @Test func transportErrorIsUnreachable() async {
        let failure = await exchangeFails { _ in throw URLError(.notConnectedToInternet) }
        guard case .unreachable = failure else { Issue.record("expected unreachable, got \(String(describing: failure))"); return }
    }

    @Test func nonHTTPResponseIsUnreachable() async {
        final class NonHTTP: ContinuityBridgeTransport, @unchecked Sendable {
            func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
                (Data(), URLResponse(url: request.url!, mimeType: nil, expectedContentLength: 0, textEncodingName: nil))
            }
        }
        let client = ContinuityBridgeClient(app: "a", baseURL: URL(string: "https://example.test/v1/licenses")!, transport: NonHTTP(), platform: { "ios" })
        await #expect(throws: ContinuityBridgeFailure.unreachable("No HTTP response.")) {
            _ = try await client.exchange(bridgeToken: "bt", deviceId: "dev")
        }
    }

    @Test func otherRejectionIsRejectedAndUnusableBodyIsRejected() async {
        #expect(await exchangeFails { _ in (400, json(#"{"error":"bad request"}"#)) } == .rejected("bad request"))
        #expect(await exchangeFails { _ in (200, json("not json")) } == .rejected(""))
        // Per rightkit#701 a 5xx is "anything else": rejected, not unreachable.
        #expect(await exchangeFails { _ in (503, Data()) } == .rejected(""))
    }

    @Test func requestShapeIsJsonWithEighteenSecondTimeout() async throws {
        let t = FakeBridgeTransport { _ in (200, json(#"{"ok":true}"#)) }
        _ = try await stubClient(t).exchange(bridgeToken: "bt", deviceId: "dev")
        let r = try #require(t.recorded.first)
        #expect(r.timeout == 18)
        #expect(r.headers["Accept"] == "application/json")
        #expect(r.headers["Content-Type"] == "application/json")
    }
}
