//! CoreML input allocation reuse for validated production fusion and probes.
//!
//! CoreML predictions are synchronous, so a retained input may be reused after
//! `Stage::predict*` returns. The cache is thread local to avoid sharing a
//! mutable `MLMultiArray` between concurrent predictions. Production enables
//! it only after fused manifest validation; evaluator probes may still opt in
//! through their environment.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use half::f16;
use objc2::rc::Retained;
use objc2_core_ml::{MLMultiArray, MLMultiArrayDataType};

use crate::coreml::{ml_f16, ml_i32};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct InputKey {
    semantic_name: String,
    shape: Vec<usize>,
}

enum CachedInput {
    F16(Retained<MLMultiArray>),
    I32(Retained<MLMultiArray>),
}

thread_local! {
    static INPUTS: RefCell<HashMap<InputKey, CachedInput>> = RefCell::new(HashMap::new());
    static EVALUATOR_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
}

thread_local! {
    static PRODUCTION_ENABLED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct ProductionScope {
    previous: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

/// Bind reuse to this decode, so loading another model cannot affect it.
pub(crate) fn production_scope(enabled: bool) -> ProductionScope {
    ProductionScope {
        previous: PRODUCTION_ENABLED.with(|value| value.replace(enabled)),
        _thread_bound: std::marker::PhantomData,
    }
}

impl Drop for ProductionScope {
    fn drop(&mut self) {
        PRODUCTION_ENABLED.with(|value| value.set(self.previous));
    }
}

pub(crate) fn set_evaluator_enabled(enabled: bool) {
    EVALUATOR_ENABLED.with(|value| value.set(Some(enabled)));
}

pub(crate) fn clear_evaluator_override() {
    EVALUATOR_ENABLED.with(|value| value.set(None));
}

/// Return true only for the explicit allocation-reuse probe opt-in.
pub(crate) fn enabled() -> bool {
    EVALUATOR_ENABLED.with(|override_value| {
        override_value.get().unwrap_or_else(|| {
            PRODUCTION_ENABLED.with(|value| value.get())
                || rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_INPUT_REUSE_PROBE").as_deref()
                    == Ok("1")
        })
    })
}

/// Build or reuse an f16 input, overwriting every element before return.
pub(crate) fn f16(
    semantic_name: &str,
    shape: &[usize],
    values: &[f32],
) -> Result<Retained<MLMultiArray>, String> {
    if !enabled() {
        return ml_f16(shape, values);
    }
    validate_values(semantic_name, shape, values.len())?;

    INPUTS.with(|inputs| {
        let mut inputs = inputs.borrow_mut();
        let key = InputKey {
            semantic_name: semantic_name.to_owned(),
            shape: shape.to_vec(),
        };
        let array = match inputs.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let array = ml_f16(shape, values)?;
                validate_array(
                    semantic_name,
                    shape,
                    &array,
                    MLMultiArrayDataType::Float16,
                )?;
                match entry.insert(CachedInput::F16(array)) {
                    CachedInput::F16(array) => array,
                    CachedInput::I32(_) => unreachable!("new f16 cache entry has wrong type"),
                }
            }
            std::collections::hash_map::Entry::Occupied(entry) => match entry.into_mut() {
                // Entries are private, immutable-shape allocations validated on creation.
                CachedInput::F16(array) => array,
                CachedInput::I32(_) => {
                    return Err(format!(
                        "CoreML input reuse type collision for {semantic_name} shape {shape:?}: expected f16, found i32"
                    ));
                }
            },
        };

        unsafe {
            // getBytesWithHandler is the follow-up; dataPointer access is kept unchanged for now.
            #[allow(deprecated)]
            let ptr = array.dataPointer().as_ptr() as *mut f16;
            for (idx, &value) in values.iter().enumerate() {
                *ptr.add(idx) = f16::from_f32(value);
            }
        }
        Ok((*array).clone())
    })
}

/// Build or reuse an i32 input, overwriting every element before return.
pub(crate) fn i32(
    semantic_name: &str,
    shape: &[usize],
    values: &[i32],
) -> Result<Retained<MLMultiArray>, String> {
    if !enabled() {
        return ml_i32(shape, values);
    }
    validate_values(semantic_name, shape, values.len())?;

    INPUTS.with(|inputs| {
        let mut inputs = inputs.borrow_mut();
        let key = InputKey {
            semantic_name: semantic_name.to_owned(),
            shape: shape.to_vec(),
        };
        let array = match inputs.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let array = ml_i32(shape, values)?;
                validate_array(
                    semantic_name,
                    shape,
                    &array,
                    MLMultiArrayDataType::Int32,
                )?;
                match entry.insert(CachedInput::I32(array)) {
                    CachedInput::I32(array) => array,
                    CachedInput::F16(_) => unreachable!("new i32 cache entry has wrong type"),
                }
            }
            std::collections::hash_map::Entry::Occupied(entry) => match entry.into_mut() {
                // Entries are private, immutable-shape allocations validated on creation.
                CachedInput::I32(array) => array,
                CachedInput::F16(_) => {
                    return Err(format!(
                        "CoreML input reuse type collision for {semantic_name} shape {shape:?}: expected i32, found f16"
                    ));
                }
            },
        };

        unsafe {
            // getBytesWithHandler is the follow-up; dataPointer access is kept unchanged for now.
            #[allow(deprecated)]
            let ptr = array.dataPointer().as_ptr() as *mut i32;
            for (idx, &value) in values.iter().enumerate() {
                *ptr.add(idx) = value;
            }
        }
        Ok((*array).clone())
    })
}

fn validate_values(semantic_name: &str, shape: &[usize], value_count: usize) -> Result<(), String> {
    let expected = shape
        .iter()
        .try_fold(1usize, |count, &dim| count.checked_mul(dim))
        .ok_or_else(|| format!("CoreML input {semantic_name} shape {shape:?} overflows"))?;
    if expected != value_count {
        return Err(format!(
            "CoreML input {semantic_name} shape {shape:?} expects {expected} values, got {value_count}"
        ));
    }
    Ok(())
}

fn validate_array(
    semantic_name: &str,
    shape: &[usize],
    array: &MLMultiArray,
    expected_type: MLMultiArrayDataType,
) -> Result<(), String> {
    let actual_shape: Vec<usize> = unsafe { array.shape() }
        .iter()
        .map(|dimension| dimension.integerValue() as usize)
        .collect();
    if actual_shape != shape {
        return Err(format!(
            "CoreML input reuse shape mismatch for {semantic_name}: expected {shape:?}, got {actual_shape:?}"
        ));
    }
    let actual_type = unsafe { array.dataType() };
    if actual_type != expected_type {
        let expected_label = if expected_type == MLMultiArrayDataType::Float16 {
            "f16"
        } else {
            "i32"
        };
        return Err(format!(
            "CoreML input reuse type mismatch for {semantic_name} shape {shape:?}: expected {expected_label}"
        ));
    }

    // Native pointer fills below use linear offsets; reject any non-contiguous
    // backing rather than risking writes into a padded or aliased layout.
    let strides: Vec<usize> = unsafe { array.strides() }
        .iter()
        .map(|stride| stride.integerValue() as usize)
        .collect();
    let mut expected_stride = 1usize;
    for axis in (0..shape.len()).rev() {
        if strides.get(axis).copied() != Some(expected_stride) {
            return Err(format!(
                "CoreML input reuse layout mismatch for {semantic_name} shape {shape:?}: expected contiguous strides, got {strides:?}"
            ));
        }
        expected_stride = expected_stride
            .checked_mul(shape[axis])
            .ok_or_else(|| format!("CoreML input {semantic_name} shape {shape:?} overflows"))?;
    }
    Ok(())
}
