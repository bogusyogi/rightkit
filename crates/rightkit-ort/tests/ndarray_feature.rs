//! `ndarray` feature: the re-exported ndarray is the one ort's tensor APIs accept.
#![cfg(feature = "ndarray")]

use rightkit_ort::ndarray::Array2;
use rightkit_ort::ort::value::TensorRef;

// Type-level proof: TensorRef::from_array_view accepts the re-exported ndarray view.
fn as_tensor(a: &Array2<f32>) -> rightkit_ort::ort::Result<TensorRef<'_, f32>> {
    TensorRef::from_array_view(a.view())
}

#[test]
fn reexported_ndarray_is_orts_ndarray() {
    let array = Array2::<f32>::zeros((2, 3));
    let _ = as_tensor;
    assert_eq!(array.shape(), &[2, 3]);
}
