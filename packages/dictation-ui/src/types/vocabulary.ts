// Vocabulary types - mirrors the Rust `VocabularyEntry` (src-tauri/src/vocabulary.rs).

export interface VocabularyEntry {
  id: number;
  term: string;
  created_at_ms: number;
  /** Optional sound-alike aliases (e.g. "whisper flow" -> "Wispr Flow").
   *  Always present in the renderer type for forward compat; older
   *  vocabularies without the field deserialize with `[]`. */
  sounds_like?: string[];
}

export interface VocabularySuggestion {
  term: string;
  heard_as: string;
  seen: number;
  created_at_ms: number;
}
