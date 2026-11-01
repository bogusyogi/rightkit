import Foundation
import XCTest
@testable import RightKitMedia
@MainActor
final class WasmComputeBridgeTests: XCTestCase {
    func testRustToolReachesInstalledWasmKitCallback() throws {
        XCTAssertEqual(
            coderight_mobile_install_wasm_executor(
                coderightIOSWasmCompute,
                coderightIOSWasmResponseFree
            ),
            0
        )
        let raw = try XCTUnwrap(coderight_mobile_wasm_executor_self_test())
        defer { coderight_mobile_string_free(raw) }

        struct Response: Decodable {
            let ok: Bool
            let outputBase64: String?
            let error: String?

            enum CodingKeys: String, CodingKey {
                case ok
                case outputBase64 = "output_base64"
                case error
            }
        }
        let response = try JSONDecoder().decode(
            Response.self,
            from: Data(String(cString: raw).utf8)
        )
        XCTAssertTrue(response.ok, response.error ?? "Rust-to-WasmKit self-test failed")
        XCTAssertFalse(response.outputBase64?.isEmpty ?? true)
    }
}
