import XCTest
#if os(iOS)
@testable import RightKitLibrary

/// The target compiles and links on its host platform; behaviour tests arrive with the hosts that adopt it.
final class RightKitLibraryLinkTests: XCTestCase {
    func testModuleLinks() {}
}
#endif
