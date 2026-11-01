import SwiftUI
import Foundation

/// Process-wide snapshot of iOS 27's resource preference for non-SwiftUI
/// callbacks. The environment remains the authority; this only makes its
/// current value available to WatchConnectivity delegates.
final class HRSystemResourceUsageState: @unchecked Sendable {
    static let shared = HRSystemResourceUsageState()

    private let lock = NSLock()
    private var value = false

    var prefersReducedResourceUsage: Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    func update(_ value: Bool) {
        lock.lock()
        self.value = value
        lock.unlock()
    }
}

/// Reads iOS 27's resource-pressure preference without raising this app's
/// deployment requirement above iOS 17.
struct HRSystemResourceUsageReader<Content: View>: View {
    private let content: (Bool) -> Content

    init(@ViewBuilder content: @escaping (Bool) -> Content) {
        self.content = content
    }

    @ViewBuilder
    var body: some View {
        if #available(iOS 27.0, *) {
            HRSystemResourceUsageReader27(content: content)
        } else {
            let _ = HRSystemResourceUsageState.shared.update(false)
            content(false)
                .onAppear {
                    HRSystemResourceUsageState.shared.update(false)
                }
        }
    }
}

@available(iOS 27.0, *)
private struct HRSystemResourceUsageReader27<Content: View>: View {
    @Environment(\.systemPrefersReducedResourceUsage) private var reducedResourceUsage
    let content: (Bool) -> Content

    var body: some View {
        let _ = HRSystemResourceUsageState.shared.update(reducedResourceUsage)
        content(reducedResourceUsage)
            .onAppear {
                HRSystemResourceUsageState.shared.update(reducedResourceUsage)
            }
            .onChange(of: reducedResourceUsage) { _, value in
                HRSystemResourceUsageState.shared.update(value)
            }
    }
}
