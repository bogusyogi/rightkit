import XCTest
#if os(iOS)
@testable import RightKitAgent

/// The target compiles and links on its host platform; behaviour tests arrive with the hosts that adopt it.
final class RightKitAgentLinkTests: XCTestCase {
    func testModuleLinks() {}
}
#endif
