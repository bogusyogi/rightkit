#if os(macOS)
import AppKit
import Foundation

/// A quick note, kept as one Markdown file. `id` is the file name (for example
/// "groceries-1a2b3c4d.md"), so it stays the same while the note is open. Deleting moves the
/// file into `.trash` and records `deletedAt` in the sidecar index; deleted notes stay 30 days
/// and can be restored ("notes trash").
struct LauncherNote: Equatable, Identifiable {
    var id: String
    /// The whole file text. Notes Pulse writes start with a "# Title" line.
    var text: String
    var createdAt: Date
    var updatedAt: Date
    var deletedAt: Date?
    /// From the sidecar index. Pinned notes list first; Command-P pins or unpins the selected note.
    var pinned: Bool = false

    var title: String { LauncherNoteFile.title(of: text) }
}

/// The text rules for note files: the heading line, the title, and the file name.
enum LauncherNoteFile {
    /// The text with a "# Title" first line. When the first non-empty line already starts with
    /// "#", the text is returned unchanged; empty text stays empty.
    static func headed(_ text: String) -> String {
        var lines = text.components(separatedBy: "\n")
        guard let index = lines.firstIndex(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty }) else {
            return text
        }
        let line = lines[index].trimmingCharacters(in: .whitespaces)
        if !line.hasPrefix("#") {
            lines[index] = "# " + line
        }
        return lines.joined(separator: "\n")
    }

    /// The first non-empty line without its heading marks, 90 characters at most.
    static func title(of text: String) -> String {
        let line = text.split(whereSeparator: { $0.isNewline })
            .map { stripHeading($0.trimmingCharacters(in: .whitespaces)) }
            .first(where: { !$0.isEmpty })
        return line.map { String($0.prefix(90)) } ?? "Empty note"
    }

    /// "# Title" gives "Title". A line whose "#" marks are not followed by a space is kept as it is.
    static func stripHeading(_ line: String) -> String {
        let marks = line.prefix(while: { $0 == "#" })
        guard !marks.isEmpty, marks.count <= 6 else { return line }
        let rest = line.dropFirst(marks.count)
        guard rest.isEmpty || rest.first == " " else { return line }
        return rest.trimmingCharacters(in: .whitespaces)
    }

    /// "<sanitised title>-<8 hex characters of the id>.md". Letters and digits only, so the name
    /// never holds a path separator.
    static func fileName(title: String, id: String) -> String {
        var slug = ""
        for character in title.lowercased() {
            if character.isLetter || character.isNumber {
                slug.append(character)
            } else if !slug.isEmpty, slug.last != "-" {
                slug.append("-")
            }
        }
        while slug.last == "-" { slug.removeLast() }
        if slug.isEmpty { slug = "note" }
        let suffix = id.replacingOccurrences(of: "-", with: "").prefix(8).lowercased()
        return String(slug.prefix(40)) + "-" + suffix + ".md"
    }

    /// A plain ".md" file name: no path separators, no colon, not hidden.
    static func isSafeName(_ name: String) -> Bool {
        name.hasSuffix(".md") && name.count > 3 && !name.hasPrefix(".")
            && !name.contains("/") && !name.contains("\\") && !name.contains(":") && !name.contains("\0")
    }
}

/// Light Markdown for the launcher: emphasis, code and links, with headings shown as plain text.
/// Parsing is inline-only, so block syntax stays as written. Any parse failure shows plain text.
enum LauncherMarkdown {
    static func inline(_ text: String) -> AttributedString {
        var options = AttributedString.MarkdownParsingOptions()
        options.interpretedSyntax = .inlineOnlyPreservingWhitespace
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }

    /// A whole note for the preview: heading lines shown as their text, the rest rendered inline.
    static func document(_ text: String) -> AttributedString {
        let lines = text.components(separatedBy: "\n")
            .map { LauncherNoteFile.stripHeading($0.trimmingCharacters(in: .whitespaces)) }
        return inline(lines.joined(separator: "\n"))
    }
}

/// The launcher's notes. Each note is a Markdown file in `Notes` under the launcher folder;
/// `Notes/notes-index.json` keeps the pinned and deleted state (files cannot carry it), and
/// deleted files wait in `Notes/.trash`. Local only; nothing leaves the Mac.
@MainActor
final class LauncherNotes {
    static let retention: TimeInterval = 30 * 86_400

    /// Live and deleted notes, as read by the last `reload()`.
    private(set) var notes: [LauncherNote] = []
    private var loaded = false
    /// Notes whose text changed in memory but whose file is not written yet.
    private var pendingWrites: Set<String> = []
    private var saveWork: [String: DispatchWorkItem] = [:]
    /// How long a note's text rests after the last keystroke before its file is written.
    static let saveDelay: TimeInterval = 0.8

    init() {
        // The app is quitting: any text still waiting for its file is written first.
        _ = NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification,
                                                   object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.flushPending() }
        }
    }

    static var folder: URL {
        LauncherStorage.launcherDirectory.appendingPathComponent("Notes", isDirectory: true)
    }

    static var trashFolder: URL {
        folder.appendingPathComponent(".trash", isDirectory: true)
    }

    private static var indexURL: URL { folder.appendingPathComponent("notes-index.json") }

    /// The sidecar index, keyed by file name.
    private struct NoteIndex: Codable {
        struct Entry: Codable {
            var pinned: Bool?
            var deletedAt: Date?
        }
        var version = 1
        var notes: [String: Entry] = [:]
    }

    var live: [LauncherNote] {
        loadIfNeeded()
        return notes.filter { $0.deletedAt == nil }
            .sorted { $0.pinned != $1.pinned ? $0.pinned : $0.updatedAt > $1.updatedAt }
    }

    var trashed: [LauncherNote] {
        loadIfNeeded()
        return notes.filter { $0.deletedAt != nil }.sorted { ($0.deletedAt ?? .distantPast) > ($1.deletedAt ?? .distantPast) }
    }

    func note(_ id: String) -> LauncherNote? {
        loadIfNeeded()
        return notes.first { $0.id == id }
    }

    /// Folder and index stamp of the last read, to tell whether anything changed since.
    private var lastStamp: Int?

    /// What `reloadIfChanged` compares: names and modification dates of the note files and the
    /// index. Read off the main thread, from a folder URL taken on it.
    nonisolated private static func stamp(of folder: URL, index: URL) -> Int {
        var hasher = Hasher()
        let urls = (try? FileManager.default.contentsOfDirectory(
            at: folder, includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles])) ?? []
        for url in urls.sorted(by: { $0.lastPathComponent < $1.lastPathComponent }) {
            hasher.combine(url.lastPathComponent)
            hasher.combine((try? url.resourceValues(forKeys: [.contentModificationDateKey]))?
                .contentModificationDate?.timeIntervalSince1970 ?? 0)
        }
        hasher.combine((try? index.resourceValues(forKeys: [.contentModificationDateKey]))?
            .contentModificationDate?.timeIntervalSince1970 ?? 0)
        return hasher.finalize()
    }

    /// Reads the notes again only when the files changed (checked off the main thread), so
    /// opening the launcher never waits on the notes folder. `changed` runs on the main thread
    /// after a reload. Replaces the reload on every show.
    func reloadIfChanged(then changed: @escaping () -> Void = {}) {
        let folder = Self.folder
        let index = Self.indexURL
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let stamp = Self.stamp(of: folder, index: index)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self else { return }
                    if self.loaded, self.lastStamp == stamp { return }
                    self.reload()
                    self.lastStamp = stamp
                    changed()
                }
            }
        }
    }

    /// Reads the notes from disk again. The launcher calls this when the files changed, so files
    /// edited outside Pulse appear (there is no watcher).
    func reload() {
        flushPending()
        loaded = true
        notes = Self.readFiles()
    }

    /// Starts a note with `text`; its first line becomes the "# Title" line. The note joins
    /// `notes` only when its file was written.
    @discardableResult
    func create(_ text: String) -> LauncherNote {
        loadIfNeeded()
        let now = Date()
        let uuid = UUID().uuidString
        let content = LauncherNoteFile.headed(text)
        let name = LauncherNoteFile.fileName(title: LauncherNoteFile.title(of: content), id: uuid)
        let note = LauncherNote(id: name, text: content,
                                createdAt: now, updatedAt: now, deletedAt: nil)
        if Self.write(content, to: Self.fileURL(name, trashed: false)) {
            notes.append(note)
        }
        return note
    }

    /// Keeps the text in memory at once; the file follows `saveDelay` after the last change.
    func update(_ id: String, text: String) {
        loadIfNeeded()
        guard let i = notes.firstIndex(where: { $0.id == id }), notes[i].text != text else { return }
        notes[i].text = text
        notes[i].updatedAt = Date()
        pendingWrites.insert(id)
        scheduleWrite(id)
    }

    func delete(_ id: String) {
        loadIfNeeded()
        guard let i = notes.firstIndex(where: { $0.id == id }), notes[i].deletedAt == nil else { return }
        // The file must hold the latest text before it moves; if that write fails, nothing moves.
        guard flush(id) else { return }
        let now = Date()
        guard Self.move(id, toTrash: true) else { return }
        notes[i].deletedAt = now
        Self.setEntry(id, pinned: notes[i].pinned, deletedAt: now)
    }

    func restore(_ id: String) {
        loadIfNeeded()
        guard let i = notes.firstIndex(where: { $0.id == id }) else { return }
        guard flush(id) else { return }
        let now = Date()
        guard Self.move(id, toTrash: false) else { return }
        notes[i].deletedAt = nil
        notes[i].updatedAt = now
        if let url = Self.fileURL(id, trashed: false) {
            try? FileManager.default.setAttributes([.modificationDate: now], ofItemAtPath: url.path)
        }
        Self.setEntry(id, pinned: notes[i].pinned, deletedAt: nil)
    }

    /// Pins or unpins a live note; pinned notes list first. The change is kept only once it is
    /// saved to the index.
    func setPinned(_ id: String, _ pinned: Bool) {
        loadIfNeeded()
        guard let i = notes.firstIndex(where: { $0.id == id }), notes[i].deletedAt == nil,
              notes[i].pinned != pinned else { return }
        if Self.setEntry(id, pinned: pinned, deletedAt: nil) {
            notes[i].pinned = pinned
        }
    }

    /// Live notes whose text matches `query` (all of them when it is empty).
    func matching(_ query: String, limit: Int) -> [LauncherNote] {
        let words = query.lowercased().split(separator: " ").map(String.init)
        let all = live
        guard !words.isEmpty else { return Array(all.prefix(limit)) }
        // A case-insensitive search of the text in place: no lowercased copy of every note per keystroke.
        return Array(all.filter { note in
            words.allSatisfy { note.text.range(of: $0, options: .caseInsensitive) != nil }
        }.prefix(limit))
    }

    /// Writes the note's file after `saveDelay`; a newer change cancels the earlier wait.
    private func scheduleWrite(_ id: String) {
        saveWork[id]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { _ = self?.flush(id) }
        }
        saveWork[id] = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.saveDelay, execute: work)
    }

    /// Writes one note's pending text now. True when nothing was pending or the write worked. A
    /// failed write keeps the text pending; the next change or flush tries again.
    @discardableResult
    func flush(_ id: String) -> Bool {
        guard pendingWrites.contains(id) else { return true }
        saveWork.removeValue(forKey: id)?.cancel()
        guard let note = notes.first(where: { $0.id == id }) else {
            pendingWrites.remove(id)
            return true
        }
        guard Self.write(note.text, to: Self.fileURL(id, trashed: note.deletedAt != nil)) else { return false }
        pendingWrites.remove(id)
        return true
    }

    /// Writes every pending note now: when the editor closes, the launcher hides, or the app quits.
    func flushPending() {
        for id in Array(pendingWrites) { flush(id) }
    }

    private func loadIfNeeded() {
        guard !loaded else { return }
        reload()
    }

    /// Every note file: live notes from `Notes`, deleted ones from `.trash`. A deleted note past
    /// 30 days is removed for good, as the trash always did. Unreadable files are skipped.
    private static func readFiles() -> [LauncherNote] {
        let index = readIndex()
        let cutoff = Date().addingTimeInterval(-retention)
        var out: [LauncherNote] = []
        for trashed in [false, true] {
            for url in markdownFiles(in: trashed ? trashFolder : folder) {
                let name = url.lastPathComponent
                let values = try? url.resourceValues(forKeys: [.creationDateKey, .contentModificationDateKey])
                let modified = values?.contentModificationDate ?? Date()
                let created = values?.creationDate ?? modified
                let entry = index.notes[name]
                let deletedAt: Date? = trashed ? (entry?.deletedAt ?? modified) : nil
                if let deletedAt, deletedAt <= cutoff {
                    try? FileManager.default.removeItem(at: url)
                    continue
                }
                guard let text = try? String(contentsOf: url, encoding: .utf8) else { continue }
                out.append(LauncherNote(id: name, text: text, createdAt: created, updatedAt: modified,
                                        deletedAt: deletedAt, pinned: !trashed && entry?.pinned == true))
            }
        }
        return out
    }

    /// The regular, non-symlink ".md" files directly inside `directory`.
    private static func markdownFiles(in directory: URL) -> [URL] {
        let keys: [URLResourceKey] = [.isRegularFileKey, .isSymbolicLinkKey]
        let urls = (try? FileManager.default.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles])) ?? []
        return urls.filter { url in
            guard LauncherNoteFile.isSafeName(url.lastPathComponent) else { return false }
            let values = try? url.resourceValues(forKeys: Set(keys))
            return values?.isRegularFile == true && values?.isSymbolicLink != true
        }
    }

    /// The file for a note name, in `Notes` or its trash. Nil when the name is unsafe, or when
    /// the file is a symlink or would resolve outside the folder.
    private static func fileURL(_ name: String, trashed: Bool) -> URL? {
        guard LauncherNoteFile.isSafeName(name) else { return nil }
        let directory = trashed ? trashFolder : folder
        let url = directory.appendingPathComponent(name)
        let values = try? url.resourceValues(forKeys: [.isSymbolicLinkKey])
        if values?.isSymbolicLink == true { return nil }
        let parent = url.resolvingSymlinksInPath().deletingLastPathComponent().path
        guard parent == directory.resolvingSymlinksInPath().path else { return nil }
        return url
    }

    /// Writes text atomically to a note file. False when there is no file or the write fails.
    private static func write(_ text: String, to url: URL?) -> Bool {
        guard let url else { return false }
        LauncherStorage.ensure(url.deletingLastPathComponent())
        do {
            try Data(text.utf8).write(to: url, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    /// Moves a note's file into the trash, or back out of it. Never overwrites a file.
    private static func move(_ name: String, toTrash: Bool) -> Bool {
        guard let source = fileURL(name, trashed: !toTrash),
              let target = fileURL(name, trashed: toTrash) else { return false }
        LauncherStorage.ensure(trashFolder)
        do {
            try FileManager.default.moveItem(at: source, to: target)
            return true
        } catch {
            return false
        }
    }

    private static func readIndex() -> NoteIndex {
        guard let data = try? Data(contentsOf: indexURL) else { return NoteIndex() }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return (try? decoder.decode(NoteIndex.self, from: data)) ?? NoteIndex()
    }

    private static func writeIndex(_ index: NoteIndex) -> Bool {
        LauncherStorage.ensure(folder)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(index) else { return false }
        do {
            try data.write(to: indexURL, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    /// Records pinned and deleted state for one note. An entry with neither is dropped.
    /// True when the index was written.
    @discardableResult
    private static func setEntry(_ name: String, pinned: Bool, deletedAt: Date?) -> Bool {
        var index = readIndex()
        index.notes[name] = (pinned || deletedAt != nil) ? NoteIndex.Entry(pinned: pinned, deletedAt: deletedAt) : nil
        return writeIndex(index)
    }
}

/// Nested snippet references: `{snippet:Name}` is replaced by the named
/// snippet's body (matched on name or keyword, ignoring case). A snippet
/// already being expanded is left as the literal reference, so cycles end;
/// nesting stops at 8 levels.
public enum LauncherSnippetNesting {
    public static func expand(_ body: String, snippets: [LauncherSnippet], visiting: [String] = []) -> String {
        guard visiting.count < 8 else { return body }
        var output = ""
        var rest = Substring(body)
        let marker = "{snippet:"
        while let start = rest.range(of: marker) {
            output += rest[..<start.lowerBound]
            let afterMarker = rest[start.upperBound...]
            guard let close = afterMarker.firstIndex(of: "}") else {
                output += rest[start.lowerBound...]
                rest = ""
                break
            }
            let name = afterMarker[..<close].trimmingCharacters(in: .whitespaces)
            let key = name.lowercased()
            let reference = String(rest[start.lowerBound...close])
            if !key.isEmpty, !visiting.contains(key),
               let target = snippets.first(where: {
                   $0.name.lowercased() == key || $0.keyword.lowercased() == key
               }) {
                output += expand(target.body, snippets: snippets, visiting: visiting + [key])
            } else {
                output += reference
            }
            rest = afterMarker[afterMarker.index(after: close)...]
        }
        output += rest
        return output
    }
}
#endif
