import RightKitArtifacts
import RightKitDiagnostics
import RightKitLicense
import SwiftUI

// Blank host: it only exercises the shared package and the wiring files beside it. No product
// behaviour, no brand.
@main
struct RightAppTemplateApp: App {
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            VStack(spacing: 8) {
                Text("Right Suite iOS template").accessibilityIdentifier("template.title")
                Text(Self.summary).font(.footnote).monospaced().accessibilityIdentifier("template.summary")
                Text(ExtensionWiring.status).font(.footnote).accessibilityIdentifier("template.wiring")
            }
            .padding()
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { ExtensionWiring.drainShareQueue() }
        }
    }

    static let summary: String = {
        let store = JSONLDiagnosticStore(
            fileURL: FileManager.default.temporaryDirectory.appendingPathComponent("template.jsonl"),
        )
        store.record(DiagnosticEvent(name: "template_launched", fields: ["source": "ios-app-template"]))
        return "diagnostics + \(String(describing: ArtifactInstaller.self)) + \(String(describing: LicenseClaims.self)) linked"
    }()
}
