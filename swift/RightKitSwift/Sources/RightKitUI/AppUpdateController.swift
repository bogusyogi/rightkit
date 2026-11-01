#if os(iOS)
import Foundation
import Combine

#if canImport(UIKit)
import UIKit
#endif

/// Lightweight App Store update check. It uses Apple's public lookup endpoint
/// and never sends transcript, audio, or account data.
@MainActor
final class AppUpdateController: ObservableObject {
    struct StoreListing: Decodable {
        let version: String
        let trackViewURL: URL?

        enum CodingKeys: String, CodingKey {
            case version
            case trackViewURL = "trackViewUrl"
        }
    }

    @Published private(set) var isChecking = false
    @Published private(set) var availableVersion: String?
    @Published private(set) var storeURL: URL?
    @Published var message = ""

    private let urlSession: URLSession
    private let bundle: Bundle

    init(urlSession: URLSession = .shared, bundle: Bundle = .main) {
        self.urlSession = urlSession
        self.bundle = bundle
    }

    var currentVersion: String {
        (bundle.infoDictionary?["CFBundleShortVersionString"] as? String)
            ?? (bundle.infoDictionary?["CFBundleVersion"] as? String)
            ?? "0.0.0"
    }

    var updateAvailable: Bool {
        guard let availableVersion else { return false }
        return Self.compare(availableVersion, currentVersion) == .orderedDescending
    }

    func check() async {
        guard !isChecking else { return }
        isChecking = true
        message = ""
        defer { isChecking = false }

        guard let bundleID = bundle.bundleIdentifier,
              var components = URLComponents(string: "https://itunes.apple.com/lookup") else {
            message = "This build has no App Store bundle identifier."
            return
        }
        components.queryItems = [URLQueryItem(name: "bundleId", value: bundleID)]
        guard let url = components.url else {
            message = "App Store update check is unavailable."
            return
        }

        do {
            var request = URLRequest(url: url)
            request.timeoutInterval = 10
            request.setValue("application/json", forHTTPHeaderField: "Accept")
            let (data, response) = try await urlSession.data(for: request)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                throw UpdateError.badResponse
            }
            let payload = try JSONDecoder().decode(LookupResponse.self, from: data)
            guard let listing = payload.results.first else {
                message = "This build is not listed on the App Store yet."
                availableVersion = nil
                storeURL = nil
                return
            }
            availableVersion = listing.version
            storeURL = listing.trackViewURL
            message = updateAvailable
                ? "Version \(listing.version) is available."
                : "You’re up to date."
        } catch is CancellationError {
            message = "Update check cancelled."
        } catch {
            message = "Couldn’t check for updates right now."
        }
    }

    func openStore() {
        #if canImport(UIKit)
        guard let storeURL else { return }
        UIApplication.shared.open(storeURL)
        #endif
    }

    private struct LookupResponse: Decodable {
        let results: [StoreListing]
    }

    private enum UpdateError: Error {
        case badResponse
    }

    static func compare(_ lhs: String, _ rhs: String) -> ComparisonResult {
        let left = lhs.split(separator: ".").map { Int($0) ?? 0 }
        let right = rhs.split(separator: ".").map { Int($0) ?? 0 }
        for index in 0..<max(left.count, right.count) {
            let l = index < left.count ? left[index] : 0
            let r = index < right.count ? right[index] : 0
            if l != r { return l < r ? .orderedAscending : .orderedDescending }
        }
        return .orderedSame
    }
}
#endif
