#if os(macOS)
import Foundation

// Pinboards (docs/clipboard-spec.md 6): create, rename, colour, order, membership. The index
// is the truth for membership (`ClipItem.boards`); `ClipBoard.itemOrder` is only the order and
// is kept in step here. Every change is written through `commit`.

/// Why a board name or a new board was refused (6: one line each).
enum ClipBoardError: Error, Equatable, Sendable {
    case emptyName
    case nameTooLong
    case duplicate
    case tooMany

    var message: String {
        switch self {
        case .emptyName: return L10n.t("Name the board")
        case .nameTooLong: return L10n.t("Board names are 40 characters at most")
        case .duplicate: return L10n.t("A board with that name already exists")
        case .tooMany: return L10n.t("You can have up to 20 boards")
        }
    }
}

extension LauncherClipboard {
    private static let maxBoardName = 40

    /// `#RRGGBB` upper-case, or nil.
    nonisolated static func normalisedColour(_ text: String?) -> String? {
        guard let text, text.utf8.count == 7, text.hasPrefix("#"),
              text.dropFirst().utf8.allSatisfy({ ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 70) || ($0 >= 97 && $0 <= 102) })
        else { return nil }
        return text.uppercased()
    }

    private func checkedName(_ name: String, excluding id: String?) -> Result<String, ClipBoardError> {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return .failure(.emptyName) }
        if trimmed.count > Self.maxBoardName { return .failure(.nameTooLong) }
        let lowered = trimmed.lowercased()
        if boards.contains(where: { $0.id != id && $0.name.lowercased() == lowered }) { return .failure(.duplicate) }
        return .success(trimmed)
    }

    private func newBoardID() -> String {
        let taken = Set(boards.map { $0.id })
        while true {
            let id = "b-" + String(format: "%04x", UInt32.random(in: 0...0xFFFF))
            if !taken.contains(id) { return id }
        }
    }

    private func renumbered(_ list: [ClipBoard]) -> [ClipBoard] {
        var out = list
        for index in out.indices { out[index].order = index }
        return out
    }

    // MARK: Boards

    /// 6: 1 to 40 characters, unique ignoring case, at most 20 boards. With no colour the board
    /// gets the first palette colour no board uses.
    @discardableResult
    func createBoard(name: String, colour: String?) -> Result<ClipBoard, ClipBoardError> {
        // A history that cannot be changed (another Pulse holds the folder) takes no new board.
        guard canMutate, boards.count < ClipBoard.maxBoards else { return .failure(.tooMany) }
        let clean: String
        switch checkedName(name, excluding: nil) {
        case .failure(let error): return .failure(error)
        case .success(let text): clean = text
        }
        let used = Set(boards.map { $0.colour.uppercased() })
        let fallback = ClipBoard.palette.first { !used.contains($0.hex) }?.hex
            ?? ClipBoard.palette[boards.count % ClipBoard.palette.count].hex
        let board = ClipBoard(
            id: newBoardID(), name: clean, colour: Self.normalisedColour(colour) ?? fallback,
            order: boards.count, itemOrder: [], createdAt: Date().timeIntervalSince1970)
        commit(boards: boards + [board])
        return .success(board)
    }

    @discardableResult
    func renameBoard(_ id: String, to name: String) -> Result<Void, ClipBoardError> {
        guard canMutate, let index = boards.firstIndex(where: { $0.id == id }) else { return .success(()) }
        switch checkedName(name, excluding: id) {
        case .failure(let error): return .failure(error)
        case .success(let clean):
            var next = boards
            next[index].name = clean
            commit(boards: next)
            return .success(())
        }
    }

    func recolourBoard(_ id: String, to colour: String) {
        guard let index = boards.firstIndex(where: { $0.id == id }),
              let hex = Self.normalisedColour(colour) else { return }
        var next = boards
        next[index].colour = hex
        commit(boards: next)
    }

    /// Cmd/Ctrl+Option+Left or Right with a board focused: one step each way.
    func moveBoard(_ id: String, by delta: Int) {
        guard let index = boards.firstIndex(where: { $0.id == id }) else { return }
        var next = boards
        let board = next.remove(at: index)
        next.insert(board, at: max(0, min(next.count, index + delta)))
        commit(boards: renumbered(next))
    }

    /// Drag in the tab row: `ids` first in that order, boards not named after them.
    func reorderBoards(_ ids: [String]) {
        var next: [ClipBoard] = []
        for id in ids where !next.contains(where: { $0.id == id }) {
            if let board = boards.first(where: { $0.id == id }) { next.append(board) }
        }
        let rest = boards.filter { board in !next.contains { $0.id == board.id } }
        next += rest
        commit(boards: renumbered(next))
    }

    /// Items stay in History. Those that were only on this board and not pinned become
    /// unprotected and follow retention.
    func deleteBoard(_ id: String) {
        guard canMutate, boards.contains(where: { $0.id == id }) else { return }
        var nextItems = items
        for index in nextItems.indices where nextItems[index].boards.contains(id) {
            nextItems[index].boards.removeAll { $0 == id }
        }
        commit(items: nextItems, boards: renumbered(boards.filter { $0.id != id }))
        retentionPass()
    }

    // MARK: Membership and order

    /// The board's name in a context menu: adds the item, or removes it when it is on the board.
    /// New items go first in the board's order. A full board (1000 items) takes no more.
    func toggleBoard(item id: String, board boardID: String) {
        guard let itemIndex = items.firstIndex(where: { $0.id == id }),
              let boardIndex = boards.firstIndex(where: { $0.id == boardID }) else { return }
        var nextItems = items
        var nextBoards = boards
        if nextItems[itemIndex].boards.contains(boardID) {
            nextItems[itemIndex].boards.removeAll { $0 == boardID }
            nextBoards[boardIndex].itemOrder.removeAll { $0 == id }
        } else {
            guard nextBoards[boardIndex].itemOrder.count < ClipBoard.maxItemsPerBoard else { return }
            nextItems[itemIndex].boards.append(boardID)
            nextBoards[boardIndex].itemOrder.removeAll { $0 == id }
            nextBoards[boardIndex].itemOrder.insert(id, at: 0)
        }
        commit(items: nextItems, boards: nextBoards)
        if !nextItems[itemIndex].isProtected { retentionPass() }
    }

    /// Inside a board: one step each way.
    func moveItem(_ item: String, inBoard board: String, by delta: Int) {
        guard let boardIndex = boards.firstIndex(where: { $0.id == board }),
              let place = boards[boardIndex].itemOrder.firstIndex(of: item) else { return }
        var next = boards
        var order = next[boardIndex].itemOrder
        order.remove(at: place)
        order.insert(item, at: max(0, min(order.count, place + delta)))
        next[boardIndex].itemOrder = order
        commit(boards: next)
    }

    /// Drag inside a board: `ids` first in that order, members not named after them.
    func reorderItems(inBoard board: String, _ ids: [String]) {
        guard let boardIndex = boards.firstIndex(where: { $0.id == board }) else { return }
        let current = boards[boardIndex].itemOrder
        let members = Set(current)
        var order: [String] = []
        var seen = Set<String>()
        for id in ids where members.contains(id) && seen.insert(id).inserted { order.append(id) }
        order += current.filter { !seen.contains($0) }
        var next = boards
        next[boardIndex].itemOrder = order
        commit(boards: next)
    }
}
#endif
