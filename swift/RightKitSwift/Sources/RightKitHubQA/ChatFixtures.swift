import Foundation
import RightKitHubQA

extension HubFixtures {
    /// Canned `pulse-engine` replies for the Chat section, shaped like the real commands'
    /// results (`engine/src/chat.rs`). Times are relative to now so ages read naturally.
    static func chat(_ engine: QAEngineClient) {
        engine.respond(to: "chat_status") { _ in
            let now = Self.chatNowMs()
            return Data(
                """
                {"enabled":true,"hubRunning":true,"device":"Demo Mac","chatsHere":3,"links":[
                  {"device":"Studio Mac","ssh":"studio.local","online":true,"lastOkMs":\(now - 8_000),"lastErrorMs":null,"lastError":null},
                  {"device":"Dell","ssh":"dell.local","online":false,"lastOkMs":\(now - 5_400_000),"lastErrorMs":\(now - 30_000),"lastError":"ssh: connect to host dell.local port 22: Operation timed out"}
                ]}
                """.utf8)
        }
        engine.respond(to: "chat_peers") { _ in
            let now = Self.chatNowMs()
            return Data(
                """
                [
                  {"id":"7f3a9c10","name":"Pulse hub port","display":"Pulse hub port on Demo Mac","device":"Demo Mac","local":true,"kind":"claude","state":"busy","liveness":"live","updatedMs":\(now - 12_000),"folder":"pulse"},
                  {"id":"b41d2e77","name":"RightKit release notes","display":"RightKit release notes on Demo Mac","device":"Demo Mac","local":true,"kind":"claude","state":"idle","liveness":"live","updatedMs":\(now - 240_000),"folder":"rightkit"},
                  {"id":"c90e5a02","name":"Engine contract review","display":"Engine contract review on Demo Mac","device":"Demo Mac","local":true,"kind":"codex","state":"idle","liveness":"stale","updatedMs":\(now - 7_200_000),"folder":"engine"},
                  {"id":"Studio Mac:5d21f8aa","name":"Windows notch QA","display":"Windows notch QA on Studio Mac","device":"Studio Mac","local":false,"kind":"claude","state":"idle","liveness":"live","updatedMs":\(now - 90_000),"folder":"windows"},
                  {"id":"Studio Mac:e0b7134c","name":"Installer signing","display":"Installer signing on Studio Mac","device":"Studio Mac","local":false,"kind":"codex","state":"active","liveness":"unknown","updatedMs":null,"folder":"installer"}
                ]
                """.utf8)
        }
        engine.respond(to: "chat_inbox") { _ in
            let now = Self.chatNowMs()
            return Data(
                """
                {"messages":[
                  {"id":"m-1042","chat":"Pulse hub port","from":"Windows notch QA","body":"The Windows notch passes the hover journey on the Dell. One note: the hub did not reopen after the second quit. Details are in the QA receipt.","receivedMs":\(now - 180_000)},
                  {"id":"m-1041","chat":"RightKit release notes","from":"Installer signing","body":"Signing finished for build 412. Notarisation ticket attached to the release record.","receivedMs":\(now - 3_600_000)}
                ],"evicted":0}
                """.utf8)
        }
        engine.respond(to: "chat_send") { _ in
            Data(#"{"msgId":"m-1043","chat":"Pulse hub port on Demo Mac","state":"delivered","detail":"The chat read it at its next turn."}"#.utf8)
        }
        engine.respond(to: "chat_link") { _ in
            Data(#"{"device":"Mac mini","ssh":"mac-mini.local","chats":2,"theirName":"Mac mini"}"#.utf8)
        }
        engine.respond(to: "chat_unlink") { _ in Data("null".utf8) }
    }

    private static func chatNowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }
}
