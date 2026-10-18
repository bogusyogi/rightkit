import Foundation
import Testing

@testable import RightKitStore

private func settingsConfig() -> AppGroupConfiguration {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-settings-\(UUID().uuidString)")
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let suite = UserDefaults(suiteName: "rk-settings-\(UUID().uuidString)")!
    return AppGroupConfiguration(groupID: "group.test", lockDirectory: dir, defaultsOverride: suite)
}

private func manifest() throws -> AppSettingsManifest {
    try AppSettingsManifest([
        AppSettingDefinition(key: "theme", kind: .string, defaultValue: .string("dark")),
        AppSettingDefinition(key: "fontPx", kind: .int, defaultValue: .int(13)),
        AppSettingDefinition(key: "scale", kind: .double, defaultValue: .double(1.0)),
        AppSettingDefinition(key: "notch", kind: .bool, defaultValue: .bool(true)),
        AppSettingDefinition(key: "licenseHint", kind: .string, defaultValue: .string(""), isProtected: true),
    ])
}

@Test func settingsReadDefaultsThenPatchedValuesWithRevision() throws {
    let s = AppSettingsStore(configuration: settingsConfig(), manifest: try manifest())
    #expect(s.value("theme") == .string("dark") && s.value("fontPx") == .int(13))
    #expect(s.value("missing") == nil && s.revision == 0)
    let snap = try s.patch(["theme": .string("light"), "fontPx": .int(15), "notch": .bool(false), "scale": .double(1.5)])
    #expect(snap["theme"] == .string("light") && snap["fontPx"] == .int(15) && snap["notch"] == .bool(false) && snap["scale"] == .double(1.5))
    #expect(s.revision == 1)
    // A patch that changes nothing writes nothing.
    try s.patch(["theme": .string("light")])
    #expect(s.revision == 1)
}

@Test func settingsPatchIsAllOrNothingAndTyped() throws {
    let s = AppSettingsStore(configuration: settingsConfig(), manifest: try manifest())
    #expect(throws: AppSettingsError.undeclaredKey("nope")) { try s.patch(["theme": .string("x"), "nope": .bool(true)]) }
    #expect(throws: AppSettingsError.typeMismatch(key: "fontPx", expected: .int)) { try s.patch(["theme": .string("x"), "fontPx": .string("big")]) }
    #expect(throws: AppSettingsError.notAllowed("theme")) { try s.patch(["theme": .string("x")], allow: ["fontPx"]) }
    #expect(s.value("theme") == .string("dark") && s.revision == 0, "a refused patch changes nothing")
}

@Test func settingsProtectedKeysNeedTheOwnerFlag() throws {
    let s = AppSettingsStore(configuration: settingsConfig(), manifest: try manifest())
    #expect(throws: AppSettingsError.protectedKey("licenseHint")) { try s.patch(["licenseHint": .string("x")]) }
    try s.patch(["licenseHint": .string("x")], includeProtected: true)
    #expect(s.value("licenseHint") == .string("x"))
}

@Test func settingsNullResetsToDefaultAndWrongTypeStoredValueReadsAsDefault() throws {
    let cfg = settingsConfig()
    let s = AppSettingsStore(configuration: cfg, manifest: try manifest())
    try s.patch(["fontPx": .int(20)])
    try s.reset(["fontPx"])
    #expect(s.value("fontPx") == .int(13) && cfg.defaults?.object(forKey: "fontPx") == nil)
    cfg.defaults?.set("garbage", forKey: "fontPx")
    #expect(s.value("fontPx") == .int(13), "a stored value of the wrong type never leaks out")
    cfg.defaults?.set(true, forKey: "fontPx")
    #expect(s.value("fontPx") == .int(13), "a bool is not an int")
}

@Test func settingsSubscribersSeeOwnWritesAndExternalRevisionMoves() throws {
    let cfg = settingsConfig()
    let a = AppSettingsStore(configuration: cfg, manifest: try manifest())
    let b = AppSettingsStore(configuration: cfg, manifest: try manifest())
    final class Seen: @unchecked Sendable { var changes: [AppSettingsStore.Change] = [] }
    let seen = Seen()
    b.subscribe { seen.changes.append($0) }
    #expect(b.pollExternal() == nil)
    try a.patch(["theme": .string("light")])
    let change = b.pollExternal()
    #expect(change?.revision == 1 && seen.changes.count == 1)
    #expect(b.pollExternal() == nil)
    try b.patch(["notch": .bool(false)])
    #expect(seen.changes.last?.keys == ["notch"] && seen.changes.last?.revision == 2)
}

@Test func settingsManifestDecodesJSONAndRejectsBadDefaults() throws {
    let json = Data(#"{"settings":[{"key":"a","kind":"bool","default":true,"protected":true},{"key":"b","kind":"int","default":3},{"key":"c","kind":"double","default":0.5},{"key":"d","kind":"string","default":"x"}]}"#.utf8)
    let m = try AppSettingsManifest.decode(json: json)
    #expect(m.definitions.map(\.key) == ["a", "b", "c", "d"] && m.definition(for: "a")?.isProtected == true)
    #expect(m.definition(for: "b")?.defaultValue == .int(3))
    for bad in [
        #"{"settings":[{"key":"a","kind":"int","default":true}]}"#,
        #"{"settings":[{"key":"a","kind":"int","default":1.5}]}"#,
        #"{"settings":[{"key":"a","kind":"bool","default":1}]}"#,
        #"{"settings":[{"key":"a","kind":"bool","default":true},{"key":"a","kind":"bool","default":false}]}"#,
        #"{"settings":[{"key":"a","kind":"nope","default":1}]}"#,
        #"[]"#,
    ] {
        #expect(throws: AppSettingsError.self) { try AppSettingsManifest.decode(json: Data(bad.utf8)) }
    }
}

@Test func settingsLockUnavailableFailsClosed() throws {
    var cfg = settingsConfig()
    cfg.lockDirectory = FileManager.default.temporaryDirectory.appendingPathComponent("missing-\(UUID().uuidString)")
    let s = AppSettingsStore(configuration: cfg, manifest: try manifest())
    #expect(throws: AppSettingsError.lockUnavailable) { try s.patch(["theme": .string("x")]) }
    #expect(s.value("theme") == .string("dark"))
}
