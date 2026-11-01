#if os(macOS)
import Foundation

/// Number and date wording shared by the sections, matching the Tauri hub's formatting
/// (`hub/src/metrics.ts`). Sections add local helpers for anything else.
public enum Format {
    /// Decimal units, as Finder and the Tauri hub show sizes: "1.2 GB", "980 MB", "0 bytes".
    public static func bytes(_ value: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: value, countStyle: .file)
    }

    /// "42%" from 0...1.
    public static func percent(_ fraction: Double) -> String {
        "\(Int((fraction * 100).rounded()))%"
    }

    /// "5 min ago", "yesterday": relative to now.
    public static func relative(_ date: Date, now: Date = Date()) -> String {
        #if os(Windows)
        // Foundation on Windows has no RelativeDateTimeFormatter (SwiftRight porting map, item 10):
        // the same short English wording, worked out here.
        let seconds = Int(now.timeIntervalSince(date))
        let past = seconds >= 0
        let span = abs(seconds)
        let (value, unit): (Int, String) =
            span < 60 ? (span, "sec.") : span < 3600 ? (span / 60, "min.") : span < 86_400 ? (span / 3600, "hr.")
            : span < 604_800 ? (span / 86_400, span / 86_400 == 1 ? "day" : "days")
            : (span / 604_800, span / 604_800 == 1 ? "wk." : "wk.")
        return past ? "\(value) \(unit) ago" : "in \(value) \(unit)"
        #else
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: now)
        #endif
    }

    /// Milliseconds since the epoch, as the engine sends times.
    public static func date(ms: Double) -> Date { Date(timeIntervalSince1970: ms / 1000) }
}
#endif
