import Foundation
import RightKitHubQA

extension HubFixtures {
    /// `status` as the engine answers it: CPU, memory, memory pressure and swap. Shared with the
    /// Overview.
    static let systemStatusJSON = """
        {"cpu_usage_percent":{"value":23.4,"capability":"Available","label":"CPU"},
         "memory_used_bytes":{"value":11200000000,"capability":"Available","label":"Memory used"},
         "memory_total_bytes":{"value":17179869184,"capability":"Available","label":"Memory total"},
         "memory_pressure":{"value":"Normal","capability":"Available","label":"macOS memory pressure"},
         "swap_used_bytes":{"value":1100000000,"capability":"Available","label":"Swap used"},
         "swap_total_bytes":{"value":2147483648,"capability":"Available","label":"Swap total"},
         "disks":[]}
        """

    /// Canned `pulse-engine` replies for the Monitor section, shaped like the real commands'
    /// results: `status`, `metrics_history` (30 minutes, one sample every 2 s), `process_rows`,
    /// `process_inspect`, `process_quit`, `process_force_quit` and `reveal`.
    static func monitor(_ engine: QAEngineClient) {
        let now = Date().timeIntervalSince1970
        engine.respond(to: "status", json: systemStatusJSON)
        engine.respond(to: "metrics_history", json: metricsHistoryJSON(now: now))

        let started = Int(now)
        engine.respond(
            to: "process_rows",
            json: "[\(processRows(now: started).joined(separator: ","))]")

        engine.respond(
            to: "process_inspect",
            json: """
                {"listening":[
                   {"protocol":"tcp","family":"IPv4","address":"127.0.0.1","port":3000},
                   {"protocol":"tcp","family":"IPv6","address":"::1","port":3000},
                   {"protocol":"udp","family":"IPv4","address":"0.0.0.0","port":5353}],
                 "connections":[
                   {"protocol":"tcp","family":"IPv4","remote_address":"93.184.216.34","remote_port":443}],
                 "open_files":[
                   "/Users/demo/Library/Application Support/Demo/cache/index.db",
                   "/Users/demo/Projects/site/node_modules/.cache/webpack/default-development/0.pack",
                   "/Applications/Demo.app/Contents/Frameworks/Core.framework/Versions/A/Core"],
                 "notes":["Open files are read with the process's own permissions."]}
                """)

        engine.respond(to: "process_quit", json: "\"quit\"")
        engine.respond(to: "process_force_quit", json: "\"quit\"")
        engine.respond(to: "reveal", json: "null")
    }

    /// 900 samples ending now, two seconds apart, with a daily-looking wobble in CPU and memory and
    /// bursts of network traffic. The first samples have no network reading, as after a start.
    private static func metricsHistoryJSON(now: Double) -> String {
        let count = 900
        let end = Int64(now * 1000)
        var samples: [String] = []
        samples.reserveCapacity(count)
        for index in 0..<count {
            let ts = end - Int64(count - 1 - index) * 2000
            let t = Double(index)
            var cpu = 22 + 12 * sin(t / 38) + 7 * sin(t / 6.5)
            if (index / 60) % 4 == 3 { cpu += 30 * sin(t / 9).magnitude }
            cpu = min(max(cpu, 2), 97)
            let mem = 10_900_000_000 + 420_000_000 * sin(t / 120) + t * 350_000
            let swap = 1_050_000_000 + t * 60_000
            let burst = max(0, sin(t / 23)) * max(0, sin(t / 5.1))
            let down = index < 4 ? "null" : String(Int(60_000 + burst * 2_400_000))
            let up = index < 4 ? "null" : String(Int(12_000 + burst * 260_000))
            samples.append(
                "{\"ts\":\(ts),\"cpu\":\(String(format: "%.1f", cpu)),\"memUsed\":\(Int64(mem)),"
                    + "\"memTotal\":17179869184,\"swapUsed\":\(Int64(swap)),\"swapTotal\":2147483648,"
                    + "\"pressure\":\"Normal\",\"netDown\":\(down),\"netUp\":\(up)}")
        }
        return "[\(samples.joined(separator: ","))]"
    }

    private static func processRows(now: Int) -> [String] {
        struct Member {
            let name: String
            let pid: Int
            let cpu: Double
            let memory: Int
            let threads: Int
            let age: Int
        }
        func row(
            _ key: String, _ name: String, bundle: String?, app: String?, pid: Int, cpu: Double, memory: Int,
            threads: Int?, age: Int?, executable: String?, members: [Member], canAct: Bool = true,
            refusal: String? = nil
        ) -> String {
            func quoted(_ value: String?) -> String { value.map { "\"\($0)\"" } ?? "null" }
            let memberJSON = members.map { member in
                """
                {"identity":{"pid":\(member.pid),"start_time":\(now - member.age)},"name":"\(member.name)",
                 "cpu_usage_percent":\(member.cpu),"memory_bytes":\(member.memory),
                 "threads":\(member.threads),"started_at":\(now - member.age)}
                """
            }.joined(separator: ",")
            return """
                {"key":"\(key)","name":"\(name)","bundle_id":\(quoted(bundle)),"app_path":\(quoted(app)),
                 "lead":{"pid":\(pid),"start_time":\(now - (age ?? 0))},"cpu_usage_percent":\(cpu),
                 "memory_bytes":\(memory),"threads":\(threads.map { String($0) } ?? "null"),
                 "started_at":\(age.map { String(now - $0) } ?? "null"),"executable":\(quoted(executable)),
                 "members":[\(memberJSON)],"can_act":\(canAct),"refusal":\(quoted(refusal))}
                """
        }
        let hour = 3600
        return [
            row(
                "app:com.example.browser", "Demo Browser", bundle: "com.example.browser",
                app: "/Applications/Demo Browser.app", pid: 612, cpu: 41.7, memory: 2_840_000_000, threads: 212,
                age: 5 * hour, executable: "/Applications/Demo Browser.app",
                members: [
                    Member(name: "Demo Browser", pid: 612, cpu: 6.2, memory: 540_000_000, threads: 48, age: 5 * hour),
                    Member(name: "Demo Browser Helper (Renderer)", pid: 640, cpu: 28.4, memory: 1_460_000_000, threads: 96, age: 5 * hour),
                    Member(name: "Demo Browser Helper (GPU)", pid: 633, cpu: 7.1, memory: 840_000_000, threads: 68, age: 5 * hour),
                ]),
            row(
                "app:com.example.editor", "Demo Editor", bundle: "com.example.editor",
                app: "/Applications/Demo Editor.app", pid: 1288, cpu: 12.9, memory: 1_620_000_000, threads: 74,
                age: 26 * hour, executable: "/Applications/Demo Editor.app",
                members: [
                    Member(name: "Demo Editor", pid: 1288, cpu: 9.9, memory: 980_000_000, threads: 44, age: 26 * hour),
                    Member(name: "Demo Editor Helper", pid: 1301, cpu: 3.0, memory: 640_000_000, threads: 30, age: 26 * hour),
                ]),
            row(
                "app:com.example.mail", "Demo Mail", bundle: "com.example.mail", app: "/Applications/Demo Mail.app",
                pid: 905, cpu: 1.3, memory: 920_000_000, threads: 38, age: 3 * 24 * hour,
                executable: "/Applications/Demo Mail.app",
                members: [Member(name: "Demo Mail", pid: 905, cpu: 1.3, memory: 920_000_000, threads: 38, age: 3 * 24 * hour)]),
            row(
                "proc:node:4410", "node", bundle: nil, app: nil, pid: 4410, cpu: 96.2, memory: 710_000_000, threads: 14,
                age: 40 * 60, executable: "/usr/local/bin/node",
                members: [Member(name: "node", pid: 4410, cpu: 96.2, memory: 710_000_000, threads: 14, age: 40 * 60)]),
            row(
                "app:com.example.chat", "Demo Chat", bundle: "com.example.chat", app: "/Applications/Demo Chat.app",
                pid: 1710, cpu: 2.4, memory: 640_000_000, threads: 52, age: 8 * hour,
                executable: "/Applications/Demo Chat.app",
                members: [Member(name: "Demo Chat", pid: 1710, cpu: 2.4, memory: 640_000_000, threads: 52, age: 8 * hour)]),
            row(
                "proc:WindowServer:155", "WindowServer", bundle: nil, app: nil, pid: 155, cpu: 7.8, memory: 590_000_000,
                threads: 21, age: 9 * 24 * hour, executable: "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer",
                members: [Member(name: "WindowServer", pid: 155, cpu: 7.8, memory: 590_000_000, threads: 21, age: 9 * 24 * hour)],
                canAct: false, refusal: "Quitting it would log you out."),
            row(
                "app:com.example.music", "Demo Music", bundle: "com.example.music", app: "/Applications/Demo Music.app",
                pid: 2231, cpu: 0.6, memory: 410_000_000, threads: 31, age: 2 * hour,
                executable: "/Applications/Demo Music.app",
                members: [Member(name: "Demo Music", pid: 2231, cpu: 0.6, memory: 410_000_000, threads: 31, age: 2 * hour)]),
            row(
                "proc:mds_stores:372", "mds_stores", bundle: nil, app: nil, pid: 372, cpu: 0.2, memory: 360_000_000,
                threads: 9, age: 9 * 24 * hour, executable: nil,
                members: [Member(name: "mds_stores", pid: 372, cpu: 0.2, memory: 360_000_000, threads: 9, age: 9 * 24 * hour)],
                canAct: false, refusal: "A system service."),
            row(
                "app:dev.orthic.pulse", "Pulse", bundle: "dev.orthic.pulse", app: "/Applications/Pulse.app",
                pid: 3120, cpu: 0.9, memory: 240_000_000, threads: 26, age: 6 * hour,
                executable: "/Applications/Pulse.app",
                members: [Member(name: "Pulse", pid: 3120, cpu: 0.9, memory: 240_000_000, threads: 26, age: 6 * hour)]),
            row(
                "proc:kernel_task:0", "kernel_task", bundle: nil, app: nil, pid: 0, cpu: 3.1, memory: 190_000_000,
                threads: 480, age: 9 * 24 * hour, executable: nil,
                members: [Member(name: "kernel_task", pid: 0, cpu: 3.1, memory: 190_000_000, threads: 480, age: 9 * 24 * hour)],
                canAct: false, refusal: "The kernel cannot be quit."),
        ]
    }
}
