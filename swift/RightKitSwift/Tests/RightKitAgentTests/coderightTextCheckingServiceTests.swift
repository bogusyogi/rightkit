import Foundation
import XCTest
@testable import RightKitAgent
@MainActor
final class TextCheckingServiceTests: XCTestCase {
    func testFlagsKnownMisspelling() {
        let service = TextCheckingService()
        let issues = service.check(text: "This is a definately correct sentence.")

        let misspelled = issues.first { $0.word.caseInsensitiveCompare("definately") == .orderedSame }
        XCTAssertNotNil(misspelled, "expected 'definately' to be flagged as misspelled")
        XCTAssertEqual(misspelled?.kind, .spelling)
    }

    func testCleanTextHasNoIssues() {
        let service = TextCheckingService()
        let issues = service.check(text: "This sentence is spelled correctly.")
        XCTAssertTrue(issues.isEmpty)
    }

    func testEmptyTextHasNoIssues() {
        let service = TextCheckingService()
        XCTAssertTrue(service.check(text: "").isEmpty)
    }
}
