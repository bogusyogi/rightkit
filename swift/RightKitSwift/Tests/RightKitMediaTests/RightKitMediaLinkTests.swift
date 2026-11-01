import XCTest
#if os(macOS)
@testable import RightKitMedia

/// The target compiles and links on its host platform; behaviour tests arrive with the hosts that adopt it.
final class RightKitMediaLinkTests: XCTestCase {
    func testModuleLinks() {}
}
#endif
