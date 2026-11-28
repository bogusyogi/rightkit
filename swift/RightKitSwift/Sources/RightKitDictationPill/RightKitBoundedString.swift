import Foundation

/// `s` cut to at most `maxBytes` UTF-8 bytes, never inside a character: a character that would cross
/// the bound is dropped whole, so the result is valid text. A string within the bound is returned
/// unchanged; a bound of zero or less gives "".
public func rightKitBoundedString(_ s: String, maxBytes: Int) -> String {
    guard maxBytes > 0 else { return "" }
    guard s.utf8.count > maxBytes else { return s }
    var used = 0
    var end = s.startIndex
    for index in s.indices {
        let next = s.index(after: index)
        let width = s[index..<next].utf8.count
        if used + width > maxBytes { break }
        used += width
        end = next
    }
    return String(s[s.startIndex..<end])
}
