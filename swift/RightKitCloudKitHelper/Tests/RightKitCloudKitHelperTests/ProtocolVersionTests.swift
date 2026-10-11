import Foundation
import Testing
@testable import rightkit_cloudkit_helper

/// S06: the helper refuses a request whose protocol version it cannot interpret, before any
/// command (and so any mutation) runs.
@Suite struct ProtocolVersionTests {
    private func session() -> HelperSession { HelperSession(transport: MemoryZone(account: "acct", pageSize: 10)) }

    private func object(_ line: String) throws -> [String: Any] {
        try #require(JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])
    }

    private func errorKind(_ line: String) throws -> String? {
        ((try object(line)["error"]) as? [String: Any])?["kind"] as? String
    }

    private let save = #""cmd":"save_records","expected_scope":"acct","policy":"overwrite","records":[{"type":"Note","name":"n1","fields":{"text":{"t":"text","v":"hi"}}}]"#

    /// Prepares the zone with a supported request, so a rejected save would otherwise succeed.
    private func prepared() async throws -> HelperSession {
        let s = session()
        let r = await s.handle(line: #"{"v":1,"id":1,"cmd":"prepare","expected_scope":"acct"}"#)
        #expect(try object(r.response)["ok"] as? Bool == true)
        return s
    }

    private func recordCount(_ s: HelperSession) async throws -> Int {
        let r = await s.handle(line: #"{"v":1,"id":9,"cmd":"fetch_changes","expected_scope":"acct","since":null}"#)
        return (try object(r.response)["records"] as? [Any])?.count ?? -1
    }

    @Test func versionOneIsAccepted() async throws {
        let s = try await prepared()
        let r = await s.handle(line: #"{"v":1,"id":2,\#(save)}"#)
        #expect(try object(r.response)["ok"] as? Bool == true)
        #expect(try await recordCount(s) == 1)
    }

    /// `nil` = the `v` key is absent; otherwise the raw JSON text of its value.
    @Test(arguments: [
        ("absent", nil), ("v0", "0"), ("v2", "2"), ("negative", "-1"), ("string", #""1""#),
        ("fraction", "1.5"), ("boolean", "true"), ("null", "null"), ("array", "[1]"),
    ] as [(String, String?)])
    func unsupportedOrMissingVersionIsRefusedBeforeAnyMutation(label: String, raw: String?) async throws {
        let s = try await prepared()
        let version = raw.map { #""v":\#($0),"# } ?? ""
        let r = await s.handle(line: #"{\#(version)"id":2,\#(save)}"#)
        let body = try object(r.response)
        #expect(body["ok"] as? Bool == false, "\(label)")
        #expect(try errorKind(r.response) == "unsupported_protocol", "\(label): \(r.response)")
        #expect(body["id"] as? Int == 2, "the refusal still answers the request id")
        #expect(r.stop == false)
        #expect(try await recordCount(s) == 0, "\(label): a refused save must not write")
    }

    @Test func statusAndShutdownAreVersionCheckedToo() async throws {
        let s = session()
        let status = await s.handle(line: #"{"id":3,"cmd":"status"}"#)
        #expect(try errorKind(status.response) == "unsupported_protocol")
        let shutdown = await s.handle(line: #"{"v":2,"id":4,"cmd":"shutdown"}"#)
        #expect(try errorKind(shutdown.response) == "unsupported_protocol")
        #expect(shutdown.stop == false, "an unsupported shutdown does not stop the helper")
        let ok = await s.handle(line: #"{"v":1,"id":5,"cmd":"shutdown"}"#)
        #expect(ok.stop)
    }

    @Test func typedErrorsDescribeTheReason() {
        #expect(throws: HelperProtocolError.missingVersion) { try HelperProtocol.validate(nil) }
        #expect(throws: HelperProtocolError.unsupportedVersion("2")) { try HelperProtocol.validate(NSNumber(value: 2)) }
        #expect(throws: Never.self) { try HelperProtocol.validate(NSNumber(value: 1)) }
        #expect(HelperProtocolError.missingVersion.failure.kind == "unsupported_protocol")
    }
}
