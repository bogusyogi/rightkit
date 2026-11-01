//! Coverage fields and a blur for soft shadows.

use rightkit_win_ui::canvas::Canvas;

/// The coverage of everything drawn on `canvas`, 0..=1 per pixel.
pub fn coverage(canvas: &Canvas) -> Vec<f32> {
    canvas
        .pixels
        .iter()
        .map(|pixel| (pixel >> 24) as f32 / 255.0)
        .collect()
}

/// One box blur pass along rows, then along columns, `radius` pixels each way.
fn box_blur(field: &mut [f32], width: usize, height: usize, radius: usize) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let span = (2 * radius + 1) as f32;
    let mut line = vec![0.0f32; width.max(height)];
    let mut pass = |field: &mut [f32], length: usize, lines: usize, stride: usize, jump: usize| {
        for index in 0..lines {
            let base = index * jump;
            for position in 0..length {
                line[position] = field[base + position * stride];
            }
            let at = |position: isize| -> f32 {
                if position < 0 || position >= length as isize {
                    0.0
                } else {
                    line[position as usize]
                }
            };
            let mut sum: f32 = (0..=radius as isize).map(&at).sum();
            for position in 0..length as isize {
                field[base + position as usize * stride] = sum / span;
                sum += at(position + radius as isize + 1) - at(position - radius as isize);
            }
        }
    };
    pass(field, width, height, 1, width);
    pass(field, height, width, width, 1);
}

/// Close to a Gaussian blur of `sigma` pixels: three box passes.
pub fn blur(field: &mut [f32], width: usize, height: usize, sigma: f32) {
    let radius = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    for _ in 0..3 {
        box_blur(field, width, height, radius);
    }
}
