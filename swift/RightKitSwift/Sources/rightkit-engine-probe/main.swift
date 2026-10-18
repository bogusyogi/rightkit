import Foundation
import RightKitEngine

// rightkit-engine-probe: starts a SupervisedEngine on the given engine executable, calls `ping`
// and `status` (or the one method named), prints each JSON result and exits 0. Any failure prints
// the error and exits 1. It is the end-to-end journey for the native transport layer; apps never
// link it.
//
//   rightkit-engine-probe <engine-path> [method] [--snapshot-method <name>] [--timeout <seconds>]
//
// Output, one line per step, on stdout:
//   started
//   <method> <json result>
//   shutdown <outcome> unreaped <count>
//   probe ok   (only after every call answered and the engine shut down cleanly)

func fail(_ message: String) -> Int32 {
    FileHandle.standardError.write(Data("rightkit-engine-probe: \(message)\n".utf8))
    return 1
}

func run() async -> Int32 {
    var positional: [String] = []
    var snapshotMethod = "engine.init"
    var timeoutSeconds = 60
    var arguments = Array(CommandLine.arguments.dropFirst())
    while !arguments.isEmpty {
        let argument = arguments.removeFirst()
        switch argument {
        case "--snapshot-method":
            guard !arguments.isEmpty else { return fail("--snapshot-method needs a value") }
            snapshotMethod = arguments.removeFirst()
        case "--timeout":
            guard !arguments.isEmpty, let seconds = Int(arguments.removeFirst()), seconds > 0 else {
                return fail("--timeout needs a positive number of seconds")
            }
            timeoutSeconds = seconds
        default:
            positional.append(argument)
        }
    }
    guard let enginePath = positional.first, positional.count <= 2 else {
        return fail("usage: rightkit-engine-probe <engine-path> [method] [--snapshot-method <name>] [--timeout <seconds>]")
    }
    let methods = positional.count == 2 ? [positional[1]] : ["ping", "status"]

    let engine = SupervisedEngine(
        enginePath: URL(fileURLWithPath: enginePath),
        snapshotMethod: snapshotMethod,
        startBudget: .seconds(timeoutSeconds),
        defaultCallTimeout: .seconds(timeoutSeconds)
    )
    var status = Int32(0)
    do {
        try await engine.start()
        print("started")
        for method in methods {
            let result = try await engine.call(method, params: Data("{}".utf8), timeout: nil)
            let text = String(data: result, encoding: .utf8) ?? "<\(result.count) bytes, not UTF-8>"
            print("\(method) \(text)")
        }
    } catch let error as EngineError {
        status = fail("\(error.code): \(error.message) (retryable: \(error.retryable))")
    } catch {
        status = fail("\(error)")
    }
    let report = await engine.shutdown(budget: .seconds(10))
    print("shutdown \(report.outcome.rawValue) unreaped \(report.unreaped)")
    if status == 0 && !report.isClean {
        status = fail("the engine did not shut down cleanly: \(report.outcome.rawValue), unreaped \(report.unreaped)")
    }
    if status == 0 { print("probe ok") }
    return status
}

exit(await run())
