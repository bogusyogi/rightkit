import RightKitNotchCore
import RightKitNotch
import Foundation
import RightKitWatch

/// the app fork: notices when Claude Desktop's signed-in account changes and has
/// the notch read the new account's usage at once.
///
/// Reads only the non-secret `lastKnownAccountUuid` in Claude's `config.json`,
/// and only when the file's modification date moves. Polled, not watched by
/// vnode: Claude rewrites the file by rename, which ends a file watch. The folder holding it is
/// watched instead (a rename lands as a folder write, which asks for one look at once), with a
/// slow 15 s poll behind it for a rewrite in place (#148: it was a 3 s poll on its own).
@MainActor
final class ClaudeAccountWatcher {
    private let configURL: URL
    private let onSwitch: () -> Void
    private var lastAccount: String?
    private var lastModified: Date?
    private var timer: Timer?
    private var polling = false
    private var folderWatcher: RightKitWatcher?
    private static let readQueue = DispatchQueue(label: "dev.orthic.pulse.claude-account-read", qos: .utility)

    /// Posted when the signed-in account changes.
    static let accountChanged = Notification.Name("dev.orthic.pulse.claudeAccountChanged")

    init(onSwitch: @escaping () -> Void) {
        self.configURL = Runtime.realHome("Claude Desktop's config")
            .appendingPathComponent("Library/Application Support/Claude/config.json")
        self.onSwitch = onSwitch
    }

    func start() {
        guard timer == nil else { return }
        lastAccount = Self.account(at: configURL)
        lastModified = Self.modified(configURL)
        schedule(every: 15)
        watchFolder()
    }

    private func watchFolder() {
        guard folderWatcher == nil else { return }
        let folder = configURL.deletingLastPathComponent().path
        // No Claude folder: the slow poll still covers it.
        guard FileManager.default.fileExists(atPath: folder) else { return }
        folderWatcher = try? RightKitWatcher(RightKitWatchConfiguration(paths: [folder]), queue: .main) {
            [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
    }

    private func schedule(every interval: TimeInterval) {
        timer?.invalidate()
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        timer.tolerance = interval / 4
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func poll() {
        // Threading: the `stat` and the config read run on a utility queue, never
        // on the main thread; only a date and a string come back. A tick is
        // skipped while one is in flight.
        guard !polling else { return }
        polling = true
        let url = configURL, known = lastModified
        Self.readQueue.async { [weak self] in
            let modified = Self.modified(url)
            let account = modified != known ? Self.account(at: url) : nil
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.apply(modified: modified, account: account) }
            }
        }
    }

    private func apply(modified: Date?, account: String?) {
        polling = false
        guard modified != lastModified else { return }
        lastModified = modified
        // A missing id (signed out mid-write) is not a switch.
        guard let account, account != lastAccount else { return }
        lastAccount = account
        NotificationCenter.default.post(name: Self.accountChanged, object: nil)
        onSwitch()
    }

    nonisolated private static func modified(_ url: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }

    /// Claude Desktop's signed-in account id (non-secret), or nil when Desktop
    /// is not installed or signed out.
    nonisolated static func desktopAccountUUID() -> String? {
        account(at: Runtime.realHome("Claude Desktop's config")
            .appendingPathComponent("Library/Application Support/Claude/config.json"))
    }

    nonisolated private static func account(at url: URL) -> String? {
        guard let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = object["lastKnownAccountUuid"] as? String, !id.isEmpty else { return nil }
        return id
    }
}
