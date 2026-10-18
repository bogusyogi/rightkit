import Foundation
import SwiftUI
import XCTest

import RightKitHub
import RightKitHubQA

final class HubSettingsTests: XCTestCase {
    private let groups = [
        HubSettingsGroup(
            id: "general",
            title: "General",
            settings: [
                .toggle(key: "app.menuBar", title: "Menu bar", detail: "Show the menu bar item.", defaultValue: true),
                .text(key: "app.name", title: "Display name", detail: "Shown in the header.", defaultValue: ""),
            ],
            keywords: ["basics"]
        )
    ]

    func testSearchMatchesGroupKeywordsAndDropsEmptyGroups() {
        XCTAssertEqual(HubSettingsSearch.filter(groups, query: "basics").first?.settings.count, 2)
        XCTAssertEqual(HubSettingsSearch.filter(groups, query: "menu bar").first?.settings.map(\.key), ["app.menuBar"])
        XCTAssertTrue(HubSettingsSearch.filter(groups, query: "nothing like this").isEmpty)
        XCTAssertTrue(HubSettingsSearch.duplicateKeys(in: groups + groups).contains("app.menuBar"))
    }

    func testSettingValueCodableKeepsTypes() throws {
        let values: [HubSettingValue] = [.bool(true), .string("x"), .number(2)]
        for value in values {
            let data = try JSONEncoder().encode([value])
            XCTAssertEqual(try JSONDecoder().decode([HubSettingValue].self, from: data), [value])
        }
    }

    @MainActor
    func testModelAppliesDefaultsStoresAndResets() async {
        let store = InMemorySettingsStore()
        let model = HubSettingsModel(store: store, groups: groups)
        let toggle = groups[0].settings[0]
        XCTAssertTrue(model.isOn(toggle))
        XCTAssertTrue(model.isDefault(toggle))
        await model.set(toggle, .bool(false))
        XCTAssertFalse(model.isOn(toggle))
        let stored = try? await store.get(["app.menuBar"])
        XCTAssertEqual(stored?["app.menuBar"], .bool(false))
        await model.reset(toggle)
        XCTAssertTrue(model.isOn(toggle))
    }

    func testEngineSettingsStoreUsesTheSettingsCommands() async throws {
        let engine = QAEngineClient()
        engine.respond(to: EngineSettingsStore.getMethod, json: #"{"values":{"a":true}}"#)
        engine.respond(to: EngineSettingsStore.setMethod, json: "{}")
        let store = EngineSettingsStore(engine: engine)
        let values = try await store.get(["a"])
        XCTAssertEqual(values["a"], .bool(true))
        try await store.set("a", .bool(false))
        XCTAssertEqual(engine.recordedCalls(), ["settings.get", "settings.set"])
    }
}

final class HubThemeTests: XCTestCase {
    func testBrandAccentReplacesSuiteAccentAndUnknownTokensAreListed() {
        let brand = HubBrand(accent: HubColor(hex: 0x0A84FF), light: ["not-a-token": HubColor(hex: 0)])
        XCTAssertEqual(brand.unknownTokens, ["not-a-token"])
        let theme = HubTheme.standard.branded(brand)
        XCTAssertEqual(theme.palette(.light).accent, HubColor(hex: 0x0A84FF))
    }

    func testHexColorComponents() {
        let color = HubColor(hex: 0xFF8000)
        XCTAssertEqual(color.red, 1)
        XCTAssertEqual(color.green, 128.0 / 255, accuracy: 1e-9)
        XCTAssertEqual(color.blue, 0)
    }
}

final class HubQAServicesTests: XCTestCase {
    func testRecordingServicesLogCallsAndAnswerFromFields() async throws {
        let services = RecordingPlatformServices(permissions: ["mic": .granted])
        try await services.revealFile("/tmp/a")
        try await services.writeClipboardText("hi")
        let clipboard = try await services.readClipboardText()
        XCTAssertEqual(clipboard, "hi")
        let mic = await services.permissionStatus("mic")
        let other = await services.permissionStatus("other")
        XCTAssertEqual(mic, .granted)
        XCTAssertEqual(other, .unsupported)
        XCTAssertEqual(services.recordedCalls(), ["revealFile /tmp/a", "writeClipboardText"])
    }
}
