import Foundation
import RightKitAppleCredentials

// App bundles provide their signed Keychain access group through the production
// store. Tests opt into an isolated in-process backend so this helper never
// modifies a user's real Keychain.
let environment = ProcessInfo.processInfo.environment
let store: AppleCredentialStore
if environment["RIGHTKIT_HELPER_TEST_IN_MEMORY"] == "1" {
    store = AppleCredentialStore(
        service: environment["RIGHTKIT_HELPER_SERVICE"] ?? "rightkit.helper.test",
        backend: InMemoryAppleCredentialBackend()
    )
} else {
    store = AppleCredentialStore(
        service: environment["RIGHTKIT_HELPER_SERVICE"] ?? AppleCredentialStore.defaultService,
        accessGroup: environment["RIGHTKIT_HELPER_ACCESS_GROUP"]
    )
}
exit(AppleCredentialHelper.run(store: store))
