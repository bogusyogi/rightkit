import RightKitNotchCore
import RightKitNotch
import Foundation

/// Currency and crypto rates for the launcher's conversions. Fetched at most
/// once a day from open.er-api.com (currencies) and CoinGecko (coins), and
/// cached in Application Support/the app/launcher/rates.json. A failed or
/// offline fetch is ignored: conversions simply do not appear.
@MainActor
final class LauncherRates {
    struct Snapshot: Codable {
        let date: Date
        /// Units of each currency per one US dollar, keyed by currency code.
        let perUSD: [String: Double]
        /// US dollars per coin, keyed by upper-case symbol.
        let usdPerCoin: [String: Double]
    }

    /// Upper-case symbol to CoinGecko id.
    static let coinIDs: [String: String] = [
        "BTC": "bitcoin", "ETH": "ethereum", "SOL": "solana", "DOGE": "dogecoin",
        "LTC": "litecoin", "XRP": "ripple", "ADA": "cardano",
    ]

    private static let maxAge: TimeInterval = 86_400
    private static let retryAfter: TimeInterval = 3_600

    private(set) var snapshot: Snapshot?
    private var loaded = false
    private var fetching = false
    private var lastAttempt: Date?
    var onUpdate: (() -> Void)?

    private static var cacheURL: URL {
        LauncherStorage.launcherDirectory.appendingPathComponent("rates.json")
    }

    /// Reads the cache off the main thread at launch, so a conversion keystroke never waits on the disk.
    /// A read that comes first does it itself and this lands as a no-op.
    func preload() {
        guard !loaded else { return }
        let url = Self.cacheURL
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let saved = (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode(Snapshot.self, from: $0) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, !self.loaded else { return }
                    self.loaded = true
                    if let saved { self.snapshot = saved }
                }
            }
        }
    }

    var date: Date? {
        loadIfNeeded()
        return snapshot?.date
    }

    func perUSD(_ code: String) -> Double? {
        loadIfNeeded()
        return snapshot?.perUSD[code]
    }

    func usdPerCoin(_ symbol: String) -> Double? {
        loadIfNeeded()
        return snapshot?.usdPerCoin[symbol]
    }

    /// Starts a background fetch when the cache is over a day old and the last
    /// try was over an hour ago. Never blocks.
    func refreshIfStale() {
        loadIfNeeded()
        if fetching { return }
        if let date = snapshot?.date, Date().timeIntervalSince(date) < Self.maxAge { return }
        if let lastAttempt, Date().timeIntervalSince(lastAttempt) < Self.retryAfter { return }
        fetching = true
        lastAttempt = Date()
        Task {
            let fresh = await Self.fetch()
            self.fetching = false
            guard let fresh else { return }
            self.snapshot = fresh
            self.save(fresh)
            self.onUpdate?()
        }
    }

    private static func fetch() async -> Snapshot? {
        guard let fiatData = await download("https://open.er-api.com/v6/latest/USD"),
              let object = try? JSONSerialization.jsonObject(with: fiatData) as? [String: Any],
              let rates = object["rates"] as? [String: Double], rates["USD"] != nil
        else { return nil }
        var prices: [String: Double] = [:]
        let ids = coinIDs.values.sorted().joined(separator: ",")
        if let coinData = await download("https://api.coingecko.com/api/v3/simple/price?ids=\(ids)&vs_currencies=usd"),
           let coins = try? JSONSerialization.jsonObject(with: coinData) as? [String: [String: Double]] {
            for (symbol, id) in coinIDs {
                if let usd = coins[id]?["usd"] { prices[symbol] = usd }
            }
        }
        return Snapshot(date: Date(), perUSD: rates, usdPerCoin: prices)
    }

    private static func download(_ address: String) async -> Data? {
        // No network under test: the launcher simply has no rates.
        if Runtime.isUnderTest { return nil }
        guard let url = URL(string: address), url.scheme == "https" else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 10
        guard let result = try? await URLSession.shared.data(for: request),
              (result.1 as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return result.0
    }

    private func loadIfNeeded() {
        guard !loaded else { return }
        loaded = true
        guard let data = try? Data(contentsOf: Self.cacheURL) else { return }
        snapshot = try? JSONDecoder().decode(Snapshot.self, from: data)
    }

    private func save(_ snapshot: Snapshot) {
        LauncherStorage.ensure(LauncherStorage.launcherDirectory)
        guard let data = try? JSONEncoder().encode(snapshot) else { return }
        try? data.write(to: Self.cacheURL, options: .atomic)
    }
}
