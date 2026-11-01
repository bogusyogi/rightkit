import RightKitHubQA

extension HubFixtures {
    /// Canned `pulse-engine` replies for the Membrane section, shaped like `engine/src/membrane_cmds.rs`.
    /// The data is invented: a running resident with a model, one indexed project, two memory notes,
    /// a small context summary and a short log. Generic `/Users/demo` paths only.
    static func membrane(_ engine: QAEngineClient) {
        engine.respond(to: "membrane.status", json: membraneStatus)
        engine.respond(to: "membrane.freshness", json: membraneFreshness)
        engine.respond(to: "membrane.memory", json: membraneMemory)
        engine.respond(to: "membrane.context", json: membraneContext)
        engine.respond(to: "membrane.logs", json: membraneLogs)
        engine.respond(to: "membrane.restart", json: "{}")
    }

    private static let membraneStatus = """
        {"state":"running","running":true,"pid":4312,"uptimeSecs":8230,
         "model":{"state":"ready","reason":null,"health":"ok"},
         "availability":{"state":"ok","reason":"Ready for memory and context requests."},
         "lastError":null,"health":"ok","healthError":null}
        """

    private static let membraneFreshness = """
        {"projects":[{"root":"/Users/demo/Projects/site","generationId":42,"complete":true,
                      "eventSequence":1187,"lastEvent":"file.changed"}],
         "watcherState":"watching","coverage":"full","freshness":"fresh","reason":null}
        """

    private static let membraneMemory = """
        {"available":true,"reason":null,"count":2,"notes":[
           {"id":"n-102","ts":1791900000,"title":"Build uses the dev profile","repository":"/Users/demo/Projects/site"},
           {"id":"n-101","ts":1791810000,"title":null,"repository":"/Users/demo/Projects/notes"}]}
        """

    private static let membraneContext = """
        {"available":true,"reason":null,"receipts":14,"byState":{"delivered":12,"skipped":2},
         "byEvent":{"session-start":9,"prompt":5},"sessions":4,
         "last":{"event":"prompt","harness":"claude","state":"delivered","ts":1791935000}}
        """

    private static let membraneLogs = """
        {"lines":["2026-10-22T09:12:01 membrane started pid 4312","2026-10-22T09:12:04 model ready"],
         "count":2,"truncated":false,"available":true}
        """
}
