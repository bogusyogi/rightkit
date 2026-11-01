import Foundation

enum ProviderKind: Equatable {
    case usage
    /// the app fork: CPU, memory and disk readings sampled on this Mac.
    case system
}

/// How recent a reading has to be to answer a fetch.
///
/// Several providers hold a cheaper source that is older than a live one:
/// Claude Desktop's cache is a file somebody else writes, and reusing it for
/// half an hour is right while nothing is being spent. It is wrong while a
/// session is running — that is precisely when the number moves, eleven points
/// in fifteen minutes on the day `desktopFreshness` was measured — and wrong
/// the moment somebody opens the menu to look. Only the caller knows which of
/// those is happening, so the caller says, and each provider decides what it
/// can do about it.
enum UsageFreshness: Equatable {
    /// Whatever the provider's ordinary sources allow, cache included. The
    /// schedule's default: a number nobody is watching and nothing is moving.
    case standard
    /// Old enough to be interesting is not good enough. A provider honouring
    /// this drops a cached reading it would otherwise have served and asks a
    /// source that can answer for right now.
    case live
    /// Nothing held, at any age. The source itself, and a request spent on it.
    ///
    /// What **Refresh now** and a click on a ring ask for, and what a look asks
    /// for when "Ask the provider every time you look" is on. Strictly more
    /// expensive than `.live` and not strictly better: a provider that rate
    /// limits answers a request too many with a 429, and its back-off then holds
    /// an *older* number than the cache would have. So it is what somebody asks
    /// for, never what the schedule decides on its own.
    ///
    /// It may not leave a ring emptier than `.standard` would have. Where no
    /// live source can answer, a provider honouring this falls back to the
    /// reading it was holding rather than failing the refresh — see
    /// `ClaudeOAuthProvider.fetchSnapshot(freshness:)`.
    case fromSource
}
