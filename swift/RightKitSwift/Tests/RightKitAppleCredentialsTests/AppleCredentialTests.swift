import Foundation
import XCTest
@testable import RightKitAppleCredentials

final class AppleCredentialTests: XCTestCase {
    private func store() -> (AppleCredentialStore, InMemoryAppleCredentialBackend) {
        let backend = InMemoryAppleCredentialBackend()
        return (AppleCredentialStore(service: "test.\(UUID().uuidString)", backend: backend), backend)
    }

    func testRoundTripAndPassivePresence() throws {
        let (store, _) = store()
        XCTAssertFalse(try store.exists("alpha", scope: .device).get())
        try store.write("opaque-fixture", account: "alpha", scope: .device).get()
        XCTAssertTrue(try store.exists("alpha", scope: .device).get())
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "opaque-fixture")
    }

    func testScopesAndAccountsAreIsolated() throws {
        let (store, _) = store()
        try store.write("synced-fixture", account: "alpha", scope: .synced).get()
        try store.write("device-fixture", account: "alpha", scope: .device).get()
        try store.write("other-fixture", account: "beta", scope: .device).get()
        XCTAssertEqual(try store.read("alpha", scope: .synced).get(), "synced-fixture")
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "device-fixture")
        XCTAssertNil(try store.read("beta", scope: .synced).get())
        XCTAssertEqual(try store.copySyncedToDeviceOnly("alpha").get(), true)
        XCTAssertEqual(try store.read("alpha", scope: .device).get(), "synced-fixture")
    }

    func testDeleteEverywhereAndInputValidation() throws {
        let (store, _) = store()
        try store.write("fixture", account: "alpha", scope: .synced).get()
        try store.write("fixture", account: "alpha", scope: .device).get()
        try store.deleteEverywhere("alpha").get()
        XCTAssertFalse(try store.exists("alpha", scope: .synced).get())
        XCTAssertFalse(try store.exists("alpha", scope: .device).get())
        XCTAssertThrowsError(try store.write("fixture", account: "bad account", scope: .device).get())
        XCTAssertThrowsError(try store.write("", account: "alpha", scope: .device).get())
    }

    func testHelperStatusAndExistsNeverReturnSecretBytes() throws {
        let (store, _) = store()
        try store.write("secret-fixture", account: "alpha", scope: .device).get()

        let request = try JSONEncoder().encode(AppleCredentialRequest(command: "exists", scope: .device, account: "alpha"))
        let input = Pipe(); let output = Pipe()
        input.fileHandleForWriting.write(request + Data([0x0a]))
        input.fileHandleForWriting.closeFile()
        XCTAssertEqual(AppleCredentialHelper.run(store: store, input: input.fileHandleForReading, output: output.fileHandleForWriting), 0)
        output.fileHandleForWriting.closeFile()
        let response = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)!
        XCTAssertTrue(response.contains("\"exists\":true"))
        XCTAssertFalse(response.contains("secret-fixture"))
    }

    func testHelperJSONLinesThroughIsolatedProcess() throws {
        guard let helper = ProcessInfo.processInfo.environment["RIGHTKIT_APPLE_CREDENTIALS_HELPER"] else {
            throw XCTSkip("set RIGHTKIT_APPLE_CREDENTIALS_HELPER to run helper process E2E")
        }
        let request = try JSONEncoder().encode(AppleCredentialRequest(command: "write", scope: .device, account: "process.account", value: "process-secret-fixture"))
        let input = Pipe(); let output = Pipe(); let process = Process()
        process.executableURL = URL(fileURLWithPath: helper)
        process.environment = ["RIGHTKIT_HELPER_TEST_IN_MEMORY": "1", "RIGHTKIT_HELPER_SERVICE": "rightkit.process.\(UUID().uuidString)"]
        process.standardInput = input; process.standardOutput = output; process.standardError = FileHandle.nullDevice
        try process.run()
        input.fileHandleForWriting.write(request + Data([0x0a]))
        input.fileHandleForWriting.closeFile()
        process.waitUntilExit()
        let response = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)!
        XCTAssertEqual(process.terminationStatus, 0)
        XCTAssertTrue(response.contains("\"ok\":true"))
        XCTAssertFalse(response.contains("process-secret-fixture"))
    }
}

private extension Result {
    func get() throws -> Success {
        switch self {
        case let .success(value): return value
        case let .failure(error): throw error
        }
    }
}
