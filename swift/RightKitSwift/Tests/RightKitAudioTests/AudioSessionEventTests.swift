import Foundation
import Testing

@testable import RightKitAudio

struct AudioSessionEventMapperTests {
    @Test func interruptionTypeAndResumeOptionMapToEvents() {
        #expect(AudioSessionEventMapper.interruption(typeRaw: 1, optionsRaw: nil) == .interruptionBegan)
        #expect(AudioSessionEventMapper.interruption(typeRaw: 0, optionsRaw: 1) == .interruptionEnded(shouldResume: true))
        #expect(AudioSessionEventMapper.interruption(typeRaw: 0, optionsRaw: 0) == .interruptionEnded(shouldResume: false))
        #expect(AudioSessionEventMapper.interruption(typeRaw: 0, optionsRaw: nil) == .interruptionEnded(shouldResume: false))
        #expect(AudioSessionEventMapper.interruption(typeRaw: nil, optionsRaw: 1) == nil)
        #expect(AudioSessionEventMapper.interruption(typeRaw: 9, optionsRaw: 1) == nil)
    }

    @Test func routeChangeReasonsMapAndOnlyInputLossesAreFlagged() {
        #expect(AudioSessionEventMapper.routeChange(reasonRaw: nil) == nil)
        #expect(AudioRouteChangeReason(rawValue: 1) == .newDeviceAvailable)
        #expect(AudioRouteChangeReason(rawValue: 2) == .oldDeviceUnavailable)
        #expect(AudioRouteChangeReason(rawValue: 3) == .categoryChange)
        #expect(AudioRouteChangeReason(rawValue: 4) == .override)
        #expect(AudioRouteChangeReason(rawValue: 6) == .wakeFromSleep)
        #expect(AudioRouteChangeReason(rawValue: 7) == .noSuitableRouteForCategory)
        #expect(AudioRouteChangeReason(rawValue: 8) == .routeConfigurationChange)
        #expect(AudioRouteChangeReason(rawValue: 5) == .unknown)
        #expect(AudioRouteChangeReason(rawValue: 99) == .unknown)

        #expect(AudioRouteChangeReason.oldDeviceUnavailable.losesActiveInput)
        #expect(AudioRouteChangeReason.noSuitableRouteForCategory.losesActiveInput)
        #expect(!AudioRouteChangeReason.newDeviceAvailable.losesActiveInput)
        #expect(!AudioRouteChangeReason.categoryChange.losesActiveInput)
        #expect(AudioSessionEventMapper.routeChange(reasonRaw: 2) == .routeChanged(.oldDeviceUnavailable))
    }

    @Test func monitorStartAndStopAreIdempotent() {
        let monitor = AudioSessionMonitor { _ in }
        monitor.stop()
        #expect(!monitor.isObserving)
        monitor.start()
        monitor.start()
        monitor.stop()
        monitor.stop()
        #expect(!monitor.isObserving)
    }
}
