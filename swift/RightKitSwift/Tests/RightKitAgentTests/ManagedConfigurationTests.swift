import Foundation
import XCTest
@testable import RightKitAgent
@MainActor
final class ManagedConfigurationTests: XCTestCase {
    func testDeclaredPresetKindsStayLimitedToLiveProviderConsumer() {
        XCTAssertEqual(ManagedConnectionPreset.Kind.allCases, [.providerBaseURL])
    }

    func testValidatorRejectsUnsafeEndpointsAndIncompleteSecretSlots() {
        let unsafe = [
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "HTTP",
                endpoint: "http://models.example.com"
            ),
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "Credentials",
                endpoint: "https://user:pass@models.example.com"
            ),
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "Query secret",
                endpoint: "https://models.example.com/v1?token=not-allowed"
            ),
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "Query API key",
                endpoint: "https://models.example.com/v1?api_key=not-allowed"
            ),
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "Half slot",
                endpoint: "https://models.example.com",
                secretSlotKind: "provider-api-key"
            ),
        ]

        XCTAssertTrue(unsafe.allSatisfy { ManagedConfigurationValidator.validate($0) == nil })
    }

    func testProviderMergesManagedAppAndLegacyChannelsWithDeclarativePrecedence() async throws {
        let defaults = makeDefaults()
        defer { clear(defaults) }

        let declarative = ManagedConfigurationPayload(presets: [
            ManagedConnectionPreset(
                kind: .providerBaseURL,
                displayName: "Declarative Models",
                endpoint: "https://models.example.com/v1"
            )
        ])
        defaults.set(
            [
                "presets": [
                    [
                        "kind": "provider-base-url",
                        "displayName": "Legacy Models",
                        "endpoint": "https://MODELS.example.com/legacy",
                    ],
                ]
            ], forKey: ManagedConfigurationProvider.legacyChannelKey)

        let (updates, continuation) = AsyncStream<ManagedConfigurationPayload?>.makeStream()
        defer { continuation.finish() }
        let provider = ManagedConfigurationProvider(defaults: defaults, declarativeUpdates: updates)
        continuation.yield(declarative)

        for _ in 0..<50 {
            if provider.presets.map(\.displayName) == ["Declarative Models"] { break }
            try await Task.sleep(for: .milliseconds(10))
        }

        XCTAssertTrue(provider.hasManagedConfiguration)
        XCTAssertEqual(provider.presets.map(\.displayName), ["Declarative Models"])
        XCTAssertEqual(
            provider.preset(kind: .providerBaseURL, host: "models.example.com")?.endpoint,
            "https://models.example.com/v1"
        )
    }

    func testProviderRejectsPayloadsThatCarrySecretMaterial() throws {
        let defaults = makeDefaults()
        defer { clear(defaults) }
        defaults.set(
            [
                "presets": [
                    [
                        "kind": "provider-base-url",
                        "displayName": "Unsafe",
                        "endpoint": "https://models.example.com",
                        "apiKey": "must-never-enter-app-configuration",
                    ]
                ]
            ], forKey: ManagedConfigurationProvider.legacyChannelKey)

        let provider = ManagedConfigurationProvider(defaults: defaults)

        XCTAssertTrue(provider.presets.isEmpty)
        XCTAssertFalse(provider.hasManagedConfiguration)

        let managedAppPayload = Data(
            """
            {"presets":[{"kind":"provider-base-url","displayName":"Unsafe","endpoint":"https://models.example.com","apiKey":"must-never-enter-app-configuration"}]}
            """.utf8
        )
        XCTAssertThrowsError(try JSONDecoder().decode(ManagedConfigurationPayload.self, from: managedAppPayload))
    }

    func testLegacyDefaultsChangeRepublishesManagedPresetsForLiveUIConsumers() async throws {
        let defaults = makeDefaults()
        defer { clear(defaults) }
        let provider = ManagedConfigurationProvider(defaults: defaults)
        XCTAssertTrue(provider.presets.isEmpty)

        defaults.set(
            [
                "presets": [
                    [
                        "kind": "provider-base-url",
                        "displayName": "Managed Models",
                        "endpoint": "https://models.example.com/v1",
                    ]
                ]
        ], forKey: ManagedConfigurationProvider.legacyChannelKey)
        NotificationCenter.default.post(name: UserDefaults.didChangeNotification, object: defaults)

        for _ in 0..<50 {
            if provider.presets.map(\.displayName) == ["Managed Models"] { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("Managed configuration change was not republished")
    }

    private func makeDefaults() -> UserDefaults {
        let name = "ManagedConfigurationTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    private func clear(_ defaults: UserDefaults) {
        defaults.removeObject(forKey: ManagedConfigurationProvider.legacyChannelKey)
    }
}
