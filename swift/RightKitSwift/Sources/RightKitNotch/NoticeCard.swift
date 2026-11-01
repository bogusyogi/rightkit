import SwiftUI

// **The toasts** (liquid-motion pass §4): one anatomy for everything the notch
// says on its own. `NoticePrompt` is what a card says; `NoticeCard` draws it,
// contents only, in 196 x height points. The silhouette, the glass and the way
// out of the notch belong to `NoticeStage` (Notch/NoticeStage.swift), which
// the hover card shares.

/// **The one card** every toast is drawn with: contents only, 196pt wide.
struct NoticeCard: View {
    let prompt: NoticePrompt
    /// The pointer is on the card: the life ring stops.
    var held = false
    var onChoice: ((NoticeChoice) -> Void)?

    @State private var boxTicked = false

    /// The card's own size, the tail aside (`NoticePrompt.size(for:)`).
    static func size(for prompt: NoticePrompt) -> CGSize { NoticePrompt.size(for: prompt) }

    private var detailInk: Color { prompt.isProblem ? NoticeMetrics.warningInk : NoticeMetrics.detailInk }
    private var warningInk: Color { prompt.isAlert ? NoticeMetrics.alertInk : NoticeMetrics.warningInk }

    var body: some View {
        let layout = NoticeLayout(prompt)
        let pad = NoticeMetrics.padding
        VStack(alignment: .leading, spacing: 0) {
            titleRow(layout)
            if layout.detailLines > 0 {
                Text(verbatim: NoticeText.bound(prompt.detail, NoticeText.detailLimit))
                    .font(NoticeMetrics.detailFont)
                    .foregroundStyle(detailInk)
                    .lineLimit(layout.detailLines)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(width: NoticeMetrics.width - 2 * pad,
                           height: CGFloat(layout.detailLines) * NoticeLayout.detailLine,
                           alignment: .topLeading)
                    .padding(.top, NoticeLayout.titleToDetail)
            }
            if layout.warningLines > 0, let warning = prompt.warning {
                Text(verbatim: NoticeText.bound(warning, NoticeText.warningLimit))
                    .font(NoticeMetrics.detailFont)
                    .foregroundStyle(warningInk)
                    .lineLimit(layout.warningLines)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(width: NoticeMetrics.width - 2 * pad,
                           height: CGFloat(layout.warningLines) * NoticeLayout.detailLine,
                           alignment: .topLeading)
                    .padding(.top, layout.detailLines > 0 ? 0 : NoticeLayout.titleToDetail)
            }
            if let progress = prompt.progress {
                NoticeBar(fraction: progress)
                    .frame(height: NoticeLayout.barSlot, alignment: .top)
            }
            if let rows = prompt.rows {
                rowList(rows, height: layout.rowsHeight)
                    .padding(.top, NoticeLayout.rowsTop)
            }
            if let shelf = prompt.shelf, layout.shelfHeight > 0 {
                SendShelfList(items: Array(shelf.prefix(NoticeMetrics.maxVisibleRows)))
                    .frame(height: layout.shelfHeight, alignment: .top)
                    .padding(.top, NoticeLayout.rowsTop)
            }
            if let box = prompt.checkbox {
                checkbox(box)
                    .frame(height: NoticeLayout.checkboxSlot, alignment: .center)
            }
            if layout.hasActionRow {
                actionRow
                    .padding(.top, NoticeLayout.actionsTop)
            }
        }
        .padding(pad)
        .frame(width: NoticeMetrics.width, height: layout.height, alignment: .topLeading)
        .environment(\.colorScheme, .dark)
        .animation(.easeOut(duration: 0.2), value: prompt)
    }

    // MARK: - Pieces

    private func titleRow(_ layout: NoticeLayout) -> some View {
        HStack(alignment: .top, spacing: NoticeMetrics.glyphGap) {
            NoticeGlyphView(glyph: prompt.glyph, alert: prompt.isAlert)
                .frame(width: NoticeMetrics.glyph, height: NoticeMetrics.glyph)
                .padding(.top, (NoticeLayout.titleLine - NoticeMetrics.glyph) / 2)
            Text(verbatim: NoticeText.bound(prompt.title, NoticeText.titleLimit))
                .font(NoticeMetrics.titleFont)
                .foregroundStyle(NoticeMetrics.titleInk)
                .lineLimit(max(layout.titleLines, 1))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .topLeading)
            if layout.closeInTitleRow, let close = prompt.closeChoice {
                closeButton(close)
                    .padding(.leading, 6 - NoticeMetrics.glyphGap)
            }
        }
        .frame(height: layout.titleHeight, alignment: .top)
    }

    private var actionRow: some View {
        HStack(spacing: 5) {
            ForEach(Array(prompt.actions.enumerated()), id: \.offset) { _, action in
                NoticePill(action: action) {
                    if boxTicked, let alternate = action.choiceWhenChecked {
                        onChoice?(alternate)
                    } else {
                        onChoice?(action.choice)
                    }
                }
            }
            Spacer(minLength: 5)
            if let close = prompt.closeChoice {
                closeButton(close)
            }
        }
        .frame(height: NoticeMetrics.button)
    }

    private func closeButton(_ choice: NoticeChoice) -> some View {
        Button { onChoice?(choice) } label: {
            Image(systemName: "xmark")
                .font(.system(size: 7.5, weight: .bold))
                .foregroundStyle(NoticeMetrics.titleInk)
                .frame(width: NoticeMetrics.button, height: NoticeMetrics.button)
                .background(Circle().fill(Color.white.opacity(0.14)))
                .contentShape(Circle())
                .overlay {
                    // The life ring: what is left of an auto-dismissing toast's
                    // time, round the ×. Hovering stops it.
                    if let life = prompt.life {
                        NoticeLifeRing(life: life, held: held)
                            .frame(width: NoticeMetrics.button + 2.4, height: NoticeMetrics.button + 2.4)
                    }
                }
        }
        .buttonStyle(CardButtonStyle())
        .help(prompt.closeHelp)
    }

    private func checkbox(_ box: NoticePrompt.Checkbox) -> some View {
        Button { boxTicked.toggle() } label: {
            HStack(spacing: 4) {
                ZStack {
                    RoundedRectangle(cornerRadius: 2, style: .continuous)
                        .strokeBorder(NoticeMetrics.detailInk, lineWidth: 1)
                    if boxTicked {
                        Image(systemName: "checkmark")
                            .font(.system(size: 6, weight: .heavy))
                            .foregroundStyle(NoticeMetrics.titleInk)
                    }
                }
                .frame(width: 8, height: 8)
                Text(verbatim: box.label)
                    .font(.system(size: 8.5))
                    .foregroundStyle(NoticeMetrics.detailInk)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(box.label)
        .accessibilityValue(boxTicked ? "1" : "0")
        // Never carried over: each request starts unticked.
        .onAppear { boxTicked = box.startsTicked }
        .onChange(of: prompt) { old, new in
            if old.title != new.title || old.detail != new.detail { boxTicked = new.checkbox?.startsTicked ?? false }
        }
    }

    private func rowList(_ rows: NoticePrompt.Rows, height: CGFloat) -> some View {
        Group {
            if rows.items.isEmpty {
                HStack(spacing: 6) {
                    CardProgress(linear: false)
                        .scaleEffect(0.7)
                        .frame(width: 12, height: 12)
                    Text(verbatim: rows.emptyText)
                        .font(NoticeMetrics.detailFont)
                        .foregroundStyle(NoticeMetrics.detailInk)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .center)
            } else {
                CardScroll {
                    VStack(spacing: NoticeMetrics.rowGap) {
                        ForEach(rows.items) { row in
                            SendDeviceRow(row: row) { onChoice?(.disk(.sendTo(row.id))) }
                        }
                    }
                }
            }
        }
        .frame(width: NoticeMetrics.width - 2 * NoticeMetrics.padding, height: height, alignment: .top)
    }
}

// MARK: - The glyph, the pill, the bar and the life ring

/// The 13pt mark before a title.
struct NoticeGlyphView: View {
    let glyph: NoticePrompt.Glyph
    var alert = false

    var body: some View {
        switch glyph {
        case .app:
            CardIcon(path: Bundle.main.bundlePath)
        case .file(let path):
            CardIcon(path: path)
        case .provider(let mark):
            ProviderGlyphView(glyph: mark, size: NoticeMetrics.glyph)
                .foregroundStyle(alert ? NoticeMetrics.alertInk : NoticeMetrics.titleInk)
        case .symbol(let name):
            Image(systemName: name)
                .font(.system(size: 11, weight: .regular))
                .foregroundStyle(NoticeMetrics.titleInk)
        case .done:
            disc(NoticeMetrics.doneTint, symbol: "checkmark")
        case .problem:
            disc(NoticeMetrics.problemTint, symbol: "exclamationmark")
        case .working:
            CardProgress(linear: false)
                .scaleEffect(0.7)
        }
    }

    /// A 13pt disc at 22% of its tint, the mark inside in the tint.
    private func disc(_ tint: Color, symbol: String) -> some View {
        ZStack {
            Circle().fill(tint.opacity(0.22))
            Image(systemName: symbol)
                .font(.system(size: 7.5, weight: .heavy))
                .foregroundStyle(tint)
        }
    }
}

/// A 22pt action pill.
private struct NoticePill: View {
    let action: NoticePrompt.Action
    let run: () -> Void

    var body: some View {
        Button(action: run) {
            HStack(spacing: 3) {
                if action.busy {
                    CardProgress(linear: false)
                        .scaleEffect(0.55)
                        .frame(width: 9, height: 9)
                } else {
                    Image(systemName: action.symbol)
                        .font(.system(size: 8.5, weight: .semibold))
                }
                Text(verbatim: action.label)
                    .font(.system(size: 9, weight: .semibold))
                    .lineLimit(1)
            }
            .foregroundStyle(NoticeMetrics.titleInk)
            .padding(.horizontal, 9)
            .frame(height: NoticeMetrics.button)
            .background(Capsule().fill(Color.white.opacity(0.14)))
            .contentShape(Capsule())
        }
        .buttonStyle(CardButtonStyle())
        .disabled(action.busy)
    }
}

/// A 3.5pt bar: white 16% track, white 90% fill.
private struct NoticeBar: View {
    let fraction: Double

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.16))
                Capsule()
                    .fill(Color.white.opacity(0.9))
                    .frame(width: max(3.5, proxy.size.width * CGFloat(min(max(fraction, 0), 1))))
                    .animation(.easeOut(duration: 0.25), value: fraction)
            }
        }
        .frame(height: 3.5)
        .padding(.top, 3)
    }
}

/// **The life ring**: what is left of a toast's time, a thin ring round the ×.
///
/// One animation, started once: the ring runs from full to empty, linear, over
/// the time that is left, and nothing runs while it is on screen but that. A
/// hover freezes it where it is and lets it go again from there. The card
/// itself goes at the deadline its owner scheduled; this only shows it.
private struct NoticeLifeRing: View {
    let life: NoticePrompt.Life
    let held: Bool

    @Environment(\.viewShotStill) private var still
    @State private var fraction: CGFloat = 1
    /// Seconds left when the current run began.
    @State private var left: TimeInterval = 0
    @State private var runStart: Date?

    var body: some View {
        Circle()
            .trim(from: 0, to: fraction)
            .stroke(Color.white.opacity(0.45), style: StrokeStyle(lineWidth: 1.4, lineCap: .round))
            .rotationEffect(.degrees(-90))
            .onAppear { begin() }
            .onChange(of: life) { _, _ in begin() }
            .onChange(of: held) { _, nowHeld in
                if nowHeld { pause() } else { run() }
            }
    }

    private func begin() {
        guard !still else { return }
        left = max(0, life.expiresAt.timeIntervalSinceNow)
        set(CGFloat(left / max(life.seconds, 0.001)))
        if !held { run() }
    }

    private func run() {
        guard !still, runStart == nil, left > 0 else { return }
        runStart = Date()
        withAnimation(.linear(duration: left)) { fraction = 0 }
    }

    private func pause() {
        guard let started = runStart else { return }
        left = max(0, left - Date().timeIntervalSince(started))
        runStart = nil
        set(CGFloat(left / max(life.seconds, 0.001)))
    }

    /// Straight to `value`, replacing the run that was going.
    private func set(_ value: CGFloat) {
        runStart = nil
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) { fraction = min(max(value, 0), 1) }
    }
}
