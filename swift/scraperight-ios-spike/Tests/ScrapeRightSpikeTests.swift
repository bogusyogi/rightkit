import Network
import RightKitArtifacts
import RightKitDiagnostics
import RightKitLicense
import XCTest

/// The Phase 7 proof: a from-scratch ScrapeRight iOS target that consumes RightKitSwift only through
/// the package (no copied source) and drives it end to end on the simulator — import a fixture,
/// download it over loopback, verify + install it through the shared installer, record a diagnostic.
final class ScrapeRightSpikeTests: XCTestCase {
    func testDownloadsVerifiesInstallsAndRecordsDiagnostics() async throws {
        // 1. Import the fixture bundled with the test target, and the sha256 recorded from its bytes.
        let bundle = Bundle(for: Self.self)
        let fixtureURL = try XCTUnwrap(bundle.url(forResource: "artifact", withExtension: "tar.gz"))
        let expectedSHA = try XCTUnwrap(bundle.url(forResource: "artifact", withExtension: "sha256"))
            .flatMap { try? String(contentsOf: $0, encoding: .utf8) }?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let fixtureData = try Data(contentsOf: fixtureURL)

        // 2. Serve those bytes on loopback and actually download them over the network, rather than
        //    reading the file — the point of the spike is to exercise a real URLSession fetch.
        let server = try LoopbackServer(body: fixtureData)
        defer { server.stop() }
        let (downloaded, response) = try await URLSession.shared.data(from: server.url)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertEqual(downloaded, fixtureData, "loopback download did not return the fixture bytes")

        let downloadedArchive = FileManager.default.temporaryDirectory
            .appendingPathComponent("spike-\(UUID().uuidString).tar.gz")
        try downloaded.write(to: downloadedArchive)
        defer { try? FileManager.default.removeItem(at: downloadedArchive) }

        // 3. Verify + install through the shared installer. A wrong hash must reject; the real one
        //    must install and expose the artifact's contents.
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("spike-install-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: destination) }

        XCTAssertThrowsError(
            try ArtifactInstaller.install(
                archive: downloadedArchive,
                descriptor: ArtifactDescriptor(topLevelDirectory: "scraperight-model", expectedSHA256: String(repeating: "0", count: 64)),
                destinationRoot: destination,
            ),
            "a mismatched checksum must be rejected",
        )

        let installed = try ArtifactInstaller.install(
            archive: downloadedArchive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "scraperight-model", expectedSHA256: expectedSHA),
            destinationRoot: destination,
        )
        let version = try String(contentsOf: installed.appendingPathComponent("VERSION"), encoding: .utf8)
        XCTAssertEqual(version.trimmingCharacters(in: .whitespacesAndNewlines), "ok")

        // 4. Record a diagnostic through the shared store and confirm it was written.
        let logURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("spike-\(UUID().uuidString).jsonl")
        let store = JSONLDiagnosticStore(fileURL: logURL)
        XCTAssertTrue(store.record(DiagnosticEvent(name: "artifact_installed", fields: ["arch": "spike"])))
        XCTAssertFalse(store.validLines().isEmpty)
        try? FileManager.default.removeItem(at: logURL)
    }
}

/// One-request loopback HTTP server. Serves `body` to the first GET, so the test drives a genuine
/// network download without reaching the internet.
private final class LoopbackServer {
    private let listener: NWListener
    let url: URL

    init(body: Data) throws {
        listener = try NWListener(using: .tcp, on: .any)
        let response = Self.httpResponse(for: body)

        listener.newConnectionHandler = { connection in
            connection.start(queue: .global())
            // Drain the request, then write the response and close.
            connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { _, _, _, _ in
                connection.send(content: response, completion: .contentProcessed { _ in
                    connection.cancel()
                })
            }
        }

        let ready = DispatchSemaphore(value: 0)
        var boundPort: UInt16?
        let started = listener
        started.stateUpdateHandler = { state in
            if case .ready = state { boundPort = started.port?.rawValue; ready.signal() }
        }
        listener.start(queue: .global())
        guard ready.wait(timeout: .now() + 5) == .success, let port = boundPort else {
            listener.cancel()
            throw URLError(.cannotConnectToHost)
        }
        url = URL(string: "http://127.0.0.1:\(port)/artifact.tar.gz")!
    }

    func stop() { listener.cancel() }

    private static func httpResponse(for body: Data) -> Data {
        var header = "HTTP/1.1 200 OK\r\n"
        header += "Content-Type: application/gzip\r\n"
        header += "Content-Length: \(body.count)\r\n"
        header += "Connection: close\r\n\r\n"
        return Data(header.utf8) + body
    }
}
