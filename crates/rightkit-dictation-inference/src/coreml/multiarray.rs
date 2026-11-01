// `MLMultiArray` construction, in-place element writes, and stride-aware reads.
// Stage loading/prediction lives in `stage.rs`.
use half::f16;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_ml::{MLMultiArray, MLMultiArrayDataType};
use objc2_foundation::{NSArray, NSNumber};

use super::stage::nserr;

/// Build an i32 MLMultiArray.
pub fn ml_i32(dims: &[usize], data: &[i32]) -> Result<Retained<MLMultiArray>, String> {
    if super::aligned_probe::enabled() {
        return finish_probe(super::aligned_probe::aligned_i32(dims, data));
    }
    unsafe {
        let arr = new_marray(dims, MLMultiArrayDataType::Int32)?;
        let ptr = arr.dataPointer().as_ptr() as *mut i32;
        for (i, &x) in data.iter().enumerate() {
            *ptr.add(i) = x;
        }
        finish_probe(Ok(arr))
    }
}

/// Write one f16 element (by linear C-contiguous index) into an existing array —
/// for mutating a reused buffer in place (e.g. the KV cache) instead of rebuilding
/// the whole MLMultiArray each decode step.
pub fn f16_set(arr: &MLMultiArray, idx: usize, val: f32) {
    unsafe {
        let ptr = arr.dataPointer().as_ptr() as *mut f16;
        *ptr.add(idx) = f16::from_f32(val);
    }
}

/// Write one i32 element (by linear C-contiguous index) into an existing array.
pub fn i32_set(arr: &MLMultiArray, idx: usize, val: i32) {
    unsafe {
        let ptr = arr.dataPointer().as_ptr() as *mut i32;
        *ptr.add(idx) = val;
    }
}

unsafe fn new_marray(
    dims: &[usize],
    dt: MLMultiArrayDataType,
) -> Result<Retained<MLMultiArray>, String> {
    let nums: Vec<Retained<NSNumber>> = dims
        .iter()
        .map(|&d| NSNumber::new_isize(d as isize))
        .collect();
    let refs: Vec<&NSNumber> = nums.iter().map(|n| n.as_ref()).collect();
    let shape = NSArray::from_slice(&refs);
    MLMultiArray::initWithShape_dataType_error(MLMultiArray::alloc(), &shape, dt)
        .map_err(|e| format!("MLMultiArray alloc: {}", nserr(&e)))
}

/// Build an f32 MLMultiArray from f32 data (row-major, C-contiguous).
pub fn ml_f32(dims: &[usize], data: &[f32]) -> Result<Retained<MLMultiArray>, String> {
    if super::aligned_probe::enabled() {
        return finish_probe(super::aligned_probe::aligned_f32(dims, data));
    }
    unsafe {
        let arr = new_marray(dims, MLMultiArrayDataType::Float32)?;
        let ptr = arr.dataPointer().as_ptr() as *mut f32;
        for (i, &x) in data.iter().enumerate() {
            *ptr.add(i) = x;
        }
        finish_probe(Ok(arr))
    }
}

/// Build an f16 MLMultiArray from f32 data (row-major, C-contiguous).
pub fn ml_f16(dims: &[usize], data: &[f32]) -> Result<Retained<MLMultiArray>, String> {
    if super::aligned_probe::enabled() {
        return finish_probe(super::aligned_probe::aligned_f16(dims, data));
    }
    unsafe {
        let arr = new_marray(dims, MLMultiArrayDataType::Float16)?;
        let ptr = arr.dataPointer().as_ptr() as *mut f16;
        for (i, &x) in data.iter().enumerate() {
            *ptr.add(i) = f16::from_f32(x);
        }
        finish_probe(Ok(arr))
    }
}

fn finish_probe(
    result: Result<Retained<MLMultiArray>, String>,
) -> Result<Retained<MLMultiArray>, String> {
    let array = result?;
    if super::aligned_probe::prefetch_enabled() {
        super::aligned_probe::touch_logical_edges(&array)?;
    }
    Ok(array)
}

/// Row-padded fast path shared by `read_f32` / `read_f16`.
///
/// MEASURED (iPhone, 2026-07-19; the macOS layout matches): the Parakeet
/// encoder output is shape `1x1024x188` with strides `196608x192x1` — every
/// 188-element row is contiguous but padded to a 192-element pitch. That is not
/// C-contiguous, so the generic gather below walks all 192,512 elements one at a
/// time. On iOS that cost 47 ms for one encoder read, more than the encoder
/// prediction itself. When the innermost stride is 1 and the outer dims stack
/// cleanly on the row pitch, each row can be copied wholesale instead.
///
/// Returns `Some(row_len, row_pitch, rows)` when the fast path applies.
fn row_layout(shape: &[usize], strides: &[isize]) -> Option<(usize, usize, usize)> {
    // Escape hatch for A/B measurement; unset in production.
    if rightkit_dictation_core::dev_var_os!("RIGHTKIT_COREML_NO_ROW_FAST_PATH").is_some() {
        return None;
    }
    if shape.len() < 2 || *strides.last()? != 1 {
        return None;
    }
    let row_len = *shape.last()?;
    let row_pitch = usize::try_from(strides[shape.len() - 2]).ok()?;
    if row_pitch < row_len {
        return None;
    }
    let mut expected = row_pitch;
    for dim in (1..shape.len() - 1).rev() {
        expected = expected.checked_mul(shape[dim])?;
        if usize::try_from(strides[dim - 1]).ok()? != expected {
            return None;
        }
    }
    let total = shape
        .iter()
        .try_fold(1usize, |product, dimension| product.checked_mul(*dimension))?;
    Some((row_len, row_pitch, total / row_len.max(1)))
}

/// Read an f32 MLMultiArray in **C-contiguous order**, honoring real strides
/// (CoreML outputs are not guaranteed contiguous).
pub fn read_f32(arr: &MLMultiArray) -> Vec<f32> {
    unsafe {
        let shape: Vec<usize> = arr
            .shape()
            .iter()
            .map(|n| n.integerValue() as usize)
            .collect();
        let strides: Vec<isize> = arr.strides().iter().map(|n| n.integerValue()).collect();
        let ptr = arr.dataPointer().as_ptr() as *const f32;
        let total: usize = shape.iter().product();
        let ndim = shape.len();
        if let Some((row_len, row_pitch, rows)) = row_layout(&shape, &strides) {
            let mut out = Vec::with_capacity(total);
            for row in 0..rows {
                let src = ptr.add(row * row_pitch);
                out.extend_from_slice(std::slice::from_raw_parts(src, row_len));
            }
            return out;
        }
        let mut out = Vec::with_capacity(total);
        let mut idx = vec![0usize; ndim];
        for _ in 0..total {
            let mut off: isize = 0;
            for k in 0..ndim {
                off += idx[k] as isize * strides[k];
            }
            out.push(*ptr.offset(off));
            for k in (0..ndim).rev() {
                idx[k] += 1;
                if idx[k] < shape[k] {
                    break;
                }
                idx[k] = 0;
            }
        }
        out
    }
}

/// Read an f16 MLMultiArray to f32 in **C-contiguous order**, honoring real strides
/// (CoreML outputs are not guaranteed contiguous).
pub fn read_f16(arr: &MLMultiArray) -> Vec<f32> {
    unsafe {
        let shape: Vec<usize> = arr
            .shape()
            .iter()
            .map(|n| n.integerValue() as usize)
            .collect();
        let strides: Vec<isize> = arr.strides().iter().map(|n| n.integerValue()).collect();
        let ptr = arr.dataPointer().as_ptr() as *const f16;
        let total: usize = shape.iter().product();
        let ndim = shape.len();
        if let Some((row_len, row_pitch, rows)) = row_layout(&shape, &strides) {
            let mut out = Vec::with_capacity(total);
            for row in 0..rows {
                let src = ptr.add(row * row_pitch);
                for i in 0..row_len {
                    out.push((*src.add(i)).to_f32());
                }
            }
            return out;
        }
        let mut out = Vec::with_capacity(total);
        let mut idx = vec![0usize; ndim];
        for _ in 0..total {
            let mut off: isize = 0;
            for k in 0..ndim {
                off += idx[k] as isize * strides[k];
            }
            out.push((*ptr.offset(off)).to_f32());
            for k in (0..ndim).rev() {
                idx[k] += 1;
                if idx[k] < shape[k] {
                    break;
                }
                idx[k] = 0;
            }
        }
        out
    }
}

#[cfg(test)]
mod row_layout_tests {
    use super::row_layout;

    /// Reference implementation: the element-wise odometer the fast path must
    /// agree with, byte for byte.
    fn odometer_gather(data: &[f32], shape: &[usize], strides: &[isize]) -> Vec<f32> {
        let total: usize = shape.iter().product();
        let ndim = shape.len();
        let mut out = Vec::with_capacity(total);
        let mut idx = vec![0usize; ndim];
        for _ in 0..total {
            let mut off: isize = 0;
            for k in 0..ndim {
                off += idx[k] as isize * strides[k];
            }
            out.push(data[off as usize]);
            for k in (0..ndim).rev() {
                idx[k] += 1;
                if idx[k] < shape[k] {
                    break;
                }
                idx[k] = 0;
            }
        }
        out
    }

    fn rowwise_gather(data: &[f32], row_len: usize, row_pitch: usize, rows: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(row_len * rows);
        for row in 0..rows {
            out.extend_from_slice(&data[row * row_pitch..row * row_pitch + row_len]);
        }
        out
    }

    #[test]
    fn detects_the_real_ane_encoder_layout() {
        // Measured on device 2026-07-19: rows of 188 padded to a 192 pitch.
        let shape = [1usize, 1024, 188];
        let strides = [196_608isize, 192, 1];
        assert_eq!(row_layout(&shape, &strides), Some((188, 192, 1024)));
    }

    #[test]
    fn plain_contiguous_is_row_layout_with_equal_pitch() {
        let shape = [1usize, 4, 3];
        let strides = [12isize, 3, 1];
        assert_eq!(row_layout(&shape, &strides), Some((3, 3, 4)));
    }

    #[test]
    fn rejects_non_unit_innermost_stride() {
        // Transposed / interleaved: only the odometer is correct here.
        let shape = [1usize, 4, 3];
        let strides = [12isize, 1, 4];
        assert_eq!(row_layout(&shape, &strides), None);
        assert_eq!(row_layout(&[1, 3, 4], &[-12, 4, 1]), None);
    }

    #[test]
    fn rejects_outer_dims_that_do_not_stack_on_the_pitch() {
        let shape = [2usize, 4, 3];
        let strides = [999isize, 4, 1]; // outer stride is not 4*4
        assert_eq!(row_layout(&shape, &strides), None);
    }

    #[test]
    fn fast_path_matches_the_odometer_on_padded_rows() {
        // 4 rows of 3 values, padded to a pitch of 5; filler must never appear.
        let row_len = 3usize;
        let pitch = 5usize;
        let rows = 4usize;
        let mut data = vec![-1.0f32; pitch * rows];
        for row in 0..rows {
            for col in 0..row_len {
                data[row * pitch + col] = (row * row_len + col) as f32;
            }
        }
        let shape = [1usize, rows, row_len];
        let strides = [(pitch * rows) as isize, pitch as isize, 1];

        let (rl, rp, r) = row_layout(&shape, &strides).expect("row layout applies");
        let fast = rowwise_gather(&data, rl, rp, r);
        let reference = odometer_gather(&data, &shape, &strides);

        assert_eq!(fast, reference);
        assert_eq!(fast, (0..12).map(|v| v as f32).collect::<Vec<_>>());
        assert!(!fast.contains(&-1.0), "row padding leaked into the output");
    }
}
