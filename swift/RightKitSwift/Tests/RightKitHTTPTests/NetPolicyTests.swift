import Foundation
import Testing
@testable import RightKitHTTP

private struct StubResolver: HostResolving {
    var answers: [String: [String]]
    func resolve(host: String) async throws -> [String] {
        guard let found = answers[host] else { throw URLError(.cannotFindHost) }
        return found
    }
}

private func url(_ text: String) -> URL { URL(string: text)! }

@Test func classificationMatchesTheRustTable() {
    let expected: [(String, AddressClass)] = [
        ("127.0.0.1", .loopback), ("127.8.8.8", .loopback), ("::1", .loopback), ("localhost", .loopback), ("a.localhost", .loopback),
        ("10.1.2.3", .privateNetwork), ("172.16.0.1", .privateNetwork), ("172.32.0.1", .public), ("192.168.1.1", .privateNetwork),
        ("100.64.0.1", .privateNetwork), ("fd12::1", .privateNetwork), ("fec0::1", .privateNetwork), ("printer.local", .privateNetwork),
        ("169.254.169.254", .linkLocal), ("fe80::1", .linkLocal), ("fd00:ec2::254", .linkLocal), ("metadata.google.internal", .linkLocal),
        ("0.0.0.0", .unspecified), ("::", .unspecified), ("224.0.0.1", .multicast), ("ff02::1", .multicast),
        ("240.0.0.1", .reserved), ("192.0.2.1", .reserved), ("198.18.0.1", .reserved), ("2001:db8::1", .reserved),
        ("8.8.8.8", .public), ("2606:4700::1111", .public),
        // IPv6 forms that embed IPv4 take the embedded class.
        ("::ffff:127.0.0.1", .loopback), ("::ffff:10.0.0.1", .privateNetwork), ("64:ff9b::a9fe:a9fe", .linkLocal), ("2002:7f00:1::", .loopback),
        // Legacy IPv4 spellings are still IPv4.
        ("2130706433", .loopback), ("0x7f.1", .loopback), ("0177.0.0.1", .loopback),
    ]
    for (host, want) in expected { #expect(NetPolicy.classify(host: host) == want, "\(host)") }
    #expect(NetPolicy.classify(host: "example.com") == nil)
    #expect(NetPolicy.classify(host: "1password.com") == nil)
    #expect(NetPolicy.classify(host: "[::1]") == .loopback)
    #expect(NetPolicy.classify(host: "localhost.") == .loopback)
}

@Test func defaultPolicyDeniesEverythingButPublicHTTPS() {
    let policy = NetPolicy.publicOnly
    #expect(policy.isAllowed(url("https://example.com/x")))
    #expect(policy.isAllowed(url("https://8.8.8.8/x")))
    for denied in ["http://example.com", "https://localhost:11434", "http://127.0.0.1:1234", "https://192.168.1.5", "https://10.0.0.1",
                   "https://169.254.169.254/latest/meta-data", "https://[::1]/", "https://0.0.0.0", "ftp://example.com", "https://u:p@example.com"] {
        #expect(!policy.isAllowed(url(denied)), "\(denied)")
    }
}

@Test func optInOpensOnlyTheNamedClasses() {
    let loopback = NetPolicy.allowingLoopback
    #expect(loopback.isAllowed(url("http://localhost:11434/v1")))
    #expect(loopback.isAllowed(url("http://127.0.0.1:1234")))
    #expect(!loopback.isAllowed(url("http://192.168.1.5:8080")))

    let lan = NetPolicy.allowingLocalNetworks
    #expect(lan.isAllowed(url("http://192.168.1.5:8080")))
    #expect(lan.isAllowed(url("https://10.0.0.7")))
    #expect(lan.isAllowed(url("http://studio.local:1234")))
    #expect(!lan.isAllowed(url("http://example.com")))
}

@Test func metadataLinkLocalAndOtherClassesCannotBeAllowed() {
    let everything = NetPolicy(allowing: Set(AddressClass.allCases))
    #expect(everything.allowing == [.loopback, .privateNetwork])
    for denied in ["http://169.254.169.254/", "https://169.254.169.254/", "http://[fe80::1]/", "http://[fd00:ec2::254]/", "http://metadata.google.internal/",
                   "http://0.0.0.0/", "http://224.0.0.1/", "http://240.0.0.1/", "http://[::ffff:169.254.169.254]/"] {
        #expect(!everything.isAllowed(url(denied)), "\(denied)")
    }
}

@Test func vetResolvesNamesAndDeniesAnyBadAddress() async throws {
    let resolver = StubResolver(answers: [
        "good.example.com": ["93.184.216.34"],
        "rebind.example.com": ["93.184.216.34", "169.254.169.254"],
        "lan.example.com": ["192.168.1.9"],
        "empty.example.com": [],
        "meta.example.com": ["169.254.169.254"],
    ])
    try await NetPolicy.publicOnly.vet(url("https://good.example.com"), resolver: resolver)
    await #expect(throws: HTTPError.self) { try await NetPolicy.publicOnly.vet(url("https://rebind.example.com"), resolver: resolver) }
    await #expect(throws: HTTPError.self) { try await NetPolicy.publicOnly.vet(url("https://lan.example.com"), resolver: resolver) }
    await #expect(throws: HTTPError.self) { try await NetPolicy.publicOnly.vet(url("https://empty.example.com"), resolver: resolver) }
    await #expect(throws: HTTPError.self) { try await NetPolicy.allowingLocalNetworks.vet(url("https://meta.example.com"), resolver: resolver) }
    // Opted in: a name that points at the LAN is fine, over HTTP too; a public name over HTTP is not.
    try await NetPolicy.allowingLocalNetworks.vet(url("https://lan.example.com"), resolver: resolver)
    try await NetPolicy.allowingLocalNetworks.vet(url("http://lan.example.com"), resolver: resolver)
    await #expect(throws: HTTPError.insecureURL("good.example.com")) {
        try await NetPolicy.allowingLocalNetworks.vet(url("http://good.example.com"), resolver: resolver)
    }
    // A resolver failure is not a denial (the connection fails the same way).
    try await NetPolicy.publicOnly.vet(url("https://unknown.example.com"), resolver: resolver)
}

@Test func blockedDestinationIsATypedError() async {
    await #expect(throws: HTTPError.blockedDestination("loopback destinations are not allowed")) {
        try await NetPolicy.publicOnly.vet(url("https://127.0.0.1/"), resolver: StubResolver(answers: [:]))
    }
}

@Test func clientWithAPolicyEnforcesItBeforeSending() async throws {
    let fake = FakeTransport([FakeTransport.ok("{}")])
    let resolver = StubResolver(answers: ["api.example.com": ["93.184.216.34"]])
    let strict = HTTPClient(transport: fake, policy: .publicOnly, resolver: resolver)
    await #expect(throws: HTTPError.self) { _ = try await strict.send(HTTPRequest(url: url("http://127.0.0.1:11434/api/tags"))) }
    await #expect(throws: HTTPError.self) { _ = try await strict.send(HTTPRequest(url: url("https://169.254.169.254/latest"))) }
    #expect(fake.requests.isEmpty)
    _ = try await strict.send(HTTPRequest(url: url("https://api.example.com/x")))
    #expect(fake.requests.count == 1)

    let local = HTTPClient(transport: fake, policy: .allowingLoopback, resolver: resolver)
    _ = try await local.send(HTTPRequest(url: url("http://localhost:11434/api/tags")))
    #expect(fake.requests.count == 2)
    await #expect(throws: HTTPError.self) { _ = try await local.send(HTTPRequest(url: url("http://169.254.169.254/"))) }
}

@Test func redirectGuardAppliesThePolicyToEveryHop() async {
    let guardRail = PolicyRedirectGuard(policy: .publicOnly, resolver: StubResolver(answers: [:]))
    let session = URLSession(configuration: .ephemeral)
    let task = session.dataTask(with: URLRequest(url: url("https://api.example.com/a")))
    let response = HTTPURLResponse(url: url("https://api.example.com/a"), statusCode: 302, httpVersion: nil, headerFields: nil)!
    func hop(_ target: String, via guardRail: PolicyRedirectGuard) async -> URLRequest? {
        var request = URLRequest(url: url(target))
        request.setValue("Bearer t", forHTTPHeaderField: "Authorization")
        return await guardRail.urlSession(session, task: task, willPerformHTTPRedirection: response, newRequest: request)
    }
    #expect(await hop("https://169.254.169.254/", via: guardRail) == nil)
    #expect(await hop("https://127.0.0.1/", via: guardRail) == nil)
    #expect(await hop("http://api.example.com/b", via: guardRail) == nil)
    #expect(await hop("https://cdn.example.net/b", via: guardRail)?.value(forHTTPHeaderField: "Authorization") == nil)
    #expect(await hop("https://api.example.com/b", via: guardRail)?.value(forHTTPHeaderField: "Authorization") == "Bearer t")
    let local = PolicyRedirectGuard(allowing: [.loopback])
    #expect(await hop("http://127.0.0.1:1234/", via: local) != nil)
    #expect(await hop("http://169.254.169.254/", via: local) == nil)
}

@Test func makeSessionAttachesTheGuard() {
    let session = NetPolicy.allowingLoopback.makeSession()
    #expect(session.delegate is PolicyRedirectGuard)
    session.invalidateAndCancel()
}
