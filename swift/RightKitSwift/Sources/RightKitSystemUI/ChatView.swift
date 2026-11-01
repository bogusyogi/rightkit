import RightKitHub
import SwiftUI

/// Chat's page: status, the chat list beside the message box, the inbox and the link form.
/// Port of `hub/src/views/Chat.tsx`. Message text is plain `Text`: Markdown rendering is a
/// RightKit question (MarkdownUI does not compile for Windows).
struct ChatView: View {
    let model: ChatModel
    /// False when `preparedView` already loaded the data.
    let loadsOnAppear: Bool
    /// The search box shows the accent ring while it has the keyboard (#336).
    @FocusState private var searchFocused: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(verbatim: "Chat sends a message from one agent chat to another, on this computer or a linked one. The receiver is another agent, not a person. A reply arrives in the chat that sent the message, not in this window.")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .frame(maxWidth: 680, alignment: .leading)
                    // The page sets this paragraph at a 1.5 line height; the spare space stands in for it.
                    .padding(.bottom, 7)

                statusPanel

                HStack(alignment: .top, spacing: 12) {
                    chatsPanel
                    composePanel
                }

                inboxPanel
                linkPanel
            }
            .padding(.horizontal, 18)
            .padding(.top, 14)
            .padding(.bottom, 18)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task {
            await model.liveLoop(loadFirst: loadsOnAppear)
        }
        .task {
            await model.peersLoop(loadFirst: loadsOnAppear)
        }
        .hubConfirm(
            "Unlink \(model.unlinkTarget ?? "")?",
            message: "Chats there stop being reachable from this computer until you link it again.",
            confirmLabel: "Unlink",
            isPresented: model.confirmingUnlinkBinding
        ) {
            model.confirmUnlink()
        }
    }

    // MARK: Status

    private var statusPanel: some View {
        ChatPanel {
            Text(verbatim: ChatFormat.statusLine(model.status))
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
            if let error = model.liveError {
                HubBanner(error, tone: .bad)
            }
            if let status = model.status, status.enabled {
                ForEach(status.links, id: \.device) { link in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(verbatim: ChatFormat.linkLine(link))
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                        if !link.online, let error = link.lastError {
                            Text(verbatim: error)
                                .font(theme.type.caption.font)
                                .foregroundStyle(palette.bad.color)
                        }
                    }
                }
            }
        }
    }

    // MARK: Chats

    private var chatsPanel: some View {
        ChatPanel {
            Text(verbatim: "Chats")
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
            searchField
            if let error = model.peersError {
                HubBanner(error, tone: .bad)
            }
            if model.peers == nil, model.peersError == nil {
                ChatHint("Reading chats…")
            }
            if let peers = model.peers, peers.isEmpty {
                ChatHint("No chats found. Open a Claude or Codex chat on this computer, or link another computer.")
            }
            if let peers = model.peers, !peers.isEmpty, model.visiblePeers.isEmpty {
                ChatHint("No chat matches the search.")
            }
            ForEach(model.groups, id: \.device) { group in
                VStack(alignment: .leading, spacing: 4) {
                    Text(verbatim: group.device)
                        .font(Font.system(size: 12, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(group.chats, id: \.id) { chat in
                            chatRow(chat)
                        }
                    }
                }
            }
        }
    }

    private var searchField: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.field)
                .stroke(searchFocused ? palette.accent.color : palette.hairline.color, lineWidth: searchFocused ? 2 : 1)
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(palette.ink3.color)
                TextField("Search by name, computer or folder", text: model.queryBinding)
                    .textFieldStyle(.plain)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    .accessibilityLabel("Search chats")
                    .focused($searchFocused)
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
        }
    }

    private func chatRow(_ chat: ChatPeerDTO) -> some View {
        let selected = chat.id == model.selectedId
        return Button {
            model.selectedId = chat.id
        } label: {
            HubLayered {
                HubHitArea()
                if selected {
                    RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.accent.color.opacity(0.14))
                    Rectangle().fill(palette.accent.color).frame(width: 2)
                }
                HStack(spacing: 8) {
                    kindPill(chat.kind)
                        .frame(width: 58)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(verbatim: chat.name)
                            .font(theme.type.label.font)
                            .foregroundStyle(palette.ink.color)
                            .lineLimit(1)
                        Text(verbatim: ChatFormat.meta(chat))
                            .font(theme.type.small.font)
                            .foregroundStyle(palette.ink3.color)
                            .lineLimit(1)
                    }
                    Spacer()
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .frame(minHeight: 40)
            }
        }
        .buttonStyle(.plain)
        // One name in the Tauri row's own reading order (kind, name, state), without the commas VoiceOver
        // would put between the three texts, so the parity tour pairs the row with the page's.
        .accessibilityLabel("\(chat.kind == "claude" ? "CLAUDE" : "CODEX") \(chat.name) \(ChatFormat.meta(chat))")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func kindPill(_ kind: String) -> some View {
        let claude = kind == "claude"
        return HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.pill)
                .fill(claude ? palette.accent.color.opacity(0.18) : palette.fill2.color)
            Text(verbatim: claude ? "CLAUDE" : "CODEX")
                .font(theme.type.micro.font)
                .foregroundStyle(claude ? palette.ink.color : palette.ink2.color)
                .padding(.horizontal, 6)
                .padding(.vertical, 1)
                .frame(maxWidth: .infinity)
        }
    }

    // MARK: Compose

    private var composePanel: some View {
        ChatPanel {
            Text(verbatim: model.chosen.map { "Message to \($0.display)" } ?? "Message")
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
            if model.chosen == nil {
                ChatHint(model.selectedId == nil ? "Choose a chat to message." : "That chat is no longer listed.")
            }
            ChatField(
                label: "Message text",
                placeholder: "Written for another agent, not a person",
                text: model.draftBinding,
                multiline: true
            )
            .disabled(model.chosen == nil)
            .opacity(model.chosen == nil ? 0.55 : 1)
            HStack(spacing: 10) {
                ChatHint("Cmd+Enter or Ctrl+Enter sends. Sent from here, the message has no sending chat, so a reply has nowhere to return.")
                ChatButton(title: model.sending ? "Sending…" : "Send", primary: true, enabled: model.canSend) {
                    model.send()
                }
                .keyboardShortcut(.return, modifiers: .command)
            }
            if let error = model.sendError {
                HubBanner(error, tone: .bad)
            }
            if let receipt = model.receipt {
                receiptView(receipt)
            }
        }
    }

    private func receiptView(_ receipt: ChatReceiptDTO) -> some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: "\(receipt.chat):")
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                Text(verbatim: "\(ChatFormat.receiptWords(receipt.state)) \(receipt.detail)")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink.color)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: Inbox

    private var inboxPanel: some View {
        ChatPanel {
            Button {
                model.inboxOpen.toggle()
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: model.inboxOpen ? "chevron.down" : "chevron.right")
                        .foregroundStyle(palette.ink2.color)
                        .frame(width: 14)
                    Text(verbatim: "Inbox" + (model.inbox.map { " (\($0.messages.count))" } ?? ""))
                        .font(Font.system(size: 13, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                }
                .padding(.vertical, 4)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Inbox" + (model.inbox.map { " (\($0.messages.count))" } ?? ""))
            .accessibilityValue(model.inboxOpen ? "expanded" : "collapsed")
            if model.inboxOpen {
                ChatHint("Messages for chats on this computer that they have not read yet. Opening this list does not mark them read.")
                if (model.inbox?.messages.count ?? 0) == 0 {
                    ChatHint("No unread messages.")
                }
                ForEach(model.inbox?.messages ?? [], id: \.id) { message in
                    messageView(message)
                }
                if let inbox = model.inbox, inbox.evicted > 0 {
                    ChatHint("\(ChatFormat.plural(inbox.evicted, "older message")) dropped to keep the inbox small.")
                }
            }
        }
    }

    private func messageView(_ message: ChatMessageDTO) -> some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
            VStack(alignment: .leading, spacing: 3) {
                Text(verbatim: "\(message.from) to \(message.chat) · \(ChatFormat.lastSeen(message.receivedMs))")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                // Plain text: the body is another agent's text, never rendered as Markdown here.
                Text(verbatim: message.body)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink.color)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: Link

    private var linkPanel: some View {
        ChatPanel {
            Text(verbatim: "Link a computer")
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
            ChatHint("Link each computer to the other, so replies can come back. The computer must be reachable over ssh with keys that work. Checking the link can take a little while.")
            ForEach(model.status?.links ?? [], id: \.device) { link in
                HStack(spacing: 10) {
                    HStack(spacing: 4) {
                        Text(verbatim: link.device)
                            .font(theme.type.body.font)
                            .foregroundStyle(palette.ink.color)
                        Text(verbatim: link.ssh)
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                    }
                    Spacer()
                    ChatButton(title: "Unlink", primary: false, enabled: true) {
                        model.askUnlink(link.device)
                    }
                }
                .frame(minHeight: 32)
            }
            if let status = model.status, status.links.isEmpty {
                ChatHint("No linked computers yet.")
            }
            HStack(alignment: .bottom, spacing: 10) {
                ChatField(label: "Computer name", placeholder: "mac-mini", text: model.linkDeviceBinding, multiline: false)
                ChatField(label: "ssh host", placeholder: "mac-mini.local", text: model.linkSshBinding, multiline: false)
                ChatField(label: "Path of pulse there (optional)", placeholder: "pulse", text: model.linkPulseBinding, multiline: false)
                ChatButton(title: model.linking ? "Linking…" : "Link", primary: true, enabled: model.canLink) {
                    model.link()
                }
            }
            if let note = model.linkNote {
                ChatHint(note)
            }
            if let error = model.linkError {
                HubBanner(error, tone: .bad)
            }
        }
    }
}

// MARK: Small parts

/// A panel: a white card with a hairline border, 14 points in at the sides and 12 at the top and
/// bottom, holding a column of content with 8 points between items (the page's `.ck-chat-panel`).
private struct ChatPanel<Content: View>: View {
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.panel.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 8) {
                content
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A muted explanatory line.
private struct ChatHint: View {
    private let text: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(verbatim: text)
            .font(theme.type.caption.font)
            .foregroundStyle(theme.palette(scheme).ink2.color)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A labelled text box: one line, or several for the message text.
private struct ChatField: View {
    let label: String
    let placeholder: String
    let text: Binding<String>
    let multiline: Bool
    /// The box shows the accent ring while the field has the keyboard (#336).
    @FocusState private var focused: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: label)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
                RoundedRectangle(cornerRadius: theme.radii.field)
                    .stroke(focused ? palette.accent.color : palette.hairline.color, lineWidth: focused ? 2 : 1)
                input
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
            }
        }
    }

    /// The page names a field by its placeholder, and so does this one: the placeholder is drawn under
    /// the text, and the field itself has an empty prompt and the placeholder as its accessibility
    /// label (a label equal to the prompt reports no name on the Mac).
    @ViewBuilder
    private var input: some View {
        if multiline {
            HubLayered {
                placeholderText
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                TextField("", text: text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    .accessibilityLabel(placeholder)
                    .focused($focused)
                    .frame(minHeight: 96, alignment: .topLeading)
            }
        } else {
            HubLayered {
                placeholderText
                    .frame(maxWidth: .infinity, alignment: .leading)
                TextField("", text: text)
                    .textFieldStyle(.plain)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    .accessibilityLabel(placeholder)
                    .focused($focused)
            }
        }
    }

    private var placeholderText: some View {
        Text(verbatim: placeholder)
            .font(theme.type.body.font)
            .foregroundStyle(palette.ink3.color)
            .lineLimit(multiline ? nil : 1)
            .opacity(text.wrappedValue.isEmpty ? 1 : 0)
    }
}

/// A small button: filled with the accent when primary, an outline otherwise.
struct ChatButton: View {
    let title: String
    let primary: Bool
    let enabled: Bool
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.control)
                    .fill(primary ? palette.accent.color : palette.fill2.color)
                RoundedRectangle(cornerRadius: theme.radii.control)
                    .stroke(primary ? palette.accent.color : palette.line.color, lineWidth: 1)
                Text(verbatim: title)
                    .font(theme.type.caption.font)
                    .foregroundStyle(primary ? palette.accentInk.color : palette.ink.color)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
            }
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }
}
