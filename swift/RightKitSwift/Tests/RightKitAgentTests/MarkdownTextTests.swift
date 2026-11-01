import XCTest
@testable import RightKitAgent
@MainActor
final class MarkdownTextTests: XCTestCase {

    func testPlainProseIsOneSegment() {
        XCTAssertEqual(MarkdownText.segments(from: "just text"), [.prose("just text")])
    }

    func testFencedBlockSplitsProseAndCode() {
        let raw = "before\n```swift\nlet x = 1\n```\nafter"
        XCTAssertEqual(
            MarkdownText.segments(from: raw),
            [.prose("before"), .code("let x = 1"), .prose("after")]
        )
    }

    func testUnterminatedFenceRendersAsCodeNotDroppedText() {
        let raw = "intro\n```\nlet y = 2\nstill code"
        XCTAssertEqual(
            MarkdownText.segments(from: raw),
            [.prose("intro"), .code("let y = 2\nstill code")]
        )
    }

    func testEmptySegmentsAreDropped() {
        XCTAssertEqual(MarkdownText.segments(from: "```\ncode\n```"), [.code("code")])
    }

    func testInlineMarkdownIsParsedNotLiteral() {
        let attributed = MarkdownText.attributed("a **bold** word")
        XCTAssertFalse(String(attributed.characters).contains("**"), "markers should be consumed")
        XCTAssertTrue(String(attributed.characters).contains("bold"))
    }

    func testMalformedMarkdownFallsBackToPlainText() {
        let raw = "unclosed [link]("
        XCTAssertEqual(String(MarkdownText.attributed(raw).characters), raw)
    }
}

@MainActor
final class DiffTests: XCTestCase {

    func testUnifiedDiffDetection() {
        XCTAssertTrue(looksLikeUnifiedDiff("--- a/x.swift\n+++ b/x.swift\n@@ -1 +1 @@\n-old\n+new"))
        XCTAssertTrue(looksLikeUnifiedDiff("@@ -10,4 +10,6 @@\n context"))
        XCTAssertFalse(looksLikeUnifiedDiff("This will delete 3 files from the workspace."))
        XCTAssertFalse(looksLikeUnifiedDiff("run `rm -rf build/`"))
    }

    func testLineClassificationHeadersBeforeSigns() {
        XCTAssertEqual(DiffLineKind(of: "+++ b/file.swift"), .meta)
        XCTAssertEqual(DiffLineKind(of: "--- a/file.swift"), .meta)
        XCTAssertEqual(DiffLineKind(of: "+added line"), .addition)
        XCTAssertEqual(DiffLineKind(of: "-removed line"), .removal)
        XCTAssertEqual(DiffLineKind(of: "@@ -1,3 +1,4 @@"), .hunk)
        XCTAssertEqual(DiffLineKind(of: " unchanged"), .context)
    }

    func testApprovalEventWithDiffPreviewSplitsDiffFromText() throws {
        let json = #"""
        {"$kind":"toolApprovalRequested","seq":1,"call":{"id":"tc1","name":"edit_file"},
        "riskLabel":"medium","proposedChange":"--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new"}
        """#
        let event = try JSONDecoder().decode(ProtocolEvent.self, from: Data(json.utf8))
        let display = try XCTUnwrap(event.displayMessage())
        XCTAssertEqual(display.text, "edit_file (medium)", "diff must not leak into the text blob")
        XCTAssertNotNil(display.diff)
        XCTAssertTrue(display.diff?.contains("@@ -1 +1 @@") ?? false)
    }

    func testApprovalEventWithProsePreviewKeepsTextBehavior() throws {
        let json = #"""
        {"$kind":"toolApprovalRequested","seq":1,"call":{"id":"tc2","name":"shell"},
        "riskLabel":"high","proposedChange":"Will run: rm -rf build/"}
        """#
        let event = try JSONDecoder().decode(ProtocolEvent.self, from: Data(json.utf8))
        let display = try XCTUnwrap(event.displayMessage())
        XCTAssertNil(display.diff)
        XCTAssertTrue(display.text.contains("Will run: rm -rf build/"))
    }
}
