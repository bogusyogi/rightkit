// Phase-0 scaffold only: it is not packaged, signed, installed, or activated.
// AUTH_EXEC decisions must remain a bounded in-memory lookup; do not add I/O here.
import Foundation
import EndpointSecurity

final class RightKitEndpointExtension {
  private let grants = Set<String>() // immutable snapshot replaced out-of-band in Phase 1

  func authorizationDecision(executablePath: String) -> es_auth_result_t {
    // Default deny. No disk, IPC, network, logging, locks, or allocation-heavy work belongs here.
    return grants.contains(executablePath) ? ES_AUTH_RESULT_ALLOW : ES_AUTH_RESULT_DENY
  }
}
