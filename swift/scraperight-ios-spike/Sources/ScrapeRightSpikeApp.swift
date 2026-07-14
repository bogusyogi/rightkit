import SwiftUI

// The spike's real proof lives in ScrapeRightSpikeTests (install + diagnostics through the shared
// package). The app target only has to build and launch; it carries no ScrapeRight product code.
@main
struct ScrapeRightSpikeApp: App {
    var body: some Scene {
        WindowGroup { Text("ScrapeRight iOS spike").padding() }
    }
}
