import Foundation
import XCTest
@testable import RightKitHelperProtocol

final class RightKitHelperProtocolTests: XCTestCase {
    struct Req: Codable, Equatable {
        let command: String
        let scope: String?
    }
    struct Resp: Codable, Equatable {
        let ok: Bool
        let value: String?
    }

    /// Runs `serve` over scripted input chunks and returns (exit code, response lines).
    private func run(_ chunks: [String], max: Int = 1024) -> (Int32, [String]) {
        var queue = chunks.map { Data($0.utf8) }
        var out: [String] = []
        let code = HelperProtocol.serve(
            maxLineBytes: max,
            read: { queue.isEmpty ? Data() : queue.removeFirst() },
            write: { out.append($0) },
            handler: { (r: Req) in Resp(ok: true, value: r.command) })
        return (code, out)
    }

    func testDecodeRequestLine() throws {
        let r = try HelperProtocol.decodeRequestLine(#"{"command":"read","scope":"synced"}"#, as: Req.self).get()
        XCTAssertEqual(r, Req(command: "read", scope: "synced"))
    }

    func testDecodeFailures() {
        XCTAssertEqual(HelperProtocol.decodeRequestLine(nil, as: Req.self), .failure(.emptyRequest))
        XCTAssertEqual(HelperProtocol.decodeRequestLine("  ", as: Req.self), .failure(.emptyRequest))
        XCTAssertEqual(HelperProtocol.decodeRequestLine("not json", as: Req.self), .failure(.invalidJSON))
        XCTAssertEqual(
            HelperProtocol.decodeRequestLine(#"{"command":"read"}"#, as: Req.self, maxBytes: 4),
            .failure(.lineTooLong(limit: 4)))
    }

    func testEncodeIsSingleSortedLine() {
        let line = HelperProtocol.encodeResponseLine(Resp(ok: true, value: "a\nb"))
        XCTAssertFalse(line.contains("\n"))
        XCTAssertEqual(line, #"{"ok":true,"value":"a\nb"}"#)
    }

    func testRunOnceHandledAndProtocolFailure() {
        var out: [String] = []
        let ok = HelperProtocol.runOnce(
            readLine: { #"{"command":"status"}"# }, write: { out.append($0) },
            handler: { (r: Req) in Resp(ok: true, value: r.command) })
        XCTAssertEqual(ok, 0)
        XCTAssertEqual(out, [#"{"ok":true,"value":"status"}"#])

        out = []
        let bad = HelperProtocol.runOnce(
            readLine: { nil }, write: { out.append($0) },
            handler: { (_: Req) in Resp(ok: true, value: nil) })
        XCTAssertEqual(bad, 1)
        XCTAssertEqual(out, [#"{"error":"no request on stdin","ok":false}"#])
    }

    func testServeRoundTripAcrossSplitReads() {
        let (code, out) = run([#"{"command":"a"}"# + "\n{\"comm", "and\":\"b\"}\r\n"])
        XCTAssertEqual(code, 0)
        XCTAssertEqual(out, [#"{"ok":true,"value":"a"}"#, #"{"ok":true,"value":"b"}"#])
    }

    func testServeMalformedLineGetsTypedErrorAndLoopContinues() {
        let (code, out) = run(["not json\n\n" + #"{"command":"next"}"# + "\n"])
        XCTAssertEqual(code, 0)
        XCTAssertEqual(out, [#"{"error":"invalid request JSON","ok":false}"#, #"{"ok":true,"value":"next"}"#])
    }

    func testServeOversizedLineGetsTypedErrorAndResynchronises() {
        let big = String(repeating: "x", count: 100)
        // The oversized line spans several chunks; the next request must still be served.
        let (code, out) = run([big, big, big + "\n", #"{"command":"after"}"# + "\n"], max: 64)
        XCTAssertEqual(code, 0)
        XCTAssertEqual(out, [#"{"error":"request line exceeds 64 bytes","ok":false}"#, #"{"ok":true,"value":"after"}"#])
    }

    func testServeEOFEndsLoop() {
        XCTAssertEqual(run([]).0, 0)
        XCTAssertEqual(run([]).1, [])
        // Final line without a newline is still handled, then EOF ends the loop.
        let (code, out) = run([#"{"command":"last"}"#])
        XCTAssertEqual(code, 0)
        XCTAssertEqual(out, [#"{"ok":true,"value":"last"}"#])
    }

    func testServeStopsWhenWriteFails() {
        struct Closed: Error {}
        let code = HelperProtocol.serve(
            read: { Data("{\"command\":\"a\"}\n".utf8) },
            write: { _ in throw Closed() },
            handler: { (_: Req) in Resp(ok: true, value: nil) })
        XCTAssertEqual(code, 1)
    }

    func testServeOverFileHandles() throws {
        let inPipe = Pipe(), outPipe = Pipe()
        inPipe.fileHandleForWriting.write(Data((#"{"command":"pipe"}"# + "\n").utf8))
        try inPipe.fileHandleForWriting.close()
        let code = HelperProtocol.serve(
            input: inPipe.fileHandleForReading, output: outPipe.fileHandleForWriting
        ) { (r: Req) in Resp(ok: true, value: r.command) }
        try outPipe.fileHandleForWriting.close()
        XCTAssertEqual(code, 0)
        let text = String(decoding: outPipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        XCTAssertEqual(text, #"{"ok":true,"value":"pipe"}"# + "\n")
    }

    func testFrameRoundTripAcrossSplitReads() throws {
        let a = try HelperProtocol.frame(Data("hello".utf8))
        let b = try HelperProtocol.frame(Data("world!".utf8))
        XCTAssertEqual(Array(a.prefix(4)), [0, 0, 0, 5])
        var buffer = Data()
        buffer.append(a.prefix(3))
        XCTAssertNil(try HelperProtocol.nextFrame(from: &buffer))
        buffer.append(a.dropFirst(3))
        buffer.append(b)
        XCTAssertEqual(try HelperProtocol.nextFrame(from: &buffer), Data("hello".utf8))
        XCTAssertEqual(try HelperProtocol.nextFrame(from: &buffer), Data("world!".utf8))
        XCTAssertNil(try HelperProtocol.nextFrame(from: &buffer))
        XCTAssertTrue(buffer.isEmpty)
    }

    func testFrameLimits() throws {
        XCTAssertThrowsError(try HelperProtocol.frame(Data(count: 9), maxBytes: 8))
        var hostile = Data([0xFF, 0xFF, 0xFF, 0xFF])
        XCTAssertThrowsError(try HelperProtocol.nextFrame(from: &hostile, maxBytes: 1024)) {
            XCTAssertEqual($0 as? HelperProtocolError, .frameTooLarge(limit: 1024))
        }
    }

    func testLineBufferSplitsAndBounds() {
        var buf = HelperLineBuffer(maxLineBytes: 16)
        XCTAssertEqual(buf.append(Data("{\"a\":1}\r\n{\"b\"".utf8)), [.line("{\"a\":1}")])
        XCTAssertEqual(buf.append(Data(":2}\n".utf8)), [.line("{\"b\":2}")])
        XCTAssertEqual(buf.append(Data(String(repeating: "x", count: 40).utf8)), [.overflow(limit: 16)])
        XCTAssertEqual(buf.append(Data("yy\nok\n".utf8)), [.line("ok")])
    }
}
