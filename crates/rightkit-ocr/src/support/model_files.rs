use image::RgbImage;
use std::path::Path;

/// Load character dictionary.
/// Vocab: ['\0'(blank), char₁, …, charₙ, ' '(space)] where N = dict line count
/// (18,708 for the characterized multilingual dictionary). The decoder reads the
/// real class count from the model output, so the trailing space is only used
/// when the model actually emits that class.
pub(crate) fn load_vocab(dict_path: &Path) -> Result<Vec<char>, String> {
    let raw = std::fs::read(dict_path).map_err(|e| format!("Cannot read {:?}: {e}", dict_path))?;
    let text = String::from_utf8_lossy(&raw);
    let mut vocab = vec!['\0']; // index 0 = CTC blank
    for line in text.lines() {
        if let Some(c) = line.chars().next() {
            vocab.push(c);
        }
    }
    vocab.push(' '); // index 437 = space
    Ok(vocab)
}

pub(crate) fn load_image(path: &Path) -> Result<RgbImage, String> {
    image::open(path)
        .map(|i| i.into_rgb8())
        .map_err(|e| format!("Cannot open image {:?}: {e}", path))
}
