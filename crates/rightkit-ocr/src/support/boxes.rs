use image::{imageops, RgbImage};

#[derive(Debug, Clone)]
pub(crate) struct BBox {
    pub(crate) y0: u32,
    pub(crate) x0: u32,
    pub(crate) y1: u32,
    pub(crate) x1: u32,
}

/// Threshold the [H×W] prob map, find connected components, map to band
/// pixels, and "unclip" (dilate) each box.
///
/// `bw`/`bh` are the band's pixel dimensions (for clamping). Tight DB boxes
/// were clipping ascenders/descenders, so the recognizer saw partial glyphs
/// and truncated reads; padding by a fraction of box height gives it the full
/// line plus a little context.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_boxes(
    prob: &[f32],
    w: usize,
    h: usize,
    scale_x: f32,
    scale_y: f32,
    thresh: f32,
    bw: u32,
    bh: u32,
) -> Vec<BBox> {
    let mut binary = vec![0u8; h * w];
    for i in 0..h * w {
        if prob[i] > thresh {
            binary[i] = 1;
        }
    }

    // Lower than the old 8×5: on native-resolution bands a real body-text line
    // is comfortably above this, and small lines no longer need to survive a
    // 960px downscale, but we still drop single-pixel speckle.
    const MIN_W: u32 = 4;
    const MIN_H: u32 = 3;

    simple_cc(&binary, w, h)
        .into_iter()
        .filter_map(|(y0n, x0n, y1n, x1n)| {
            if x1n - x0n < MIN_W || y1n - y0n < MIN_H {
                return None;
            }
            // prob-map coords → band pixels.
            let fx0 = x0n as f32 * scale_x;
            let fy0 = y0n as f32 * scale_y;
            let fx1 = x1n as f32 * scale_x;
            let fy1 = y1n as f32 * scale_y;
            // Unclip by a fraction of box height.
            let box_h = (fy1 - fy0).max(1.0);
            let pad_y = box_h * 0.30;
            let pad_x = box_h * 0.15;
            let x0 = (fx0 - pad_x).max(0.0) as u32;
            let y0 = (fy0 - pad_y).max(0.0) as u32;
            let x1 = ((fx1 + pad_x) as u32).min(bw);
            let y1 = ((fy1 + pad_y) as u32).min(bh);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            Some(BBox { y0, x0, y1, x1 })
        })
        .collect()
}

/// Box area in pixels².
fn box_area(b: &BBox) -> u64 {
    (b.x1.saturating_sub(b.x0)) as u64 * (b.y1.saturating_sub(b.y0)) as u64
}

/// True when `b` essentially duplicates / is contained in `k` (≥60% of the
/// smaller box covered). Used to collapse the same line detected in two
/// overlapping bands.
fn boxes_duplicate(k: &BBox, b: &BBox) -> bool {
    let ix0 = k.x0.max(b.x0);
    let iy0 = k.y0.max(b.y0);
    let ix1 = k.x1.min(b.x1);
    let iy1 = k.y1.min(b.y1);
    if ix1 <= ix0 || iy1 <= iy0 {
        return false;
    }
    let inter = (ix1 - ix0) as u64 * (iy1 - iy0) as u64;
    let smaller = box_area(k).min(box_area(b)).max(1);
    inter * 100 / smaller >= 60
}

/// Drop near-duplicate boxes from band seams; the larger-area box wins.
pub(crate) fn dedup_boxes(mut boxes: Vec<BBox>) -> Vec<BBox> {
    boxes.sort_by_key(|b| std::cmp::Reverse(box_area(b)));
    let mut kept: Vec<BBox> = Vec::with_capacity(boxes.len());
    for b in boxes {
        if kept.iter().any(|k| boxes_duplicate(k, &b)) {
            continue;
        }
        kept.push(b);
    }
    kept
}

/// Scanline union-find connected components on a 4-connected binary image.
///
/// `binary`: flat [H×W] array; non-zero = foreground.
/// Returns list of (y0, x0, y1, x1) bounding boxes.
fn simple_cc(binary: &[u8], w: usize, h: usize) -> Vec<(u32, u32, u32, u32)> {
    let mut labels = vec![0usize; w * h];
    // parent[0] unused; labels start at 1.
    let mut parent: Vec<usize> = vec![0, 1];
    let mut next = 1usize;

    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    fn union(p: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p[ra] = rb;
        }
    }

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            if binary[idx] == 0 {
                continue;
            }
            let up = if y > 0 { labels[(y - 1) * w + x] } else { 0 };
            let lf = if x > 0 { labels[y * w + x - 1] } else { 0 };
            labels[idx] = match (up, lf) {
                (0, 0) => {
                    next += 1;
                    parent.push(next);
                    next
                }
                (a, 0) | (0, a) => a,
                (a, b) => {
                    union(&mut parent, a, b);
                    find(&mut parent, a)
                }
            };
        }
    }

    // Resolve and gather bounding boxes.
    use std::collections::HashMap;
    let mut bboxes: HashMap<usize, (usize, usize, usize, usize)> = HashMap::new();
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l == 0 {
                continue;
            }
            let r = find(&mut parent, l);
            let e = bboxes.entry(r).or_insert((y, x, y, x));
            if y < e.0 {
                e.0 = y;
            }
            if x < e.1 {
                e.1 = x;
            }
            if y > e.2 {
                e.2 = y;
            }
            if x > e.3 {
                e.3 = x;
            }
        }
    }

    bboxes
        .values()
        .map(|&(y0, x0, y1, x1)| (y0 as u32, x0 as u32, y1 as u32, x1 as u32))
        .collect()
}

/// Crop an axis-aligned bounding box from the image (clamped to image bounds).
pub(crate) fn crop_box(img: &RgbImage, b: &BBox) -> RgbImage {
    let x0 = b.x0.min(img.width().saturating_sub(1));
    let y0 = b.y0.min(img.height().saturating_sub(1));
    let x1 = b.x1.min(img.width());
    let y1 = b.y1.min(img.height());
    if x1 <= x0 || y1 <= y0 {
        return RgbImage::new(0, 0);
    }
    imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image()
}

/// Group boxes into visual text lines by baseline overlap, top-to-bottom, then
/// order boxes within each line left-to-right.
///
/// A box joins the current line when its vertical center is within 60% of the
/// taller of (box, line) heights from the line's mean center; otherwise it
/// starts a new line. Restores correct reading order when detection splits a
/// line into multiple word boxes.
pub(crate) fn group_into_lines(mut boxes: Vec<BBox>) -> Vec<Vec<BBox>> {
    // Sort top-to-bottom by vertical center (y0+y1 avoids the /2 rounding).
    boxes.sort_by_key(|a| a.y0 + a.y1);
    let mut rows: Vec<Vec<BBox>> = Vec::new();
    for b in boxes {
        let bc = (b.y0 + b.y1) / 2;
        let bh = b.y1.saturating_sub(b.y0).max(1);
        // Decide with an immutable borrow so `b` is moved exactly once below.
        let attach = match rows.last() {
            Some(row) => {
                let cnt = row.len() as u32;
                let rc = row.iter().map(|r| (r.y0 + r.y1) / 2).sum::<u32>() / cnt;
                let rh = row
                    .iter()
                    .map(|r| r.y1.saturating_sub(r.y0))
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let tol = (bh.max(rh) as f32 * 0.6) as u32;
                bc.abs_diff(rc) <= tol
            }
            None => false,
        };
        if attach {
            rows.last_mut().unwrap().push(b);
        } else {
            rows.push(vec![b]);
        }
    }
    for row in rows.iter_mut() {
        row.sort_by_key(|a| a.x0);
    }
    rows
}
