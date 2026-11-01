//! Conservative upper/lower safe-band placement based on decoded image edges.
//! Measures contrast edges & text-sized neutral ink, not OCR. The engine samples fitted
//! frames before rendering; explicit caption styles retain their own anchors.

/// Mean horizontal + vertical edge magnitude in the two caption-safe bands.
/// Input must be a non-empty, tightly packed 8-bit grayscale frame.
pub fn band_edges(gray: &[u8], width: usize, height: usize) -> Option<[f64; 2]> {
    if width < 10 || height < 10 || gray.len() != width.checked_mul(height)? {
        return None;
    }
    let score = |start: usize, end: usize| {
        let mut sum = 0u64;
        let mut count = 0u64;
        for y in start..end {
            for x in width / 10..width * 9 / 10 {
                let i = y * width + x;
                sum += gray[i].abs_diff(gray[i - 1]) as u64;
                sum += gray[i].abs_diff(gray[i - width]) as u64;
                count += 2;
            }
        }
        sum as f64 / count as f64
    };
    Some([
        score(height * 7 / 100, height * 28 / 100),
        score(height * 70 / 100, height * 91 / 100),
    ])
}

/// Count rows of small, bright neutral components distributed across a band.
/// Texture alone is not UI evidence: a row needs at least twelve text-sized
/// components spanning half the frame. Input is fitted, tightly packed RGB8.
pub fn band_text_rows(rgb: &[u8], width: usize, height: usize) -> Option<[usize; 2]> {
    if width < 10 || height < 10 || rgb.len() != width.checked_mul(height)?.checked_mul(3)? {
        return None;
    }
    let mut seen = vec![false; width * height];
    let ink = |i: usize| {
        let p = &rgb[i * 3..i * 3 + 3];
        let lo = *p.iter().min().expect("RGB");
        let hi = *p.iter().max().expect("RGB");
        lo >= 180 && hi - lo <= 30
    };
    let mut result = [0; 2];
    for (band, (top, bottom)) in [
        (height * 7 / 100, height * 28 / 100),
        (height * 70 / 100, height * 91 / 100),
    ]
    .into_iter()
    .enumerate()
    {
        let mut components = Vec::new();
        for y in top..bottom {
            for x in width / 10..width * 9 / 10 {
                let i = y * width + x;
                if seen[i] || !ink(i) {
                    continue;
                }
                let mut stack = vec![i];
                seen[i] = true;
                let (mut left, mut right, mut low, mut high, mut area) = (x, x, y, y, 0);
                while let Some(i) = stack.pop() {
                    let (x, y) = (i % width, i / width);
                    left = left.min(x);
                    right = right.max(x);
                    low = low.min(y);
                    high = high.max(y);
                    area += 1;
                    for (nx, ny) in [
                        (x.saturating_sub(1), y),
                        (x + 1, y),
                        (x, y.saturating_sub(1)),
                        (x, y + 1),
                    ] {
                        if nx < width / 10 || nx >= width * 9 / 10 || ny < top || ny >= bottom {
                            continue;
                        }
                        let next = ny * width + nx;
                        if !seen[next] && ink(next) {
                            seen[next] = true;
                            stack.push(next);
                        }
                    }
                }
                // Scaled UI glyphs are small; reject broad shapes & tall
                // natural edges. Dots alone cannot establish a text row.
                if area >= 3
                    && (3..=height / 18).contains(&(high - low + 1))
                    && right - left < width / 30
                {
                    components.push((low, left, right));
                }
            }
        }
        components.sort_unstable();
        let mut first = 0;
        while first < components.len() {
            let row_y = components[first].0;
            let mut end = first;
            let (mut left, mut right) = (width, 0);
            while end < components.len() && components[end].0 <= row_y + 2 {
                left = left.min(components[end].1);
                right = right.max(components[end].2);
                end += 1;
            }
            if end - first >= 12 && right - left >= width / 2 {
                result[band] += 1;
            }
            first = end;
        }
    }
    Some(result)
}
