# Changelog

## 0.1.1

- `trim` remaps the column files instead of `madvise(DONTNEED)` and runs after reopen and, at most every 2 s, after a search on stores above 500k slots; it takes the write lock briefly.
- Hidden diagnostics `aux_counts` / `special_count`; `footprint` bench takes the entry count after `--`.
- README documents memory and on-disk layout; package metadata (documentation, keywords, categories, include) completed.

## 0.1.0

- Initial release: macOS getattrlistbulk crawl, FSEvents upkeep with event-ID resume, fuzzy and one-typo name search,
  64-bit logical/allocated/allocated-unique folder rollups (APFS clone and hard-link aware), package flag,
  coalesced change subscription, opt-in `content` feature (trigram search).
- Index state lives in memory-mapped fixed-width column files with a generation header; clean commits reopen without a crawl.
