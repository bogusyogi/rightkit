import Foundation
import RightKitAppleCredentials

// Dedicated synthetic namespace: never configure this host with an application's service.
let host = AppleCredentialHelperHost(
    service: "org.rightkit.qualification.credentials",
    accessGroup: "6KLGD3LLKF.org.rightkit.qualification.credentials",
    configuration: .init(accounts: ["qualification.synthetic"], defaultAccount: "qualification.synthetic")
)

// Qualification needs readiness only. Reject every secret-bearing command before dispatch.
let response: (line: String, exitCode: Int32)
if let line = readLine(),
   let request = try? JSONDecoder().decode(AppleCredentialHelperRequest.self, from: Data(line.utf8)),
   request.command == "status", request.scope == nil, request.account == nil, request.value == nil {
    response = host.respond(toLine: line)
} else {
    response = (#"{"ok":false,"error":"qualification helper accepts status only"}"#, 1)
}
print(response.line)
exit(response.exitCode)
