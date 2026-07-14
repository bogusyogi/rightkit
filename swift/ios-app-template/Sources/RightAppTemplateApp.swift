import RightKitArtifacts
import RightKitDiagnostics
import RightKitLicense
import SwiftUI

// A blank app that only exercises the shared package: write one diagnostic and name the artifact
// installer. No product behaviour, no brand — the point is that RightKitSwift resolves and links in
// an iOS app target.
@main
struct RightAppTemplateApp: App {
    var body: some Scene {
        WindowGroup {
            VStack(spacing: 8) {
                Text("Right Suite iOS template")
                Text(Self.summary).font(.footnote).monospaced()
            }
            .padding()
        }
    }

    static let summary: String = {
        let store = JSONLDiagnosticStore(
            fileURL: FileManager.default.temporaryDirectory.appendingPathComponent("template.jsonl"),
        )
        store.record(DiagnosticEvent(name: "template_launched", fields: ["source": "ios-app-template"]))
        return "diagnostics + \(String(describing: ArtifactInstaller.self)) linked"
    }()
}
