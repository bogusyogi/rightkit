import CoreGraphics
import Foundation
import XCTest
@testable import RightKitHotkeys

#if os(macOS)
/// Pure logic only: no tap is created and no event is posted, so no permission is needed.
final class RightKitEventTapTests: XCTestCase {
    private final class Calls: @unchecked Sendable {
        private let lock = NSLock()
        private var names: [String] = []
        func add(_ name: String) { lock.lock(); names.append(name); lock.unlock() }
        var all: [String] { lock.lock(); defer { lock.unlock() }; return names }
    }

    private func keyEvent() throws -> CGEvent {
        try XCTUnwrap(CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true))
    }

    func testChainRunsHighestPriorityFirstThenRegistrationOrder() throws {
        let calls = Calls()
        var chain = RightKitTapChain()
        chain.add(priority: 1) { _, _ in calls.add("low"); return .pass }
        chain.add(priority: 10) { _, _ in calls.add("high-a"); return .pass }
        chain.add(priority: 10) { _, _ in calls.add("high-b"); return .pass }
        XCTAssertEqual(chain.decide(.keyDown, try keyEvent()), .pass)
        XCTAssertEqual(calls.all, ["high-a", "high-b", "low"])
    }

    func testFirstSwallowWinsAndStopsTheChain() throws {
        let calls = Calls()
        var chain = RightKitTapChain()
        chain.add(priority: 5) { _, _ in calls.add("first"); return .swallow }
        chain.add(priority: 1) { _, _ in calls.add("second"); return .pass }
        XCTAssertEqual(chain.decide(.keyDown, try keyEvent()), .swallow)
        XCTAssertEqual(calls.all, ["first"])
    }

    func testRemoveByToken() throws {
        var chain = RightKitTapChain()
        let token = chain.add(priority: 1) { _, _ in .swallow }
        XCTAssertEqual(chain.count, 1)
        chain.remove(token)
        XCTAssertEqual(chain.count, 0)
        XCTAssertEqual(chain.decide(.keyDown, try keyEvent()), .pass)
    }

    func testMaskSetsOneBitPerEventType() {
        let mask = RightKitEventTapMask.mask(for: [.keyDown, .leftMouseUp])
        XCTAssertEqual(mask, (CGEventMask(1) << CGEventMask(CGEventType.keyDown.rawValue))
            | (CGEventMask(1) << CGEventMask(CGEventType.leftMouseUp.rawValue)))
        XCTAssertEqual(RightKitEventTapMask.mask(for: []), 0)
    }

    func testDisableThrottleNotesOncePerInterval() {
        var throttle = RightKitTapDisableThrottle(interval: 60)
        XCTAssertTrue(throttle.shouldNote(now: 1000))
        XCTAssertFalse(throttle.shouldNote(now: 1030))
        XCTAssertTrue(throttle.shouldNote(now: 1060))
    }

    func testLiftCarriesRemainingFlagsAndSkipsNothing() {
        let shift = RightKitModifierKey(keyCode: 56, flagsRawValue: CGEventFlags.maskShift.rawValue)
        let option = RightKitModifierKey(keyCode: 58, flagsRawValue: CGEventFlags.maskAlternate.rawValue)
        let steps = RightKitKeystrokePlanner.lift([shift, option])
        XCTAssertEqual(steps, [
            RightKitKeyStep(keyCode: 56, isDown: false, flagsRawValue: CGEventFlags.maskAlternate.rawValue),
            RightKitKeyStep(keyCode: 58, isDown: false, flagsRawValue: 0),
        ])
    }

    func testLiftKeepsSharedFlagWhileTheOtherSideIsStillHeld() {
        let left = RightKitModifierKey(keyCode: 56, flagsRawValue: CGEventFlags.maskShift.rawValue)
        let right = RightKitModifierKey(keyCode: 60, flagsRawValue: CGEventFlags.maskShift.rawValue)
        let steps = RightKitKeystrokePlanner.lift([left, right])
        XCTAssertEqual(steps.first?.flagsRawValue, CGEventFlags.maskShift.rawValue)
        XCTAssertEqual(steps.last?.flagsRawValue, 0)
    }

    func testRestoreSkipsKeysReleasedMeanwhileAndAccumulatesFlags() {
        let shift = RightKitModifierKey(keyCode: 56, flagsRawValue: CGEventFlags.maskShift.rawValue)
        let option = RightKitModifierKey(keyCode: 58, flagsRawValue: CGEventFlags.maskAlternate.rawValue)
        let control = RightKitModifierKey(keyCode: 59, flagsRawValue: CGEventFlags.maskControl.rawValue)
        let steps = RightKitKeystrokePlanner.restore([shift, option, control]) { $0 != 58 }
        XCTAssertEqual(steps.map(\.keyCode), [56, 59])
        XCTAssertTrue(steps.allSatisfy(\.isDown))
        XCTAssertEqual(steps[1].flagsRawValue, CGEventFlags.maskShift.rawValue | CGEventFlags.maskControl.rawValue)
        XCTAssertTrue(RightKitKeystrokePlanner.restore([shift]) { _ in false }.isEmpty)
    }

    func testChordIsDownThenUp() {
        let steps = RightKitKeystrokePlanner.chord(keyCode: 9, flagsRawValue: CGEventFlags.maskCommand.rawValue)
        XCTAssertEqual(steps.map(\.isDown), [true, false])
        XCTAssertEqual(Set(steps.map(\.flagsRawValue)), [CGEventFlags.maskCommand.rawValue])
    }

    func testLiftableModifiersExcludeCommand() {
        let flags = RightKitKeystrokePlanner.liftableModifiers.map(\.flagsRawValue)
        XCTAssertFalse(flags.contains(CGEventFlags.maskCommand.rawValue))
        XCTAssertEqual(RightKitKeystrokePlanner.liftableModifiers.count, 6)
    }

    func testUTF16ChunksNeverSplitSurrogatePairs() {
        let text = String(repeating: "a", count: 19) + "\u{1F600}" + "bc"
        let chunks = RightKitKeystrokePlanner.utf16Chunks(text, size: 20)
        XCTAssertEqual(chunks.flatMap { $0 }, Array(text.utf16))
        for chunk in chunks {
            XCTAssertLessThanOrEqual(chunk.count, 20)
            if let last = chunk.last { XCTAssertFalse(UTF16.isLeadSurrogate(last)) }
        }
        XCTAssertEqual(chunks.count, 2)
        XCTAssertTrue(RightKitKeystrokePlanner.utf16Chunks("").isEmpty)
    }

    func testParseCombo() {
        XCTAssertEqual(RightKitHotkeyCombo.parse("cmd+opt+k"),
                       RightKitHotkeyCombo(keyCode: 40, modifiers: [.command, .option]))
        XCTAssertEqual(RightKitHotkeyCombo.parse("Ctrl + Shift + 1"),
                       RightKitHotkeyCombo(keyCode: 18, modifiers: [.control, .shift]))
        XCTAssertEqual(RightKitHotkeyCombo.parse("alt+space")?.modifiers, .option)
        XCTAssertEqual(RightKitHotkeyCombo.parse("control+f5")?.keyCode, 96)
    }

    func testParseRejectsPlainKeysAndUnknownNames() {
        XCTAssertNil(RightKitHotkeyCombo.parse("k"))
        XCTAssertNil(RightKitHotkeyCombo.parse("cmd+"))
        XCTAssertNil(RightKitHotkeyCombo.parse("cmd+nope"))
        XCTAssertNil(RightKitHotkeyCombo.parse("hyper+k"))
        XCTAssertNil(RightKitHotkeyCombo.parse(""))
    }

    func testModifiersRoundTripThroughEventFlags() {
        let all: RightKitHotkeyModifiers = [.command, .shift, .option, .control]
        XCTAssertEqual(RightKitHotkeyModifiers(cgEventFlags: all.cgEventFlags), all)
        XCTAssertEqual(RightKitHotkeyModifiers([]).cgEventFlags, [])
        XCTAssertEqual(RightKitHotkeyModifiers(cgEventFlags: [.maskCommand, .maskSecondaryFn]), .command)
    }
}
#endif
