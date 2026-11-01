#if os(macOS)
import RightKitHub
import SwiftUI

/// Chat: port of `hub/src/views/Chat.tsx`. Messages between agent chats on this computer and
/// linked computers (the `pulse chat` engine).
///
/// The model lives in `ChatModelHolder`, so the draft, the search and the link form survive the
/// shell rebuilding the view and switching sections.
struct ChatSection: PulseSection {
    let id = "chat"
    let title = "Chat"
    let symbol = "message"
    /// After the five Settings pages (orders 70 to 110), as in the Tauri sidebar.
    let order = 120
    var searchEntries: [HubSearchEntry] {
        [HubSearchEntry(id: "chat", title: "Chat", keywords: ["message", "agent", "claude", "codex", "link", "send", "inbox"])]
    }

    private let holder = ChatModelHolder()

    init() {}

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(ChatView(model: holder.model(ctx), loadsOnAppear: true))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = holder.model(ctx)
        await model.load()
        return AnyView(ChatView(model: model, loadsOnAppear: false))
    }
}

/// Owns the section's one `ChatModel`, created on first use.
@MainActor
final class ChatModelHolder {
    private var created: ChatModel?

    nonisolated init() {}

    func model(_ ctx: HubContext) -> ChatModel {
        if let created { return created }
        let model = ChatModel(engine: ctx.engine)
        created = model
        return model
    }
}
#endif
