use super::*;

#[test]
fn det_net_size_basics() {
    // Native resolution now (no 960 downscale): round each side up to ×32.
    // 1248 is already 39×32; 561 → 576.
    let (w, h) = det_net_size(1248, 561);
    assert_eq!(w, 1248, "width kept at native (rounded ×32)");
    assert_eq!(h, 576, "height rounded up to ×32");
    assert_eq!(w % 32, 0, "width must be ×32");
    assert_eq!(h % 32, 0, "height must be ×32");
}

#[test]
fn det_net_size_small() {
    // Small images are NOT upscaled — kept native, rounded to ×32.
    let (w, h) = det_net_size(320, 240);
    assert_eq!(w, 320);
    assert_eq!(h, 256);
    assert_eq!(w % 32, 0);
    assert_eq!(h % 32, 0);
}

#[test]
fn det_net_size_square() {
    // 1000 → 1024 (next ×32); no longer clamped to 960.
    let (w, h) = det_net_size(1000, 1000);
    assert_eq!(w, 1024);
    assert_eq!(h, 1024);
}

#[test]
fn det_net_size_caps_huge() {
    // A dimension above the hard cap is clamped to DET_MAX_SIDE (2048).
    let (w, _h) = det_net_size(5000, 100);
    assert_eq!(w, 2048, "huge width clamped to DET_MAX_SIDE");
}

#[test]
fn group_into_lines_orders_words() {
    // Two words on line 1 (same y), one word on line 2 — given out of order.
    let w1b = BBox {
        x0: 200,
        y0: 10,
        x1: 300,
        y1: 40,
    }; // line1, right
    let w1a = BBox {
        x0: 10,
        y0: 12,
        x1: 120,
        y1: 42,
    }; // line1, left
    let w2 = BBox {
        x0: 10,
        y0: 120,
        x1: 150,
        y1: 150,
    }; // line2
    let rows = group_into_lines(vec![w1b, w2, w1a]);
    assert_eq!(rows.len(), 2, "two visual lines");
    assert_eq!(rows[0].len(), 2, "line1 has both words");
    assert!(rows[0][0].x0 < rows[0][1].x0, "line1 ordered left→right");
}

#[test]
fn dedup_collapses_band_overlap() {
    // Two near-identical boxes (same line seen in two bands) → one kept.
    let a = BBox {
        x0: 10,
        y0: 10,
        x1: 200,
        y1: 40,
    };
    let b = BBox {
        x0: 11,
        y0: 11,
        x1: 201,
        y1: 41,
    };
    let c = BBox {
        x0: 10,
        y0: 200,
        x1: 180,
        y1: 230,
    }; // distinct line
    let out = dedup_boxes(vec![a, b, c]);
    assert_eq!(out.len(), 2, "overlapping duplicate should collapse");
}

#[test]
fn ctc_decode_basic() {
    // vocab = [blank, 'A', 'B', ' ']
    // T=4: A(1) A(1) B(2) blank(0) → "AB"
    let vocab: Vec<char> = vec!['\0', 'A', 'B', ' '];
    let logits: Vec<f32> = vec![
        0.0, 10.0, 0.0, 0.0, // t=0: A
        0.0, 10.0, 0.0, 0.0, // t=1: A (collapse)
        0.0, 0.0, 10.0, 0.0, // t=2: B
        10.0, 0.0, 0.0, 0.0, // t=3: blank (skip)
    ];
    assert_eq!(ctc_greedy_decode(&logits, 4, 4, &vocab), "AB");
}

#[test]
fn ctc_decode_all_blank() {
    let vocab: Vec<char> = vec!['\0', 'A', 'B'];
    let logits: Vec<f32> = vec![10.0, 0.0, 0.0, 10.0, 0.0, 0.0];
    assert_eq!(ctc_greedy_decode(&logits, 2, 3, &vocab), "");
}
