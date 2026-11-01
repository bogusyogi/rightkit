import Foundation
import RightKitHubQA

extension HubFixtures {
    /// Canned `pulse-engine` replies for the Overview section, shaped like the real commands'
    /// results: `status`, `volumes`, and the saved results `cleanup_cached`, `apps_cached` and
    /// `apps_updates_cached`. The history (`metrics_history`) is registered by `monitor(_:)`, which
    /// the Overview shares.
    static func overview(_ engine: QAEngineClient) {
        let now = Int(Date().timeIntervalSince1970)

        engine.respond(to: "status", json: systemStatusJSON)

        engine.respond(
            to: "volumes",
            json: """
                [
                  {"name":"Macintosh HD","mount_point":"/","total_bytes":994662584320,"available_bytes":312400000000,
                   "reclaimable_bytes":41800000000,"removable":false,"internal":true,"disk_image":false,
                   "kind":"local","detail":null,"state":"ok"},
                  {"name":"Backup SSD","mount_point":"/Volumes/Backup SSD","total_bytes":1000204886016,
                   "available_bytes":96400000000,"reclaimable_bytes":null,"removable":true,"internal":false,
                   "disk_image":false,"kind":"local","detail":null,"state":"ok"},
                  {"name":"Installer","mount_point":"/Volumes/Installer","total_bytes":4800000000,
                   "available_bytes":0,"reclaimable_bytes":null,"removable":true,"internal":false,
                   "disk_image":true,"kind":"local","detail":null,"state":"ok"},
                  {"name":"team-share","mount_point":"/Volumes/team-share","total_bytes":null,"available_bytes":null,
                   "reclaimable_bytes":null,"removable":false,"internal":false,"disk_image":false,
                   "kind":"network","detail":"nas.local/team","state":"not responding"}
                ]
                """)

        engine.respond(
            to: "cleanup_cached",
            json: """
                {"chrome_snapshots":null,"findings":[],"safe_bytes":6234000000,"review_bytes":3120000000,
                 "scanned_at":\(now - 1500)}
                """)

        let apps = (0..<64).map { index in
            """
            {"name":"Demo App \(index + 1)","path":"/Applications/Demo App \(index + 1).app",
             "bundle_id":"com.example.demo\(index + 1)","version":"1.\(index % 9).0",
             "size_bytes":\(180_000_000 + index * 11_000_000),"last_used":\(now - 3600 * (index + 1)),
             "running":false,"protected":null}
            """
        }.joined(separator: ",")
        engine.respond(to: "apps_cached", json: "{\"saved_at\":\(now - 3600),\"apps\":[\(apps)]}")

        let states = ["available", "available", "available", "current", "current", "unknown", "app_store"]
        let updates = states.enumerated().map { index, state in
            """
            {"path":"/Applications/Demo App \(index + 1).app","name":"Demo App \(index + 1)",
             "bundle_id":"com.example.demo\(index + 1)","installed_version":"1.0.0","source":"sparkle",
             "state":"\(state)","latest_version":"1.1.0","cask":null,"store_url":null,"reason":null,
             "checked_at":\(now - 7200)}
            """
        }.joined(separator: ",")
        engine.respond(to: "apps_updates_cached", json: "{\"checked_at\":\(now - 7200),\"apps\":[\(updates)]}")
    }

    // MARK: Notch state pieces

    // The Overview and Monitor read three parts of the notch's state (`notch-state.json`). The
    // settings lane owns `notchState()`; these are the JSON fragments to put in it, so the Overview
    // shows its AI usage card and the Monitor its sensors in the rendered pictures:
    //   {"accounts": <notchAccountsJSON>, "permissions": <notchPermissionsJSON>, "system": <notchSystemJSON>, ...}

    /// Two connected accounts with their five-hour and weekly windows, and one that is not connected.
    static let notchAccountsJSON = """
        [
          {"id":"claude","name":"Claude","connected":true,"usesKeychain":true,"refusedAccess":false,
           "needsRenewal":false,"signInExplanation":"",
           "limits":[{"label":"5-hour","usedFraction":0.42,"seconds":18000},
                     {"label":"Weekly","usedFraction":0.71,"seconds":604800}]},
          {"id":"openai","name":"OpenAI","connected":true,"usesKeychain":true,"refusedAccess":false,
           "needsRenewal":false,"signInExplanation":"",
           "limits":[{"label":"5-hour","usedFraction":0.18,"seconds":18000},
                     {"label":"Weekly","usedFraction":0.93,"seconds":604800}]},
          {"id":"gemini","name":"Gemini","connected":false,"usesKeychain":false,"refusedAccess":false,
           "needsRenewal":false,"signInExplanation":"Sign in to see usage."}
        ]
        """

    /// Every permission granted, so the Overview shows no approval banner.
    static let notchPermissionsJSON = """
        [
          {"id":"accessibility","title":"Accessibility","why":"Window shortcuts","status":"granted","required":true},
          {"id":"notifications","title":"Notifications","why":"Alerts","status":"granted","required":false}
        ]
        """

    /// The System readings: network, battery, a fan and temperatures.
    static let notchSystemJSON = """
        {"network":{"interface":"en0","kind":"Wi-Fi","down":1250000,"up":84000},
         "battery":{"percent":78,"charging":false,"cycles":212,"health":0.93,"celsius":31.4,"watts":-9.6,
                    "minutes":312,"toFull":false},
         "fans":[{"name":"Left fan","rpm":1480},{"name":"Right fan","rpm":1512}],
         "temperatures":[{"name":"CPU die","celsius":47.5},{"name":"SSD","celsius":38.2},
                         {"name":"Battery","celsius":31.4}]}
        """
}
