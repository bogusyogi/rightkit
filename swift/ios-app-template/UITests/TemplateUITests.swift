import XCTest

/// End-to-end: launches the real app and checks the user-visible result of the wiring.
final class TemplateUITests: XCTestCase {
    func testAppLaunchesWithSharedPackageWired() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.staticTexts["template.title"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["template.summary"].label.contains("linked"))
        XCTAssertTrue(app.staticTexts["template.wiring"].label.contains("wired"))
    }
}
