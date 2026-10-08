import Foundation
import XCTest
@testable import RecordWire

final class PlanAndTransportTests: XCTestCase {
    private func goodPlan() -> [String: Any] {
        ["outDir": "/tmp/x", "windowId": 7, "pid": 3, "fps": 30, "scale": 1.0, "codec": "h264",
         "audio": [String](), "visibility": "off_screen_visible", "masking": ["mode": "declared", "regions": [Any]()]]
    }

    func testPlanRefusesWhatTheCoreWouldNotSend() {
        XCTAssertNoThrow(try StartPlan.decode(goodPlan()))
        var missing = goodPlan(); missing.removeValue(forKey: "fps")
        XCTAssertThrowsError(try StartPlan.decode(missing))
        var badCodec = goodPlan(); badCodec["codec"] = "prores"
        XCTAssertThrowsError(try StartPlan.decode(badCodec))
        var floaty = goodPlan(); floaty["windowId"] = 7.5
        XCTAssertThrowsError(try StartPlan.decode(floaty))
        var negative = goodPlan(); negative["pid"] = -1
        XCTAssertThrowsError(try StartPlan.decode(negative))
        var bool = goodPlan(); bool["fps"] = true
        XCTAssertThrowsError(try StartPlan.decode(bool))
        var huge = goodPlan(); huge["pid"] = 5_000_000_000
        XCTAssertThrowsError(try StartPlan.decode(huge))
        var badMask = goodPlan(); badMask["masking"] = ["mode": "sparkle"]
        XCTAssertThrowsError(try StartPlan.decode(badMask))
        var badVis = goodPlan(); badVis["visibility"] = "peekaboo"
        XCTAssertThrowsError(try StartPlan.decode(badVis))
    }

    func testRequestNeedsKindAndOp() {
        XCTAssertThrowsError(try Request.decode(["id": 1, "op": "hello"]))
        XCTAssertThrowsError(try Request.decode(["kind": "request", "id": 1]))
        XCTAssertEqual(try Request.decode(["kind": "request", "id": 9, "op": "stop"]).op, "stop")
    }

    func testFramesRoundTripThroughAPipe() throws {
        var fds: [Int32] = [0, 0]
        XCTAssertEqual(pipe(&fds), 0)
        defer { close(fds[0]); close(fds[1]) }
        let writer = FrameWriter(fd: fds[1])
        let reader = FrameReader(fd: fds[0])
        try writer.write(Messages.event(.exited))
        try writer.write(Messages.bye)
        XCTAssertEqual(try Framing.parse(try XCTUnwrap(reader.next()))["kind"] as? String, "event")
        XCTAssertEqual(try Framing.parse(try XCTUnwrap(reader.next()))["kind"] as? String, "bye")
    }

    func testReaderReportsEndTruncationAndHostileLengths() throws {
        func feed(_ bytes: [UInt8]) throws -> FrameReader {
            var fds: [Int32] = [0, 0]
            XCTAssertEqual(pipe(&fds), 0)
            _ = bytes.withUnsafeBytes { write(fds[1], $0.baseAddress, bytes.count) }
            close(fds[1])
            return FrameReader(fd: fds[0])
        }
        // Clean end between frames.
        XCTAssertNil(try feed([]).next())
        // Ends inside the length, and inside the body.
        XCTAssertThrowsError(try feed([0, 0]).next())
        XCTAssertThrowsError(try feed([0, 0, 0, 5, 1, 2]).next())
        // Zero length and a length past the limit are refused before reading a body.
        XCTAssertThrowsError(try feed([0, 0, 0, 0]).next())
        XCTAssertThrowsError(try feed([0xFF, 0xFF, 0xFF, 0xFF]).next())
        XCTAssertThrowsError(try Framing.frame(body: Data()))
    }

    func testFrameHasherIgnoresRowPadding() {
        // 5x3 BGRA, rows padded from 20 to 32 bytes with a poison value.
        let width = 5, height = 3, stride = 32
        var padded = [UInt8](repeating: 0xEE, count: stride * height)
        var packed = [UInt8]()
        for y in 0..<height {
            for x in 0..<(width * 4) {
                let value = UInt8((y * 37 + x * 11) & 0xFF)
                padded[y * stride + x] = value
                packed.append(value)
            }
        }
        let hasher = FrameHasher()
        let viaPadded = padded.withUnsafeBytes {
            hasher.hashPackedBGRA(base: $0.baseAddress!, width: width, height: height, bytesPerRow: stride)
        }
        XCTAssertEqual(viaPadded, Blake3.text(Data(packed)))
        let viaPacked = packed.withUnsafeBytes {
            hasher.hashPackedBGRA(base: $0.baseAddress!, width: width, height: height, bytesPerRow: width * 4)
        }
        XCTAssertEqual(viaPacked, viaPadded)
    }

    func testFrameHashSeesASinglePixelChange() {
        var frame = [UInt8](repeating: 7, count: 640 * 360 * 4)
        let hasher = FrameHasher()
        let before = frame.withUnsafeBytes { hasher.hashPackedBGRA(base: $0.baseAddress!, width: 640, height: 360, bytesPerRow: 2560) }
        frame[frame.count - 1] = 8
        let after = frame.withUnsafeBytes { hasher.hashPackedBGRA(base: $0.baseAddress!, width: 640, height: 360, bytesPerRow: 2560) }
        XCTAssertNotEqual(before, after)
        XCTAssertTrue(before.hasPrefix("blake3:") && before.count == 7 + 64)
    }

    func testHashingAFullFrameIsFastEnoughForRealTime() throws {
        #if DEBUG
        throw XCTSkip("timing is asserted in release builds only: swift test -c release -Xswiftc -enable-testing")
        #endif
        // 1600x900 BGRA is 5.76 MB. At 60 fps the helper has about 16 ms a frame for hash plus encode.
        let frame = [UInt8](repeating: 1, count: 1600 * 900 * 4)
        let hasher = FrameHasher()
        let start = Date()
        for _ in 0..<20 {
            _ = frame.withUnsafeBytes { hasher.hashPackedBGRA(base: $0.baseAddress!, width: 1600, height: 900, bytesPerRow: 6400) }
        }
        let perFrameMs = Date().timeIntervalSince(start) * 1000 / 20
        print("blake3 1600x900 frame: \(perFrameMs) ms")
        XCTAssertLessThan(perFrameMs, 12, "hashing a frame must stay well under a 30 fps interval")
    }
}
