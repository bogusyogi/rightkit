#if os(macOS)
import AppKit
import Foundation

/// Percent-encoding for a URL query value: only unreserved characters pass.
enum LauncherURLText {
    static func encode(_ text: String) -> String {
        text.addingPercentEncoding(withAllowedCharacters: unreserved) ?? text
    }

    private static let unreserved = CharacterSet(
        charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
}

/// The web search the launcher offers under every search (`launcherWebSearchEngine`).
enum LauncherWebEngine: String, CaseIterable {
    case google, bing, duckduckgo

    var title: String {
        switch self {
        case .google: return "Google"
        case .bing: return "Bing"
        case .duckduckgo: return "DuckDuckGo"
        }
    }

    var host: String {
        switch self {
        case .google: return "google.com"
        case .bing: return "bing.com"
        case .duckduckgo: return "duckduckgo.com"
        }
    }

    /// The address for a search; the text is percent-encoded so only unreserved characters pass.
    func url(for text: String) -> URL? {
        let query = LauncherURLText.encode(text)
        switch self {
        case .google: return URL(string: "https://www.google.com/search?q=" + query)
        case .bing: return URL(string: "https://www.bing.com/search?q=" + query)
        case .duckduckgo: return URL(string: "https://duckduckgo.com/?q=" + query)
        }
    }
}

@MainActor
enum LauncherWebSearch {
    /// The longest text sent to a search address.
    static let maxLength = 2_000

    static func open(_ text: String, engine: LauncherWebEngine) {
        guard let url = engine.url(for: String(text.prefix(maxLength))) else { return }
        NSWorkspace.shared.open(url)
    }
}

/// Hands a question to ChatGPT, with Pulse holding no key and making no network call.
///
/// ChatGPT: when the ChatGPT desktop app is installed, its documented `codex://` link opens a
/// new chat with the question typed into the composer
/// (https://learn.chatgpt.com/docs/app/commands: `codex://threads/new?prompt=<url-encoded text>`;
/// "keeps the codex:// URL scheme for compatibility"; the prompt is not sent automatically). The
/// app is found as the one macOS opens for that scheme, and only when it is named ChatGPT (the
/// scheme belongs to the Codex product too). Without the app, the question goes to
/// https://chatgpt.com/?q=<url-encoded text> in the default browser. No `chatgpt://` scheme is
/// documented, so none is used.
@MainActor
enum LauncherAssistant {
    /// The documented limit on the prompt text; longer text is cut.
    static let maxLength = 14_000
    /// The longest question put in a browser address.
    static let maxWebLength = 4_000

    /// The installed app, found off the main thread by `refreshInstalled` and read from memory
    /// by every keystroke.
    private(set) static var chatGPTApp: URL?
    private static var checked: Date?
    private static var checking = false

    /// Looks for the app (at most once a minute) and calls `changed` on the main thread when
    /// the answer is different.
    static func refreshInstalled(changed: @escaping () -> Void) {
        if checking { return }
        if let checked, Date().timeIntervalSince(checked) < 60 { return }
        checking = true
        DispatchQueue.global(qos: .utility).async {
            let chat = findChatGPT()
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    checking = false
                    checked = Date()
                    let different = chat != chatGPTApp
                    chatGPTApp = chat
                    if different { changed() }
                }
            }
        }
    }

    /// The app macOS opens for `codex://`, when it is the ChatGPT app.
    nonisolated private static func findChatGPT() -> URL? {
        guard let probe = URL(string: "codex://threads/new"),
              let app = NSWorkspace.shared.urlForApplication(toOpen: probe) else { return nil }
        let bundle = Bundle(url: app)
        let display = (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? app.deletingPathExtension().lastPathComponent
        let isChatGPT = bundle?.bundleIdentifier == "com.openai.chat" || display == "ChatGPT"
            || app.deletingPathExtension().lastPathComponent == "ChatGPT"
        return isChatGPT ? app : nil
    }

    /// The link that opens a new ChatGPT app chat with the question in the composer.
    static func chatGPTAppLink(for question: String) -> URL? {
        let text = String(question.prefix(maxLength))
        return URL(string: "codex://threads/new?prompt=" + LauncherURLText.encode(text))
    }

    /// The browser address that asks chatgpt.com the question.
    static func chatGPTWebLink(for question: String) -> URL? {
        let text = String(question.prefix(maxWebLength))
        return URL(string: "https://chatgpt.com/?q=" + LauncherURLText.encode(text))
    }

    /// The ChatGPT app with the question typed in, or chatgpt.com in the browser. True when
    /// something was opened.
    @discardableResult
    static func askChatGPT(_ question: String) -> Bool {
        if let app = chatGPTApp, let link = chatGPTAppLink(for: question) {
            NSWorkspace.shared.open([link], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
            return true
        }
        guard let link = chatGPTWebLink(for: question) else { return false }
        return NSWorkspace.shared.open(link)
    }
}
#endif
