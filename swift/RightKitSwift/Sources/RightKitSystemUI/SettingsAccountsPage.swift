#if os(macOS)
import RightKitHub
import SwiftUI

/// Wording for the Accounts page: when a reading was taken and when a window resets. Ports of
/// `asOf`, `readingAge`, `accountFreshness`, `resetsIn` and `claudeColumns` in Settings.tsx.
enum SettingsAccountText {
    static func parseISO(_ text: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: text) { return date }
        return ISO8601DateFormatter().date(from: text)
    }

    /// "14:02" today, "Oct 14, 14:02" on another day.
    static func asOf(_ seconds: Double) -> String {
        let date = Date(timeIntervalSince1970: seconds)
        let time = DateFormatter()
        time.dateStyle = .none
        time.timeStyle = .short
        if Calendar.current.isDateInToday(date) { return time.string(from: date) }
        let day = DateFormatter()
        day.setLocalizedDateFormatFromTemplate("MMMd")
        return "\(day.string(from: date)), \(time.string(from: date))"
    }

    /// "Updated 12 min ago" once a reading is more than 10 minutes old; nil while it is fresh or unreadable.
    static func readingAge(_ lastReadAt: String, now: Date) -> String? {
        guard let at = parseISO(lastReadAt) else { return nil }
        let elapsed = now.timeIntervalSince(at)
        if elapsed <= 10 * 60 { return nil }
        let minutes = Int(elapsed / 60)
        if minutes < 60 { return "Updated \(minutes) min ago" }
        if minutes < 1440 { return "Updated \(minutes / 60) h ago" }
        return "Updated \(minutes / 1440) d ago"
    }

    /// The freshness line under an account, or nil. A sign-in renewal already says it.
    static func freshness(_ account: SettingsAccount, now: Date) -> String? {
        if account.needsRenewal == true { return nil }
        if let failure = account.lastError, !failure.isEmpty {
            if let stamp = account.lastReadAt, let at = parseISO(stamp) {
                return "Not updated since \(asOf(at.timeIntervalSince1970)): \(failure)"
            }
            return "Not updated: \(failure)"
        }
        if let stamp = account.lastReadAt { return readingAge(stamp, now: now) }
        return nil
    }

    /// "2 h 10 min" until `seconds` (epoch seconds), or nil once it has passed.
    static func resetsIn(_ seconds: Double, now: Date) -> String? {
        let left = seconds - now.timeIntervalSince1970
        if left <= 0 { return nil }
        let minutes = Swift.max(1, Int((left / 60).rounded()))
        let days = minutes / 1440
        let hours = (minutes % 1440) / 60
        if days > 0 { return "\(days) d \(hours) h" }
        if hours > 0 { return "\(hours) h \(minutes % 60) min" }
        return "\(minutes) min"
    }

    struct Column {
        var title: String
        var window: SettingsClaudeWindow?
    }

    /// Session and weekly (all models) windows; the first two the notch saved if neither is found.
    static func windows(_ windows: [SettingsClaudeWindow]) -> [Column] {
        let hour = 3600.0
        let session = windows.first { ($0.seconds).map { abs($0 - 5 * hour) < 60 } ?? false }
        let weeklies = windows.filter { ($0.seconds).map { $0 >= 6 * 24 * hour && $0 <= 8 * 24 * hour } ?? false }
        let weekly = weeklies.first { $0.label == "All models" } ?? weeklies.first
        var rows: [Column] = []
        if let session { rows.append(Column(title: "Session", window: session)) }
        if let weekly { rows.append(Column(title: "Weekly", window: weekly)) }
        if !rows.isEmpty { return rows }
        return windows.prefix(2).map { Column(title: $0.label, window: $0) }
    }

    /// The two columns every read account shows: Session and Weekly, a dash where a window is not known.
    static func columns(_ list: [SettingsClaudeWindow]) -> [Column] {
        let found = windows(list)
        if !found.isEmpty && !found.allSatisfy({ $0.title == "Session" || $0.title == "Weekly" }) { return found }
        return ["Session", "Weekly"].map { title in
            Column(title: title, window: found.first { $0.title == title }?.window)
        }
    }

    /// The colour of a used fraction (`tone` in api.ts).
    static func tone(_ fraction: Double) -> HubTone {
        if fraction >= 0.85 { return .bad }
        if fraction >= 0.6 { return .warn }
        return .ok
    }
}

/// Accounts: the logins the app reads, in the notch's order, with Claude's accounts under the Claude row.
struct SettingsAccountsPage: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme

    private var accounts: [SettingsAccount] {
        guard let state = model.state else { return [] }
        let order = state.providerOrder
        func rank(_ id: String) -> Int { order.firstIndex(of: id) ?? 999 }
        return state.accounts.enumerated()
            .sorted { (rank($0.element.id), $0.offset) < (rank($1.element.id), $1.offset) }
            .map { $0.element }
    }

    var body: some View {
        let list = accounts
        if list.isEmpty {
            HubEmptyState(symbol: "person.crop.circle", title: "No logins yet", message: "The notch has not listed any logins.")
        } else {
            SettingsCardStack(model: model, cards: cards(list))
                .task { await model.claudeMove.load() }
        }
    }

    private func cards(_ list: [SettingsAccount]) -> [SettingsCardSpec] {
        let now = Date()
        let hasGauges = model.state?.gauges != nil
        var cards = SettingsCardList()
        cards.add(
            "Logins the app reads (it never signs in itself)",
            list.enumerated().map { entry in
                SettingsRowSpec(
                    id: entry.element.id, text: searchText(entry.element, now: now),
                    view: SettingsAccountItem(
                        model: model, account: entry.element, index: entry.offset, ids: list.map { $0.id },
                        now: now, hasGauges: hasGauges))
            })
        if !model.claudeMove.offers.isEmpty || model.claudeMove.message != nil {
            cards.add(
                "Move Claude chats",
                [SettingsRowSpec(id: "claude-move", text: "Move chats between Claude accounts", view: SettingsClaudeMoveRow(model: model.claudeMove))])
        }
        return cards.cards
    }

    /// What a search matches for a login (the page's `Item text`): its name, summary, freshness line and its
    /// Claude accounts.
    private func searchText(_ account: SettingsAccount, now: Date) -> String {
        let summary = (account.summary ?? "").isEmpty ? (account.signInExplanation ?? "") : (account.summary ?? "")
        let fresh = SettingsAccountText.freshness(account, now: now) ?? ""
        let claude = (account.claudeAccounts ?? []).map { "\($0.name) \($0.email ?? "") \($0.plan ?? "")" }.joined(separator: " ")
        return "\(account.name) \(summary) \(fresh) \(claude)"
    }
}

/// One login: its name and state, its actions, its order buttons, and (for Claude) its accounts.
struct SettingsAccountItem: View {
    let model: SettingsPageModel
    let account: SettingsAccount
    let index: Int
    let ids: [String]
    let now: Date
    let hasGauges: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var summaryText: String {
        if let summary = account.summary, !summary.isEmpty { return summary }
        return account.signInExplanation ?? ""
    }

    private var hasSummary: Bool { !(account.summary ?? "").isEmpty }

    private func move(_ by: Int) {
        let target = index + by
        if target < 0 || target >= ids.count { return }
        var next = ids
        next.swapAt(index, target)
        let current = model
        Task { await current.setOrder(next) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: theme.spacing.lg) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: theme.spacing.sm) {
                        Text(verbatim: account.name)
                            .font(Font.system(size: 13, weight: .semibold))
                            .foregroundStyle(palette.ink.color)
                        if !hasGauges {
                            SettingsBadge(text: account.connected == true ? "Shown" : "Hidden", tone: account.connected == true ? .ok : .neutral)
                        }
                        if account.refusedAccess == true {
                            SettingsBadge(text: "Access refused", tone: .warn)
                        }
                        if account.needsRenewal == true {
                            SettingsBadge(text: "Sign-in needed", tone: .warn)
                        }
                    }
                    Text(verbatim: summaryText)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                    if let notice = account.notice, !notice.isEmpty {
                        Text(verbatim: notice)
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                    }
                    if let freshness = SettingsAccountText.freshness(account, now: now) {
                        Text(verbatim: freshness)
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                    }
                    actions
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                controls
            }
            .padding(.horizontal, theme.spacing.md + 2)
            .padding(.vertical, theme.spacing.sm + 2)
            if let claude = account.claudeAccounts, !claude.isEmpty {
                Divider()
                SettingsClaudeAccountList(model: model, accounts: claude)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var actions: some View {
        HStack(spacing: theme.spacing.sm) {
            if !hasSummary, let title = account.signInTitle {
                SettingsButton(title) {
                    let current = model
                    let id = account.id
                    Task { await current.send("signIn", ["provider": .string(id)]) }
                }
            }
            if account.usesKeychain == true && account.refusedAccess == true {
                SettingsButton("Allow access…") {
                    let current = model
                    let id = account.id
                    Task { await current.send("allowAccess", ["provider": .string(id)]) }
                }
            }
            if hasSummary {
                SettingsButton(
                    "Forget reading", kind: .ghost,
                    hint: "Clears what the app read. You stay signed in to \(account.name.split(separator: " ").first.map(String.init) ?? account.name) itself."
                ) {
                    let current = model
                    let id = account.id
                    Task { await current.send("signOut", ["provider": .string(id)]) }
                }
            }
        }
    }

    private var controls: some View {
        HStack(spacing: theme.spacing.md) {
            HStack(spacing: 4) {
                SettingsMiniButton(symbol: "chevron.up", label: "Move \(account.name) up", enabled: index != 0) {
                    move(-1)
                }
                SettingsMiniButton(symbol: "chevron.down", label: "Move \(account.name) down", enabled: index != ids.count - 1) {
                    move(1)
                }
            }
            if !hasGauges {
                Toggle("", isOn: connectedBinding)
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .accessibilityLabel("Show \(account.name)")
            }
        }
    }

    private var connectedBinding: Binding<Bool> {
        let current = model
        let id = account.id
        let on = account.connected == true
        return Binding(
            get: { on },
            set: { value in
                Task { await current.send("connect", ["provider": .string(id), "value": .bool(value)]) }
            }
        )
    }
}

/// Claude's accounts: those read at least once (and the signed-in one) as rows; the rest fold into
/// one disclosure line.
struct SettingsClaudeAccountList: View {
    let model: SettingsPageModel
    let accounts: [SettingsClaudeAccount]
    @State private var open = false
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let seen = accounts.filter { $0.active == true || $0.capturedAt != nil }
        let unseen = accounts.filter { account in !seen.contains { $0.id == account.id } }
        VStack(alignment: .leading, spacing: 0) {
            ForEach(seen) { account in
                if account.id != seen.first?.id { Divider() }
                SettingsClaudeAccountRow(model: model, account: account, unseen: false)
            }
            if !unseen.isEmpty {
                if !seen.isEmpty { Divider() }
                Button {
                    open.toggle()
                } label: {
                    HubLayered {
                        HubHitArea()
                        HStack(spacing: 6) {
                            Image(systemName: open ? "chevron.up" : "chevron.down")
                            Text(verbatim: "\(unseen.count) \(unseen.count == 1 ? "account" : "accounts") never seen signed in")
                                .font(theme.type.caption.font)
                            Spacer()
                        }
                        .foregroundStyle(palette.ink3.color)
                        .padding(.horizontal, theme.spacing.md + 2)
                        .padding(.vertical, theme.spacing.sm)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(open ? .isSelected : [])
                if open {
                    ForEach(unseen) { account in
                        Divider()
                        SettingsClaudeAccountRow(model: model, account: account, unseen: true)
                    }
                }
            }
            Divider()
            Text(verbatim: "A name set here is shared with your paired computer and phone; the newest rename wins.")
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
                .padding(.vertical, theme.spacing.sm)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Claude accounts")
    }
}

/// One Claude account: its name (renamed in place), plan, and Session and Weekly use.
struct SettingsClaudeAccountRow: View {
    let model: SettingsPageModel
    let account: SettingsClaudeAccount
    let unseen: Bool
    @State private var editing = false
    @State private var draft = ""
    @FocusState private var nameFocused: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func commit() {
        let next = draft.trimmingCharacters(in: .whitespaces)
        editing = false
        if next != account.name {
            let current = model
            let id = account.id
            Task { await current.send("renameClaudeAccount", ["id": .string(id), "name": .string(next)]) }
        }
    }

    var body: some View {
        let now = Date()
        let cached = account.active != true && account.capturedAt != nil
        HStack(alignment: .center, spacing: theme.spacing.md) {
            HStack(spacing: theme.spacing.sm) {
                if editing {
                    // Tauri keeps Enter (saves) and Escape (cancels); here the same two are icon buttons, so
                    // the field and its buttons fit the name column.
                    TextField("Name this account", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 150)
                        .accessibilityLabel("Name for \(account.name)")
                        .focused($nameFocused)
                        // The field takes focus as soon as it appears, so its accent ring and selection stay
                        // (Windows keeps them; comparison README.md:363).
                        .onAppear { nameFocused = true }
                        .onSubmit { commit() }
                    SettingsMiniButton(symbol: "checkmark", label: "Save") { commit() }
                    SettingsMiniButton(symbol: "xmark", label: "Cancel") { editing = false }
                } else {
                    Button {
                        draft = account.name
                        editing = true
                    } label: {
                        HStack(spacing: 6) {
                            Text(verbatim: account.name)
                                .font(Font.system(size: 13, weight: .semibold))
                                .foregroundStyle(palette.ink.color)
                                .lineLimit(1)
                                // Served before the pencil, or an equal share cuts the name at half (Fit.swift).
                                .layoutPriority(Fit.priority(windows: 1))
                            Image(systemName: "pencil")
                                .foregroundStyle(palette.ink3.color)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Rename \(account.name)")
                    // As in the Tauri row, a long name is cut short (with an ellipsis) before the plan and
                    // the "as of" time are. (Windows serves the name first, which is what makes it the one cut.)
                    .layoutPriority(Fit.priority(windows: 3, other: 1))
                }
                if account.active == true && !editing {
                    SettingsBadge(text: "Active", tone: .ok)
                }
                if let plan = account.plan, !plan.isEmpty, !unseen, !editing {
                    Text(verbatim: plan)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                        .layoutPriority(2)
                }
                if cached, !editing, let captured = account.capturedAt {
                    Text(verbatim: "as of \(SettingsAccountText.asOf(captured))")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                        .layoutPriority(2)
                }
                if account.active != true && account.onDisk == false {
                    Button {
                        let current = model
                        let id = account.id
                        Task { await current.send("forgetClaudeAccount", ["id": .string(id)]) }
                    } label: {
                        Image(systemName: "xmark")
                            .foregroundStyle(palette.ink3.color)
                            .frame(width: 22, height: 22)
                    }
                    .buttonStyle(.plain)
                    .layoutPriority(2)
                    .accessibilityLabel("Forget \(account.name)")
                    .accessibilityHint("Its folder is gone. Removes this account from the list; it returns if you sign in to it again.")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if unseen {
                Text(verbatim: "no reading")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            } else {
                HStack(spacing: 16) {
                    ForEach(Array(SettingsAccountText.columns(account.windows ?? []).enumerated()), id: \.offset) { entry in
                        meter(entry.element, now: now)
                    }
                }
            }
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, 4)
        .frame(minHeight: 44)
        .task(id: account.name) {
            if !editing { draft = account.name }
        }
    }

    private func meter(_ column: SettingsAccountText.Column, now: Date) -> some View {
        let left = column.window.flatMap { SettingsAccountText.resetsIn($0.resetsAt, now: now) }
        let fraction = Swift.min(Swift.max(column.window?.usedFraction ?? 0, 0), 1)
        return VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                // The title and the percentage keep their width (never "Sessio / n", "42 / %"); the
                // reset note beside them gives way first.
                Text(verbatim: column.title)
                    .font(Font.system(size: 11))
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                    .layoutPriority(2)
                Text(verbatim: left != nil ? SettingsFormat.percent(fraction) : "\u{2014}")
                    .font(Font.system(size: 12, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    // Below the title (the longer), above nothing: distinct priorities serve each what it
                    // needs. Windows serves the reset note first, so it is what is cut (Fit.swift).
                    .layoutPriority(Fit.priority(windows: 1, other: 2))
                Spacer(minLength: 0)
                Text(verbatim: left != nil ? "resets in \(left ?? "")" : (column.window != nil ? "reset" : ""))
                    .font(Font.system(size: 11))
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                    .layoutPriority(Fit.priority(windows: 3, other: 1))
            }
            if left != nil {
                HubProgressBar(value: fraction, tone: SettingsAccountText.tone(fraction), height: 4)
            } else {
                RoundedRectangle(cornerRadius: 2)
                    .fill(palette.fill2.color)
                    .frame(height: 4)
            }
        }
        // The page's 184 pt column (two of them are 60% of the row at most).
        .frame(width: 184)
    }
}
#endif
