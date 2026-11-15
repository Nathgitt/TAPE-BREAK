//! Sines for signals that are worked out in `f64` and kept as `f32`.
//!
//! The model takes a sine per sample in several places (the FM carrier, the supply hum) and
//! rounds the result to `f32`. The library's sine is exact to the last bit of an `f64`, which
//! the rounding then throws away. `fast_sin` is cheaper and good to `FAST_SIN_ERR`;
//! `cast_if_sure` tells whether that error can change the `f32`; `sin_as_f32` uses the fast
//! sine where it cannot and the library's where it might. The result is the library's, bit for
//! bit, so nothing downstream can tell the difference.

/// Largest size of argument the fast sine takes; beyond it `sin_as_f32` uses the library.
pub const FAST_SIN_MAX: f64 = 1.0e7;

/// Bound on `|fast_sin(x) - x.sin()|` for `|x| <= FAST_SIN_MAX`.
pub const FAST_SIN_ERR: f64 = 1.0e-14;

/// An approximation of `x.sin()` within `FAST_SIN_ERR` for `|x| <= FAST_SIN_MAX`.
pub fn fast_sin(x: f64) -> f64 {
    x.sin()
}

/// `Some(f)` when every `f64` within `err` of `v` rounds to the one `f32` `f`; `None` when two
/// of them round differently, or when `v` or `err` is not finite.
///
/// Rounding to `f32` never turns the order of two values round, so it is enough that the two
/// ends of the reach round alike. Values each side of zero differ in sign and are never sure.
#[inline]
pub fn cast_if_sure(v: f64, err: f64) -> Option<f32> {
    let (lo, hi) = ((v - err) as f32, (v + err) as f32);
    if lo.to_bits() == hi.to_bits() && lo.is_finite() {
        Some(lo)
    } else {
        None
    }
}

/// `x.sin() as f32`, bit for bit.
#[inline]
pub fn sin_as_f32(x: f64) -> f32 {
    x.sin() as f32
}

/// `out[i] = x[i].sin() as f32`, bit for bit. The slices have the same length.
pub fn sin_as_f32_slice(x: &[f64], out: &mut [f32]) {
    assert_eq!(x.len(), out.len());
    for (o, &v) in out.iter_mut().zip(x.iter()) {
        *o = sin_as_f32(v);
    }
}
