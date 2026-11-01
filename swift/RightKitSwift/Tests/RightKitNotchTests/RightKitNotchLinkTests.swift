import XCTest
#if os(macOS)
@testable import RightKitNotch

/// The target compiles and links on its host platform; behaviour tests arrive with the hosts that adopt it.
final class RightKitNotchLinkTests: XCTestCase {
    func testModuleLinks() {}
}
#endif
