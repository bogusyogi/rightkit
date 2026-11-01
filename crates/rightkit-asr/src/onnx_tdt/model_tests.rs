use super::parse_vocab;

#[test]
fn reads_the_onnx_indexed_format() {
    // The id is authoritative and must NOT survive into the piece.
    let v = parse_vocab("<unk> 0\n\u{2581}the 1\n\u{2581}cat 2\n<blk> 3\n").unwrap();
    assert_eq!(v, ["<unk>", "\u{2581}the", "\u{2581}cat", "<blk>"]);
    assert_eq!(v.len() - 1, 3, "blank is the last row");
}

#[test]
fn reads_the_coreml_one_per_line_format() {
    let v = parse_vocab("<unk>\n\u{2581}the\n\u{2581}cat\n<blk>\n").unwrap();
    assert_eq!(v, ["<unk>", "\u{2581}the", "\u{2581}cat", "<blk>"]);
}

#[test]
fn honours_sparse_and_out_of_order_ids() {
    let v = parse_vocab("\u{2581}cat 2\n<unk> 0\n").unwrap();
    assert_eq!(v, ["<unk>", "", "\u{2581}cat"]);
}

#[test]
fn a_single_unindexed_line_makes_the_whole_file_plain() {
    // Otherwise a plain vocabulary containing one "word 7" piece would be
    // read as indexed and scatter every other piece to the wrong id.
    let v = parse_vocab("word 7\n\u{2581}plain\n").unwrap();
    assert_eq!(v, ["word 7", "\u{2581}plain"]);
}

#[test]
fn empty_is_an_error_not_an_empty_vocabulary() {
    // An empty vocabulary underflows `blank_id()` (`len() - 1`).
    assert!(parse_vocab("\n\n").is_err());
}
