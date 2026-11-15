//! A sine that is quicker than the library's and gives the same `f32`, bit for bit.
//!
//! The model takes many sines in `f64` and keeps them as `f32` (the FM carrier, the supply
//! hum). `trig::fast_sin` is an approximation with a stated error bound; `trig::cast_if_sure`
//! says whether every value within a bound of an `f64` rounds to the same `f32`; and
//! `trig::sin_as_f32` puts the two together and falls back on the library's sine where the
//! cast is not sure. These tests hold `sin_as_f32(x)` to `x.sin() as f32` exactly.

use vhsphys_core::trig::{cast_if_sure, fast_sin, sin_as_f32, sin_as_f32_slice, FAST_SIN_ERR, FAST_SIN_MAX};

/// SplitMix64: a plain generator for the test points.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

fn same_bits(x: f64) {
    let (got, want) = (sin_as_f32(x), x.sin() as f32);
    assert!(got.to_bits() == want.to_bits(), "sin_as_f32({x:e}) = {got:e} ({:#010x}), the library gives {want:e} ({:#010x})", got.to_bits(), want.to_bits());
}

#[test]
fn the_bounds_are_stated_sensibly() {
    // large enough for hours of mains hum at its third harmonic, small enough to be useful
    assert!(FAST_SIN_MAX >= 1.0e7, "{FAST_SIN_MAX}");
    assert!(FAST_SIN_ERR > 0.0 && FAST_SIN_ERR <= 1.0e-13, "{FAST_SIN_ERR}");
}

#[test]
fn the_carrier_phase_range_gives_the_librarys_bits() {
    // the FM modulator keeps its phase in (-pi, pi] and a little beyond
    let mut r = Mix(1);
    for _ in 0..30_000_000 {
        same_bits(r.range(-3.3, 3.3));
    }
}

#[test]
fn large_and_small_arguments_give_the_librarys_bits() {
    let mut r = Mix(2);
    // the whole fast range and beyond it, where the function must fall back
    for _ in 0..8_000_000 {
        same_bits(r.range(-1.5 * FAST_SIN_MAX, 1.5 * FAST_SIN_MAX));
    }
    // every size of argument, from the smallest subnormal to far beyond the fast range
    for _ in 0..8_000_000 {
        let e = r.range(-1074.0, 80.0);
        let x = 2f64.powf(e) * if r.next() & 1 == 0 { 1.0 } else { -1.0 };
        same_bits(x);
    }
}

#[test]
fn arguments_near_the_multiples_of_a_quarter_turn_give_the_librarys_bits() {
    // where the reduction cancels most and the sine is nearest 0 or +/-1
    let mut r = Mix(3);
    for _ in 0..6_000_000 {
        let k = (r.next() % 40_000_000) as f64 - 20_000_000.0;
        let near = k * std::f64::consts::FRAC_PI_2;
        let d = 2f64.powf(r.range(-60.0, -2.0)) * if r.next() & 1 == 0 { 1.0 } else { -1.0 };
        same_bits(near + d);
        same_bits(near);
    }
}

#[test]
fn special_values_give_the_librarys_bits() {
    for x in [
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        5e-324,
        -5e-324,
        1.0,
        -1.0,
        std::f64::consts::PI,
        -std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::FRAC_PI_4,
        FAST_SIN_MAX,
        -FAST_SIN_MAX,
        FAST_SIN_MAX * (1.0 + f64::EPSILON),
        f64::MAX,
        f64::MIN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        same_bits(x);
    }
}

#[test]
fn the_fast_sine_is_well_inside_its_stated_error() {
    // The bound has to hold for arguments not tried here, so the worst error seen over many
    // points must leave a wide margin: a quarter of the bound.
    let mut r = Mix(4);
    let mut worst = 0.0f64;
    let mut at = 0.0f64;
    let mut see = |x: f64| {
        let e = (fast_sin(x) - x.sin()).abs();
        if e > worst {
            worst = e;
            at = x;
        }
    };
    for _ in 0..20_000_000 {
        see(r.range(-3.3, 3.3));
    }
    for _ in 0..10_000_000 {
        see(r.range(-FAST_SIN_MAX, FAST_SIN_MAX));
    }
    for _ in 0..4_000_000 {
        let k = (r.next() % 40_000_000) as f64 - 20_000_000.0;
        let x = k * std::f64::consts::FRAC_PI_2 + 2f64.powf(r.range(-60.0, -2.0));
        if x.abs() <= FAST_SIN_MAX {
            see(x);
        }
    }
    assert!(worst <= FAST_SIN_ERR / 4.0, "worst error {worst:e} at {at:e}; the stated bound is {FAST_SIN_ERR:e}");
}

#[test]
fn a_cast_is_sure_only_when_every_value_in_reach_rounds_alike() {
    let mut r = Mix(5);
    let (mut sure, mut unsure) = (0u64, 0u64);
    for _ in 0..4_000_000 {
        let v = match r.next() % 4 {
            0 => r.range(-1.0, 1.0),
            1 => r.range(-200.0, 200.0),
            2 => 2f64.powf(r.range(-140.0, 120.0)) * if r.next() & 1 == 0 { 1.0 } else { -1.0 },
            // close to a value an f32 holds, where the cast is least sure
            _ => (r.range(-4.0, 4.0) as f32) as f64 * (1.0 + r.range(-1.0e-7, 1.0e-7)),
        };
        let err = 2f64.powf(r.range(-70.0, -20.0));
        match cast_if_sure(v, err) {
            Some(f) => {
                sure += 1;
                // every value within `err` of `v` must round to `f`
                for w in [v, v - err, v + err, v - 0.5 * err, v + 0.5 * err, v - err * r.unit(), v + err * r.unit()] {
                    assert!((w as f32).to_bits() == f.to_bits(), "cast_if_sure({v:e}, {err:e}) = {f:e}, yet {w:e} rounds to {:e}", w as f32);
                }
            }
            None => unsure += 1,
        }
    }
    // a useful test of sureness says yes most of the time and no sometimes
    assert!(sure > 2 * unsure, "sure {sure}, unsure {unsure}");
    assert!(unsure > 0, "the unsure case was never met");
    // what cannot be sure
    assert_eq!(cast_if_sure(f64::NAN, 1e-15), None);
    assert_eq!(cast_if_sure(1.0, f64::NAN), None);
    assert_eq!(cast_if_sure(f64::INFINITY, 1e-15), None);
    assert_eq!(cast_if_sure(0.0, 1e-15), None, "values each side of zero differ in sign");
    assert_eq!(cast_if_sure(0.5, 0.0), Some(0.5));
}

#[test]
fn the_slice_form_is_the_scalar_form() {
    let mut r = Mix(6);
    for len in [0usize, 1, 2, 3, 7, 8, 9, 31, 1000, 4097] {
        let x: Vec<f64> = (0..len)
            .map(|i| match i % 5 {
                0 => r.range(-3.3, 3.3),
                1 => r.range(-FAST_SIN_MAX, FAST_SIN_MAX),
                2 => 0.0,
                3 => f64::NAN,
                _ => r.range(-2.0, 2.0) * FAST_SIN_MAX,
            })
            .collect();
        let mut out = vec![7.0f32; len];
        sin_as_f32_slice(&x, &mut out);
        for (i, (&xi, &o)) in x.iter().zip(out.iter()).enumerate() {
            assert!(o.to_bits() == (xi.sin() as f32).to_bits(), "len {len}, element {i}: {o:e} for sin({xi:e})");
        }
    }
}
