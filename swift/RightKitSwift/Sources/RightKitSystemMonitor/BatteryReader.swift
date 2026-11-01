#if os(macOS)
import Foundation
import IOKit
import IOKit.ps

/// Pulse fork: the internal battery's charge, charging state, cycle count,
/// health, temperature, power draw and time remaining, for the System sensors.
/// Charge and state come from the power source list (IOPowerSources); the rest
/// comes from the AppleSmartBattery registry entry. A Mac without an internal
/// battery returns nil, and the rows are left out. Any figure the registry does
/// not publish, or publishes as unknown, is nil and is left out of the view.
enum BatteryReader {
    struct Reading {
        /// 0...100.
        let percent: Int
        let charging: Bool
        let cycles: Int?
        /// Full-charge capacity as a share of the design capacity, 0...1.
        let health: Double?
        /// Battery temperature in °C.
        let celsius: Double?
        /// Signed power in watts: positive while charging, negative while discharging.
        let watts: Double?
        /// Minutes to empty while discharging, or to full while charging.
        let minutesRemaining: Int?
        /// True when `minutesRemaining` counts down to full (the battery is charging).
        let toFull: Bool
    }

    static func read() -> Reading? {
        guard let charge = powerSourceCharge() else { return nil }
        let registry = registryFigures()
        let minutes = charge.charging
            ? (registry.timeRemaining ?? registry.avgToFull)
            : (registry.timeRemaining ?? registry.avgToEmpty)
        return Reading(percent: charge.percent, charging: charge.charging,
                       cycles: registry.cycles, health: registry.health,
                       celsius: registry.celsius, watts: registry.watts,
                       minutesRemaining: minutes, toFull: charge.charging)
    }

    private static func powerSourceCharge() -> (percent: Int, charging: Bool)? {
        let info = IOPSCopyPowerSourcesInfo().takeRetainedValue()
        guard let sources = IOPSCopyPowerSourcesList(info)?.takeRetainedValue() as? [CFTypeRef] else {
            return nil
        }
        for source in sources {
            guard let description = IOPSGetPowerSourceDescription(info, source)?
                    .takeUnretainedValue() as? [String: Any],
                  description["Type"] as? String == "InternalBattery",
                  let current = (description["Current Capacity"] as? NSNumber)?.doubleValue,
                  let maximum = (description["Max Capacity"] as? NSNumber)?.doubleValue,
                  maximum > 0
            else { continue }
            let percent = min(max(Int((current / maximum * 100).rounded()), 0), 100)
            return (percent, description["Is Charging"] as? Bool ?? false)
        }
        return nil
    }

    /// Raw figures from the battery's own registry entry. Each may be missing on
    /// a machine that does not publish it; times use 65535 for unknown.
    private struct Registry {
        var cycles: Int?
        var health: Double?
        var celsius: Double?
        var watts: Double?
        var timeRemaining: Int?
        var avgToEmpty: Int?
        var avgToFull: Int?
    }

    private static func registryFigures() -> Registry {
        var figures = Registry()
        let service = IOServiceGetMatchingService(kIOMainPortDefault,
                                                  IOServiceMatching("AppleSmartBattery"))
        guard service != 0 else { return figures }
        defer { IOObjectRelease(service) }
        var properties: Unmanaged<CFMutableDictionary>?
        guard IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)
                == KERN_SUCCESS,
              let dictionary = properties?.takeRetainedValue() as? [String: Any]
        else { return figures }
        figures.cycles = (dictionary["CycleCount"] as? NSNumber).map(\.intValue)
        let design = (dictionary["DesignCapacity"] as? NSNumber)?.doubleValue ?? 0
        let full = ((dictionary["AppleRawMaxCapacity"] ?? dictionary["NominalChargeCapacity"])
            as? NSNumber)?.doubleValue ?? 0
        figures.health = design > 0 && full > 0 ? min(full / design, 1) : nil

        // Temperature is published in hundredths of a degree; 0 or an implausible value is treated as missing.
        if let raw = (dictionary["Temperature"] as? NSNumber)?.doubleValue {
            let celsius = raw / 100
            figures.celsius = celsius > 0 && celsius < 100 ? celsius : nil
        }

        // Amperage is signed (positive charging). InstantAmperage is preferred when published.
        // A negative value stored as an unsigned 64-bit number reads back as negative through int64Value.
        let amperage = ((dictionary["InstantAmperage"] as? NSNumber)
            ?? (dictionary["Amperage"] as? NSNumber))?.int64Value
        let millivolts = (dictionary["Voltage"] as? NSNumber)?.doubleValue ?? 0
        if let amperage, millivolts > 0 {
            // mA × mV is microwatts.
            let watts = Double(amperage) * millivolts / 1_000_000
            figures.watts = abs(watts) <= 300 ? watts : nil
        }

        func minutes(_ key: String) -> Int? {
            guard let value = (dictionary[key] as? NSNumber)?.intValue,
                  value > 0, value < 65535
            else { return nil }
            return value
        }
        figures.timeRemaining = minutes("TimeRemaining")
        figures.avgToEmpty = minutes("AvgTimeToEmpty")
        figures.avgToFull = minutes("AvgTimeToFull")
        return figures
    }
}
#endif
