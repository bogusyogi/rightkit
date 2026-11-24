//! Uppercase the first alphabetic character, leaving all preceding characters unchanged.

pub fn capitalize_first_alpha(input: &str) -> String {
    let Some((index, first)) = input
        .char_indices()
        .find(|(_, value)| value.is_alphabetic())
    else {
        return input.to_string();
    };
    let upper = first.to_uppercase().to_string();
    if upper == first.to_string() {
        return input.to_string();
    }
    let mut output = input.to_string();
    output.replace_range(index..index + first.len_utf8(), &upper);
    output
}
