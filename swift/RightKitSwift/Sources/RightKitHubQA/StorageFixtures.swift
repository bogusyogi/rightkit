import Foundation
import RightKitHubQA

extension HubFixtures {
    /// Canned `pulse-engine` replies for the Storage section, shaped like the real commands'
    /// results. Invented data under `/Users/demo`; times are relative to the moment the engine is built.
    static func storage(_ engine: QAEngineClient) {
        let now = Int(Date().timeIntervalSince1970)
        let day = 86_400

        engine.respond(to: "volumes", json: """
        [
          {"name":"Macintosh HD","mount_point":"/","total_bytes":994662584320,"available_bytes":312400000000,"reclaimable_bytes":42000000000,"removable":false,"internal":true,"disk_image":false,"kind":"local","detail":null,"state":"ok"},
          {"name":"Demo Backup","mount_point":"/Volumes/Demo Backup","total_bytes":2000398934016,"available_bytes":1421000000000,"reclaimable_bytes":null,"removable":true,"internal":false,"disk_image":false,"kind":"local","detail":null,"state":"ok"},
          {"name":"Team Share","mount_point":"/Volumes/Team Share","total_bytes":4000787030016,"available_bytes":1100000000000,"reclaimable_bytes":null,"removable":false,"internal":false,"disk_image":false,"kind":"network","detail":"nas.local/team","state":"ok"},
          {"name":"Demo Installer","mount_point":"/Volumes/Demo Installer","total_bytes":145000000,"available_bytes":0,"reclaimable_bytes":null,"removable":false,"internal":false,"disk_image":true,"kind":"local","detail":null,"state":"ok"}
        ]
        """)

        engine.respond(to: "scan_status", json: """
        {"running":false,"running_root":null,"has_index":true,"root":"/Users/demo","scanned_at":\(now - 120),"from_snapshot":false,"live_refresh":true}
        """)

        let rows = """
        [
          {"path":"/Users/demo/Library","name":"Library","is_dir":true,"bytes":182400000000,"modified":\(now - 2 * day)},
          {"path":"/Users/demo/Movies","name":"Movies","is_dir":true,"bytes":96300000000,"modified":\(now - 9 * day)},
          {"path":"/Users/demo/Developer","name":"Developer","is_dir":true,"bytes":74800000000,"modified":\(now - 1 * day)},
          {"path":"/Users/demo/Documents","name":"Documents","is_dir":true,"bytes":41200000000,"modified":\(now - 4 * day)},
          {"path":"/Users/demo/Downloads","name":"Downloads","is_dir":true,"bytes":28900000000,"modified":\(now - 1 * day)},
          {"path":"/Users/demo/Pictures","name":"Pictures","is_dir":true,"bytes":22100000000,"modified":\(now - 40 * day)},
          {"path":"/Users/demo/Applications","name":"Applications","is_dir":true,"bytes":14600000000,"modified":\(now - 120 * day)},
          {"path":"/Users/demo/Music","name":"Music","is_dir":true,"bytes":8400000000,"modified":\(now - 400 * day)},
          {"path":"/Users/demo/Desktop","name":"Desktop","is_dir":true,"bytes":3100000000,"modified":\(now - 3 * day)},
          {"path":"/Users/demo/Backup-2025.zip","name":"Backup-2025.zip","is_dir":false,"bytes":2600000000,"modified":\(now - 300 * day)},
          {"path":"/Users/demo","name":"Smaller files","is_dir":false,"bytes":1900000000,"summary":true}
        ]
        """
        let folder = """
        {"path":"/Users/demo","root":"/Users/demo","rows":\(rows),"total_children":24,"incomplete":false,"needs_access":false,"unread":0,"limited":false,"root_label":"Home","scanned_at":\(now - 120),"from_snapshot":false,"has_dates":true}
        """
        // A folder of pictures, for the picture of the inspector's image preview.
        let pictureRows = """
        [
          {"path":"/Users/demo/Pictures/Photos Library.photoslibrary","name":"Photos Library.photoslibrary","is_dir":true,"bytes":19800000000,"modified":\(now - 40 * day)},
          {"path":"/Users/demo/Pictures/Trip 2025","name":"Trip 2025","is_dir":true,"bytes":1900000000,"modified":\(now - 120 * day)},
          {"path":"/Users/demo/Pictures/Screenshots","name":"Screenshots","is_dir":true,"bytes":310000000,"modified":\(now - 6 * day)},
          {"path":"/Users/demo/Pictures/Market stalls.jpg","name":"Market stalls.jpg","is_dir":false,"bytes":3900000,"modified":\(now - 75 * day)},
          {"path":"/Users/demo/Pictures/Harbor at dawn.png","name":"Harbor at dawn.png","is_dir":false,"bytes":2400000,"modified":\(now - 31 * day)},
          {"path":"/Users/demo/Pictures/Notes diagram.webp","name":"Notes diagram.webp","is_dir":false,"bytes":410000,"modified":\(now - 9 * day)}
        ]
        """
        let pictures = """
        {"path":"/Users/demo/Pictures","root":"/Users/demo","rows":\(pictureRows),"total_children":6,"incomplete":false,"needs_access":false,"unread":0,"limited":false,"root_label":"Home","scanned_at":\(now - 120),"from_snapshot":false,"has_dates":true}
        """
        engine.respond(to: "last_scan", json: folder)
        engine.respond(to: "scan", json: folder)
        engine.respond(to: "children") { params in
            let path = (try? JSONSerialization.jsonObject(with: params) as? [String: Any])?["path"] as? String
            return Data((path == "/Users/demo/Pictures" ? pictures : folder).utf8)
        }
        engine.respond(to: "search", json: """
        {"rows":[{"path":"/Users/demo/Documents/Taxes/2025-summary.pdf","name":"2025-summary.pdf","is_dir":false,"bytes":4200000,"modified":\(now - 20 * day)}],"undated":0,"has_dates":true}
        """)

        engine.respond(to: "growth", json: """
        {"available":true,"since":\(now - 2 * day),"grown":[{"path":"/Users/demo/Library/Developer","bytes":3200000000},{"path":"/Users/demo/Movies","bytes":1100000000}],"shrunk":[{"path":"/Users/demo/Downloads","bytes":-2400000000}],"reason":null}
        """)

        engine.respond(to: "fda_status", json: "\"granted\"")
        engine.respond(to: "home_path", json: "\"/Users/demo\"")

        func finding(
            _ id: String, rule: String, ruleName: String, name: String, path: String, bytes: Int, risk: String, reason: String,
            eligible: Bool = true, action: String = "Move to the Trash"
        ) -> String {
            """
            {"id":"\(id)","rule_id":"\(rule)","rule_name":"\(ruleName)","category":"developer","name":"\(name)","path":"\(path)","bytes":\(bytes),"apparent_bytes":\(bytes),"partial":false,"risk":"\(risk)","eligible":\(eligible),"preselected":\(risk == "safe"),"reason":"\(reason)","action":"\(action)","dev":"16777234","ino":"\(1000 + id.count * 37)"}
            """
        }
        let findings = [
            finding("f1", rule: "xcode-derived-data", ruleName: "Xcode build files", name: "Xcode build files · the app", path: "/Users/demo/Library/Developer/Xcode/DerivedData/Pulse-abc", bytes: 18_400_000_000, risk: "safe", reason: "Build products Xcode recreates on the next build"),
            finding("f2", rule: "xcode-derived-data", ruleName: "Xcode build files", name: "Xcode build files · Notes", path: "/Users/demo/Library/Developer/Xcode/DerivedData/Notes-def", bytes: 6_200_000_000, risk: "safe", reason: "Build products Xcode recreates on the next build"),
            finding("f3", rule: "npm-cache", ruleName: "npm cache", name: "npm cache", path: "/Users/demo/.npm/_cacache", bytes: 5_900_000_000, risk: "safe", reason: "Downloaded packages npm fetches again when needed"),
            finding("f4", rule: "old-installers", ruleName: "Old installers", name: "Demo-1.2.dmg", path: "/Users/demo/Downloads/Demo-1.2.dmg", bytes: 1_200_000_000, risk: "safe", reason: "Disk images and packages in Downloads older than 30 days"),
            finding("f5", rule: "old-installers", ruleName: "Old installers", name: "Setup.pkg", path: "/Users/demo/Downloads/Setup.pkg", bytes: 840_000_000, risk: "safe", reason: "Disk images and packages in Downloads older than 30 days"),
            finding("f6", rule: "old-installers", ruleName: "Old installers", name: "Tool-3.dmg", path: "/Users/demo/Downloads/Tool-3.dmg", bytes: 410_000_000, risk: "safe", reason: "Disk images and packages in Downloads older than 30 days"),
            finding("f7", rule: "docker-images", ruleName: "Docker images", name: "Docker images", path: "/Users/demo/Library/Containers/com.docker.docker/Data", bytes: 22_700_000_000, risk: "review", reason: "Images and layers; containers you still use will pull them again"),
            finding("f8", rule: "simulator-runtimes", ruleName: "Simulator runtimes", name: "Simulator runtimes", path: "/Users/demo/Library/Developer/CoreSimulator", bytes: 11_300_000_000, risk: "review", reason: "Old iOS simulators; Xcode downloads them again"),
            finding("f9", rule: "gradle-cache", ruleName: "Gradle cache", name: "Gradle cache", path: "/Users/demo/.gradle/caches", bytes: 3_700_000_000, risk: "safe", reason: "Dependencies Gradle downloads again"),
            finding("f10", rule: "xcode-archives", ruleName: "Xcode archives", name: "Xcode archives", path: "/Users/demo/Library/Developer/Xcode/Archives", bytes: 4_100_000_000, risk: "review", reason: "Archived app builds; keep any you still need to submit"),
            finding("f11", rule: "trash", ruleName: "Trash contents", name: "Trash contents", path: "/Users/demo/.Trash", bytes: 2_300_000_000, risk: "safe", reason: "Items you already moved to the Trash"),
            finding("n1", rule: "local-snapshots", ruleName: "Time Machine local snapshots", name: "Time Machine local snapshots", path: "/", bytes: 14_000_000_000, risk: "info", reason: "macOS removes these on its own when space is needed", eligible: false, action: "Run tmutil in Terminal"),
            finding("n2", rule: "spotlight", ruleName: "Spotlight index", name: "Spotlight index", path: "/System/Volumes/Data/.Spotlight-V100", bytes: 1_800_000_000, risk: "info", reason: "In use by macOS; it rebuilds itself", eligible: false, action: "None"),
            finding("n3", rule: "mail", ruleName: "Mail downloads", name: "Mail downloads", path: "/Users/demo/Library/Mail", bytes: 6_100_000_000, risk: "info", reason: "Attachments of your mail; open Mail to remove them", eligible: false, action: "None"),
            finding("n4", rule: "icloud", ruleName: "iCloud Drive cache", name: "iCloud Drive cache", path: "/Users/demo/Library/Mobile Documents", bytes: 2_000_000_000, risk: "info", reason: "Local copies macOS evicts when it needs space", eligible: false, action: "None"),
        ].joined(separator: ",")
        let report = """
        {"chrome_snapshots":{"count":3,"apparent_bytes":3600000000,"reclaimable_bytes":480000000,"reclaimable_known":true,"running":false,"since_at":\(now - 3 * day),"since_count":5},"findings":[\(findings)],"safe_bytes":38950000000,"review_bytes":38100000000,"scanned_at":\(now - 180)}
        """
        engine.respond(to: "cleanup_cached", json: report)
        engine.respond(to: "cleanup_scan", json: report)

        let reading: (Int, Int, Int, Int) -> String = { at, temp, wear, hours in
            """
            {"at":\(at),"passed":true,"temperature_c":\(temp),"wear_percent":\(wear),"written_bytes":41800000000000,"power_on_hours":\(hours),"critical_warning":0,"media_errors":0,"self_tests":[{"kind":"Short offline","result":"Completed without error","passed":true,"power_on_hours":\(hours - 40)}]}
            """
        }
        engine.respond(to: "drive_health", json: """
        {"tool_available":true,"sampled_at":\(now - 300),"drives":[{"mount":"/","disk":"disk0","model":"APPLE SSD AP1024Z","status":"ok","reachable":true,"latest":\(reading(now - 300, 40, 4, 3120)),"history":[\(reading(now - 2 * day, 38, 3, 3072)),\(reading(now - day, 41, 4, 3096)),\(reading(now - 300, 40, 4, 3120))]}],"alerts":[]}
        """)

        let inspectFolder = """
        {"is_dir":true,"is_link":false,"logical_bytes":null,"created_at":\(now - 900 * day),"modified_at":\(now - 2 * day),"files":14,"folders":9}
        """
        let inspectImage = """
        {"is_dir":false,"is_link":false,"logical_bytes":2350000,"created_at":\(now - 31 * day),"modified_at":\(now - 31 * day),"files":null,"folders":null}
        """
        engine.respond(to: "storage_inspect") { params in
            let path = (try? JSONSerialization.jsonObject(with: params) as? [String: Any])?["path"] as? String ?? ""
            let isFile = [".png", ".jpg", ".webp"].contains { path.lowercased().hasSuffix($0) }
            return Data((isFile ? inspectImage : inspectFolder).utf8)
        }
        engine.respond(to: "storage_preview", json: "{\"base64\":\"\(storagePreviewBase64)\"}")
        engine.respond(to: "duplicates_scan", json: """
        {"groups":[{"kept_path":"/Users/demo/Documents/Report.pdf","extras":["/Users/demo/Downloads/Report.pdf","/Users/demo/Desktop/Report copy.pdf"],"size_bytes":8400000}],"skipped":[],"diagnostics":[],"truncated":false,"files_considered":1840,"bytes_read":912000000}
        """)
    }
}
