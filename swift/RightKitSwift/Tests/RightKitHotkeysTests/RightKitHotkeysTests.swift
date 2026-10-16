import Foundation
import XCTest
@testable import RightKitHotkeys

#if os(macOS)
final class RightKitHotkeysTests: XCTestCase {
    func testModifierMasksMatchCarbon() {
        XCTAssertEqual(RightKitHotkeyModifiers.command.rawValue, 256)
        XCTAssertEqual(RightKitHotkeyModifiers.shift.rawValue, 512)
        XCTAssertEqual(RightKitHotkeyModifiers.option.rawValue, 2048)
        XCTAssertEqual(RightKitHotkeyModifiers.control.rawValue, 4096)
    }

    func testSymbolsUseMenuOrder() {
        let all: RightKitHotkeyModifiers = [.command, .shift, .option, .control]
        XCTAssertEqual(all.symbols, "\u{2303}\u{2325}\u{21E7}\u{2318}")
        XCTAssertEqual(RightKitHotkeyModifiers([]).symbols, "")
    }

    func testComboRoundTripsThroughCodable() throws {
        let combo = RightKitHotkeyCombo(keyCode: 49, modifiers: [.option, .command])
        let data = try JSONEncoder().encode(combo)
        XCTAssertEqual(try JSONDecoder().decode(RightKitHotkeyCombo.self, from: data), combo)
        XCTAssertTrue(combo.hasModifier)
        XCTAssertFalse(RightKitHotkeyCombo(keyCode: 49, modifiers: []).hasModifier)
    }

    /// Registers and releases a real hot key. Carbon needs no permission, so this runs unattended.
    /// Ctrl+Opt+Shift+Cmd+F15 is unlikely to be owned by anything else.
    @MainActor
    func testRegisterDuplicateAndUnregister() throws {
        let registrar = RightKitHotkeyRegistrar()
        let combo = RightKitHotkeyCombo(keyCode: 113, modifiers: [.control, .option, .shift, .command])
        let id = try registrar.register(combo) { _ in }
        XCTAssertEqual(registrar.registeredCombos, [combo])
        XCTAssertThrowsError(try registrar.register(combo) { _ in }) {
            XCTAssertEqual($0 as? RightKitHotkeyError, .alreadyRegistered(combo))
        }
        registrar.unregister(id)
        XCTAssertTrue(registrar.registeredCombos.isEmpty)
        XCTAssertNoThrow(try registrar.register(combo) { _ in })
        registrar.unregisterAll()
    }
}
#endif
