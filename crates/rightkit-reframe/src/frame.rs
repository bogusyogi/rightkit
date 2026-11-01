//! A decoded RGB frame and the small pixel helpers the analysers share.

#[derive(Debug, Clone)]
pub struct RgbFrame {
    pub width: usize,
    pub height: usize,
    /// Tightly packed `rgb24`, `width * height * 3` bytes.
    pub data: Vec<u8>,
}

impl RgbFrame {
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), width * height * 3);
        Self {
            width,
            height,
            data,
        }
    }

    pub fn solid(width: usize, height: usize, rgb: [u8; 3]) -> Self {
        let mut data = Vec::with_capacity(width * height * 3);
        for _ in 0..width * height {
            data.extend_from_slice(&rgb);
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// Rec.601 luma, one byte per pixel.
    pub fn to_gray(&self) -> GrayFrame {
        let mut out = Vec::with_capacity(self.width * self.height);
        for px in self.data.as_chunks::<3>().0 {
            let y = (77 * px[0] as u32 + 150 * px[1] as u32 + 29 * px[2] as u32) >> 8;
            out.push(y as u8);
        }
        GrayFrame {
            width: self.width,
            height: self.height,
            data: out,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrayFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl GrayFrame {
    /// Mean absolute difference against `other` inside the normalized region
    /// `[x0,x1] x [y0,y1]`. `None` when the frames differ in size or the region is empty.
    pub fn region_motion(
        &self,
        other: &GrayFrame,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) -> Option<f32> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        let px0 = ((x0.clamp(0.0, 1.0)) * self.width as f32) as usize;
        let px1 = ((x1.clamp(0.0, 1.0)) * self.width as f32).ceil() as usize;
        let py0 = ((y0.clamp(0.0, 1.0)) * self.height as f32) as usize;
        let py1 = ((y1.clamp(0.0, 1.0)) * self.height as f32).ceil() as usize;
        let (px1, py1) = (px1.min(self.width), py1.min(self.height));
        if px1 <= px0 || py1 <= py0 {
            return None;
        }
        let mut sum = 0u64;
        for y in py0..py1 {
            let row = y * self.width;
            for x in px0..px1 {
                sum +=
                    (self.data[row + x] as i32 - other.data[row + x] as i32).unsigned_abs() as u64;
            }
        }
        Some(sum as f32 / ((px1 - px0) * (py1 - py0)) as f32)
    }
}
