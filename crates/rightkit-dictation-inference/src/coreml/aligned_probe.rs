//! Opt-in CoreML input allocation probe.
//!
//! This is deliberately separate from normal allocator. The Objective-C
//! bridge owns a 64-byte aligned allocation & gives CoreML explicit strides;
//! Rust validates layout & copies logical C-order values into it.

use std::ffi::c_void;

use half::f16;
use objc2::rc::Retained;
use objc2_core_ml::{MLMultiArray, MLMultiArrayDataType};
use objc2_foundation::NSString;

unsafe extern "C" {
    fn rightkit_dictation_coreml_aligned_array_f16(
        shape: *const i64,
        rank: usize,
        error_out: *mut *mut c_void,
        exception_out: *mut *mut c_void,
    ) -> *mut c_void;
    fn rightkit_dictation_coreml_aligned_array_f32(
        shape: *const i64,
        rank: usize,
        error_out: *mut *mut c_void,
        exception_out: *mut *mut c_void,
    ) -> *mut c_void;
    fn rightkit_dictation_coreml_aligned_array_i32(
        shape: *const i64,
        rank: usize,
        error_out: *mut *mut c_void,
        exception_out: *mut *mut c_void,
    ) -> *mut c_void;
}

/// True when aligned input allocation should be selected by caller.
pub fn enabled() -> bool {
    rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_ARRAY_ALIGNMENT_PROBE").as_deref() == Ok("1")
}

/// True when separate first/last-value prefetch touch should run.
pub fn prefetch_enabled() -> bool {
    rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_ARRAY_PREFETCH_PROBE").as_deref() == Ok("1")
}

/// Build aligned Float32 array for logical C-order input.
pub fn aligned_f32(dims: &[usize], data: &[f32]) -> Result<Retained<MLMultiArray>, String> {
    let layout = Layout::new(dims, data.len(), std::mem::size_of::<f32>())?;
    let array = unsafe { allocate(&layout, rightkit_dictation_coreml_aligned_array_f32)? };
    unsafe {
        copy_strided(&array, &layout, data, MLMultiArrayDataType::Float32)?;
    }
    Ok(array)
}

/// Build aligned Float16 array from logical Float32 values.
pub fn aligned_f16(dims: &[usize], data: &[f32]) -> Result<Retained<MLMultiArray>, String> {
    let layout = Layout::new(dims, data.len(), std::mem::size_of::<f16>())?;
    let array = unsafe { allocate(&layout, rightkit_dictation_coreml_aligned_array_f16)? };
    unsafe {
        copy_f32_as_f16(&array, &layout, data)?;
    }
    Ok(array)
}

/// Build aligned Int32 array for logical C-order input.
pub fn aligned_i32(dims: &[usize], data: &[i32]) -> Result<Retained<MLMultiArray>, String> {
    let layout = Layout::new(dims, data.len(), std::mem::size_of::<i32>())?;
    let array = unsafe { allocate(&layout, rightkit_dictation_coreml_aligned_array_i32)? };
    unsafe {
        copy_strided(&array, &layout, data, MLMultiArrayDataType::Int32)?;
    }
    Ok(array)
}

/// Touch only first & last logical elements of existing array.
///
/// Caller should gate this with prefetch_enabled. This function never
/// allocates, changes values, or reuses an input allocation.
pub fn touch_logical_edges(array: &MLMultiArray) -> Result<(), String> {
    unsafe {
        let shape = read_positive_shape(array)?;
        let strides = read_nonnegative_strides(array, shape.len())?;
        let last_offset = shape.iter().zip(strides.iter()).try_fold(
            0usize,
            |offset, (&dimension, &stride)| {
                let term = (dimension - 1)
                    .checked_mul(stride)
                    .ok_or_else(|| "prefetch offset overflow".to_string())?;
                offset
                    .checked_add(term)
                    .ok_or_else(|| "prefetch offset overflow".to_string())
            },
        )?;
        let ptr = array.dataPointer().as_ptr();
        match array.dataType() {
            MLMultiArrayDataType::Float16 => {
                let ptr = ptr.cast::<f16>();
                std::ptr::read_volatile(ptr.add(0));
                std::ptr::read_volatile(ptr.add(last_offset));
            }
            MLMultiArrayDataType::Float32 => {
                let ptr = ptr.cast::<f32>();
                std::ptr::read_volatile(ptr.add(0));
                std::ptr::read_volatile(ptr.add(last_offset));
            }
            MLMultiArrayDataType::Int32 => {
                let ptr = ptr.cast::<i32>();
                std::ptr::read_volatile(ptr.add(0));
                std::ptr::read_volatile(ptr.add(last_offset));
            }
            _ => return Err("prefetch probe: unsupported MLMultiArray type".into()),
        }
        Ok(())
    }
}

type NativeAllocator =
    unsafe extern "C" fn(*const i64, usize, *mut *mut c_void, *mut *mut c_void) -> *mut c_void;

struct Layout {
    dims: Vec<i64>,
    shape: Vec<usize>,
    strides: Vec<usize>,
    logical_count: usize,
    #[cfg(test)]
    storage_elements: usize,
}

impl Layout {
    fn new(dims: &[usize], value_count: usize, element_size: usize) -> Result<Self, String> {
        if dims.is_empty() {
            return Err("aligned probe: rank must be positive".into());
        }
        if dims.len() > 64 {
            return Err("aligned probe: rank is too large".into());
        }
        let mut logical_count = 1usize;
        let mut dims_i64 = Vec::with_capacity(dims.len());
        for &dimension in dims {
            if dimension == 0 {
                return Err("aligned probe: dimensions must be positive".into());
            }
            dims_i64.push(
                i64::try_from(dimension)
                    .map_err(|_| "aligned probe: dimension exceeds signed 64-bit range")?,
            );
            logical_count = logical_count
                .checked_mul(dimension)
                .ok_or_else(|| "aligned probe: logical element count overflow".to_string())?;
        }
        if logical_count != value_count {
            return Err(format!(
                "aligned probe: shape expects {logical_count} values, got {value_count}"
            ));
        }
        let last = dims.len() - 1;
        let padded_last = dims[last]
            .checked_add(15)
            .ok_or_else(|| "aligned probe: padded stride overflow".to_string())?
            / 16
            * 16;
        let mut strides = vec![0usize; dims.len()];
        strides[last] = 1;
        let mut storage_elements = padded_last;
        if dims.len() > 1 {
            strides[last - 1] = storage_elements;
            for axis in (1..last).rev() {
                storage_elements = storage_elements
                    .checked_mul(dims[axis])
                    .ok_or_else(|| "aligned probe: storage size overflow".to_string())?;
                strides[axis - 1] = storage_elements;
            }
            storage_elements = storage_elements
                .checked_mul(dims[0])
                .ok_or_else(|| "aligned probe: storage size overflow".to_string())?;
        }
        storage_elements
            .checked_mul(element_size)
            .ok_or_else(|| "aligned probe: byte size overflow".to_string())?;
        Ok(Self {
            dims: dims_i64,
            shape: dims.to_vec(),
            strides,
            logical_count,
            #[cfg(test)]
            storage_elements,
        })
    }
}

unsafe fn allocate(
    layout: &Layout,
    allocator: NativeAllocator,
) -> Result<Retained<MLMultiArray>, String> {
    let mut error = std::ptr::null_mut();
    let mut exception = std::ptr::null_mut();
    let raw = allocator(
        layout.dims.as_ptr(),
        layout.dims.len(),
        &mut error,
        &mut exception,
    );
    if !exception.is_null() {
        let description = Retained::<NSString>::from_raw(exception.cast())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown Objective-C exception".into());
        return Err(format!("aligned probe exception: {description}"));
    }
    if !error.is_null() {
        let description = Retained::<NSString>::from_raw(error.cast())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown CoreML allocation error".into());
        return Err(format!("aligned probe allocation: {description}"));
    }
    Retained::<MLMultiArray>::from_raw(raw.cast())
        .ok_or_else(|| "aligned probe allocation returned no array".into())
}

unsafe fn copy_strided<T: Copy>(
    array: &MLMultiArray,
    layout: &Layout,
    values: &[T],
    expected_type: MLMultiArrayDataType,
) -> Result<(), String> {
    let actual_strides = validate_array_layout(array, layout, expected_type)?;
    if values.len() != layout.logical_count {
        return Err("aligned probe: value count changed during copy".into());
    }
    let ptr = array.dataPointer().as_ptr().cast::<T>();
    let row_len = *layout.shape.last().unwrap();
    let row_pitch = actual_strides
        .get(layout.shape.len().saturating_sub(2))
        .copied()
        .unwrap_or(row_len);
    let row_count = layout.logical_count / row_len;
    for row in 0..row_count {
        let source_offset = row
            .checked_mul(row_len)
            .ok_or_else(|| "aligned probe: source offset overflow".to_string())?;
        let destination_offset = row
            .checked_mul(row_pitch)
            .ok_or_else(|| "aligned probe: destination offset overflow".to_string())?;
        std::ptr::copy_nonoverlapping(
            values.as_ptr().add(source_offset),
            ptr.add(destination_offset),
            row_len,
        );
    }
    Ok(())
}

unsafe fn copy_f32_as_f16(
    array: &MLMultiArray,
    layout: &Layout,
    values: &[f32],
) -> Result<(), String> {
    let actual_strides = validate_array_layout(array, layout, MLMultiArrayDataType::Float16)?;
    if values.len() != layout.logical_count {
        return Err("aligned probe: value count changed during copy".into());
    }
    let ptr = array.dataPointer().as_ptr().cast::<f16>();
    let row_len = *layout.shape.last().unwrap();
    let row_pitch = actual_strides
        .get(layout.shape.len().saturating_sub(2))
        .copied()
        .unwrap_or(row_len);
    let row_count = layout.logical_count / row_len;
    for row in 0..row_count {
        let source_offset = row
            .checked_mul(row_len)
            .ok_or_else(|| "aligned probe: source offset overflow".to_string())?;
        let destination_offset = row
            .checked_mul(row_pitch)
            .ok_or_else(|| "aligned probe: destination offset overflow".to_string())?;
        let destination = ptr.add(destination_offset);
        for (column, &value) in values[source_offset..source_offset + row_len]
            .iter()
            .enumerate()
        {
            std::ptr::write(destination.add(column), f16::from_f32(value));
        }
    }
    Ok(())
}

unsafe fn validate_array_layout(
    array: &MLMultiArray,
    layout: &Layout,
    expected_type: MLMultiArrayDataType,
) -> Result<Vec<usize>, String> {
    if array.dataType() != expected_type {
        return Err("aligned probe: native data type mismatch".into());
    }
    let actual_shape = read_positive_shape(array)?;
    if actual_shape != layout.shape {
        return Err("aligned probe: native shape mismatch".into());
    }
    let actual_strides = read_nonnegative_strides(array, layout.shape.len())?;
    if actual_strides != layout.strides {
        return Err(format!(
            "aligned probe: native strides mismatch: expected {:?}, got {:?}",
            layout.strides, actual_strides
        ));
    }
    Ok(actual_strides)
}

unsafe fn read_positive_shape(array: &MLMultiArray) -> Result<Vec<usize>, String> {
    let shape: Vec<usize> = array
        .shape()
        .iter()
        .map(|dimension| {
            let value = dimension.integerValue();
            usize::try_from(value)
                .map_err(|_| "aligned probe: negative shape dimension".to_string())
        })
        .collect::<Result<_, _>>()?;
    if shape.is_empty() || shape.contains(&0) {
        return Err("aligned probe: empty or zero shape".into());
    }
    Ok(shape)
}

unsafe fn read_nonnegative_strides(
    array: &MLMultiArray,
    rank: usize,
) -> Result<Vec<usize>, String> {
    let strides: Vec<usize> = array
        .strides()
        .iter()
        .map(|stride| {
            usize::try_from(stride.integerValue())
                .map_err(|_| "aligned probe: negative stride".to_string())
        })
        .collect::<Result<_, _>>()?;
    if strides.len() != rank || strides.contains(&0) {
        return Err("aligned probe: invalid stride vector".into());
    }
    Ok(strides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_pads_only_innermost_dimension() {
        let layout = Layout::new(&[2, 3, 188], 2 * 3 * 188, 2).unwrap();
        assert_eq!(layout.strides, [576, 192, 1]);
        assert_eq!(layout.storage_elements, 1_152);
    }

    #[test]
    fn layout_rejects_empty_mismatch_and_overflow() {
        assert!(Layout::new(&[], 0, 4).is_err());
        assert!(Layout::new(&[0, 2], 0, 4).is_err());
        assert!(Layout::new(&[2, 3], 5, 4).is_err());
        assert!(Layout::new(&[usize::MAX / 2, 3], usize::MAX, 4).is_err());
    }

    unsafe fn assert_native_layout(
        array: &MLMultiArray,
        shape: &[usize],
        strides: &[usize],
        data_type: MLMultiArrayDataType,
    ) {
        assert_eq!(array.dataType(), data_type);
        let actual_shape: Vec<usize> = array
            .shape()
            .iter()
            .map(|dimension| dimension.integerValue() as usize)
            .collect();
        let actual_strides: Vec<usize> = array
            .strides()
            .iter()
            .map(|stride| stride.integerValue() as usize)
            .collect();
        assert_eq!(actual_shape, shape);
        assert_eq!(actual_strides, strides);
        assert_eq!(array.dataPointer().as_ptr() as usize % 64, 0);
    }

    #[test]
    fn native_f32_roundtrip_preserves_rows_and_padding() {
        let values = [0.25, -1.5, 2.75, 7.0, -8.0, 9.5];
        let array = aligned_f32(&[2, 3], &values).unwrap();
        unsafe {
            assert_native_layout(&array, &[2, 3], &[16, 1], MLMultiArrayDataType::Float32);
            let ptr = array.dataPointer().as_ptr().cast::<f32>();
            for row in 0..2 {
                for column in 0..3 {
                    assert_eq!(*ptr.add(row * 16 + column), values[row * 3 + column]);
                }
            }
        }
    }

    #[test]
    fn native_f16_roundtrip_converts_each_row() {
        let values = [0.25, -1.5, 2.75, 7.0, -8.0, 9.5];
        let array = aligned_f16(&[2, 3], &values).unwrap();
        unsafe {
            assert_native_layout(&array, &[2, 3], &[16, 1], MLMultiArrayDataType::Float16);
            let ptr = array.dataPointer().as_ptr().cast::<f16>();
            for row in 0..2 {
                for column in 0..3 {
                    assert_eq!(
                        (*ptr.add(row * 16 + column)).to_bits(),
                        f16::from_f32(values[row * 3 + column]).to_bits()
                    );
                }
            }
        }
    }

    #[test]
    fn native_i32_roundtrip_preserves_rows_and_padding() {
        let values = [i32::MIN, -7, 0, 13, 42, i32::MAX];
        let array = aligned_i32(&[2, 3], &values).unwrap();
        unsafe {
            assert_native_layout(&array, &[2, 3], &[16, 1], MLMultiArrayDataType::Int32);
            let ptr = array.dataPointer().as_ptr().cast::<i32>();
            for row in 0..2 {
                for column in 0..3 {
                    assert_eq!(*ptr.add(row * 16 + column), values[row * 3 + column]);
                }
            }
        }
    }
}
