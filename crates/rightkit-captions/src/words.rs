//! Caption input: word timings in OUTPUT time. The type, the source -> output mapping (which
//! records each word's source span in `source_ms`) and the drop tolerance are RightKit's.

pub use rightkit_transcript::{words_from_transcript, TimedWord, MAP_DURATION_TOLERANCE_MS};
