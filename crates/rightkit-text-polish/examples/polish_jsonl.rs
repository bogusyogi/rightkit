//! Apply the deterministic polish to JSON-lines on stdin: one `{"id": "...", "text": "..."}` object
//! per line in, the same shape with `text` polished out.
//!
//! Run: cargo run -p rightkit-text-polish --example polish_jsonl < in.jsonl > out.jsonl

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    rightkit_text_polish::polish_jsonl::polish_jsonl(
        stdin.lock(),
        stdout.lock(),
        rightkit_text_polish::text_pipeline::deterministic_polish,
    );
}
