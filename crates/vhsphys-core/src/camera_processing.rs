//! Camera electronics on the line signal: head amplifier (shot, feedback and rising FET noise,
//! delay-compensated analog Butterworth band) and processing amplifier (clamp, gain, knee, gamma,
//! aperture correction from delay lines with coring, clip).
//!
//! Sources: `docs/design/CAMERA_SCAN.md` ("Camera electronics"), `docs/CAMERA_RESEARCH.md` ("Head-
//! amplifier noise and processing lie mostly above 2.5 MHz"), US 4032974 (channel order),
//! US 3743766 (head-amplifier band), US 4470067 (knee), US 4355335 and US 4704635 (triangular
//! FET noise), BBC Engineering 91 and BBC Monograph 47 (horizontal/vertical aperture correction and
//! coring), EBU Tech 3335 (0.45 gamma law), and Panasonic WV-3110 / PK-958 (consumer camera figures).

use super::camera::TargetSignal;
use super::consts::{ACTIVE_H, FS_BB};
use crate::dsp::sample_cubic;
use crate::rng::Rng;
use crate::tube::TubeChannel;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Deterministic RNG stream for the camera head amplifier.
pub const STREAM_CAMERA: u64 = 61;
/// White and black clip levels relative to peak white (US 4032974; contract).
pub const WHITE_CLIP: f64 = 1.05;
pub const BLACK_CLIP: f64 = -0.03;

/// Elementary charge, C (SI exact).
const Q_E: f64 = 1.602_176_634e-19;
/// Boltzmann constant, J/K (SI exact).
const K_B: f64 = 1.380_649e-23;
/// Squared nanoamperes per squared ampere.
const NA2_PER_A2: f64 = 1.0e18;

/// Ambient head-amplifier temperature, K (`docs/design/CAMERA_SCAN.md`).
const TEMP_K: f64 = 300.0;
/// Head-amplifier -3 dB band, Hz (flat to about 5 MHz, US 3743766).
const BAND_HZ: f64 = 5.5e6;

/// Studio (three-tube broadcast camera) head-amplifier and processing figures.
/// Input capacitance, FET noise resistance and feedback resistance sit inside the published ranges
/// (15 to 30 pF, 50 to 100 ohm, 0.3 to 1 Mohm; `docs/CAMERA_RESEARCH.md`).
const STUDIO_INPUT_C_F: f64 = 20.0e-12;
const STUDIO_R_N_OHM: f64 = 75.0;
const STUDIO_R_F_OHM: f64 = 1.0e6;
/// Studio knee range: linear to 100% of rated light, 230% reaching white clip (US 4470067).
const STUDIO_KNEE_RANGE: f64 = 2.3;
/// Studio aperture correction: 150 ns delay (BBC Engineering 91), vertical `k_v = 0.5` (`g = 0.25`,
/// BBC Monograph 47), and coring threshold `[est]`.
const STUDIO_DELAY_S: f64 = 150.0e-9;
const STUDIO_VERTICAL: f64 = 0.5;
const STUDIO_CORING: f64 = 0.008;

/// Consumer (home camera) head-amplifier and processing figures: poor end of the ranges (~45 dB
/// at 300 nA, matching the Panasonic PK-958 "> 45 dB" specification).
const CONSUMER_INPUT_C_F: f64 = 30.0e-12;
const CONSUMER_R_N_OHM: f64 = 100.0;
const CONSUMER_R_F_OHM: f64 = 470.0e3;
/// Consumer aperture correction: 0.2 us delay line (Panasonic), no vertical 1H corrector, coring `[est]`.
const CONSUMER_DELAY_S: f64 = 200.0e-9;
const CONSUMER_VERTICAL: f64 = 0.0;
const CONSUMER_CORING: f64 = 0.015;

/// Gamma corrector break point in linear light and its output level (EBU Tech 3335 / BT.709).
const GAMMA_BREAK_L: f64 = 0.018;
const GAMMA_BREAK_V: f64 = 0.081;

/// Half-width of the Blackman-windowed ideal FIR differentiator used for the FET `C_in dv/dt`
/// noise term, in baseband samples (tap `DIFF_HALF` is zero at the window edge).
const DIFF_HALF: usize = 20;

/// Symmetric linear-phase FIR parameters for the delay-compensated analog second-order
/// Butterworth head-amplifier band: frequency-sampled on a 4096-point grid, 21 centred taps,
/// Kaiser window beta 6, normalised to DC gain 1.
const BAND_FIR_TAPS: usize = 21;
const BAND_FIR_GRID: usize = 4096;
const BAND_FIR_BETA: f64 = 6.0;

/// Camera electronics profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Processing {
    #[default]
    Studio,
    Consumer,
}

/// Head-amplifier electrical parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadAmpSpec {
    pub input_c_f: f64,
    pub r_n_ohm: f64,
    pub r_f_ohm: f64,
    pub temp_k: f64,
    pub band_hz: f64,
}

/// Aperture-corrector parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApertureSpec {
    pub delay_s: f64,
    pub vertical: f64,
    pub coring: f64,
}

/// Complete specification of a camera electronics profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessingSpec {
    pub head_amp: HeadAmpSpec,
    pub knee_range: Option<f64>,
    pub aperture: ApertureSpec,
}

impl Processing {
    /// Return the electrical parameters for this camera profile.
    pub fn spec(self) -> ProcessingSpec {
        match self {
            Processing::Studio => ProcessingSpec {
                head_amp: HeadAmpSpec {
                    input_c_f: STUDIO_INPUT_C_F,
                    r_n_ohm: STUDIO_R_N_OHM,
                    r_f_ohm: STUDIO_R_F_OHM,
                    temp_k: TEMP_K,
                    band_hz: BAND_HZ,
                },
                knee_range: Some(STUDIO_KNEE_RANGE),
                aperture: ApertureSpec {
                    delay_s: STUDIO_DELAY_S,
                    vertical: STUDIO_VERTICAL,
                    coring: STUDIO_CORING,
                },
            },
            Processing::Consumer => ProcessingSpec {
                head_amp: HeadAmpSpec {
                    input_c_f: CONSUMER_INPUT_C_F,
                    r_n_ohm: CONSUMER_R_N_OHM,
                    r_f_ohm: CONSUMER_R_F_OHM,
                    temp_k: TEMP_K,
                    band_hz: BAND_HZ,
                },
                knee_range: None,
                aperture: ApertureSpec {
                    delay_s: CONSUMER_DELAY_S,
                    vertical: CONSUMER_VERTICAL,
                    coring: CONSUMER_CORING,
                },
            },
        }
    }
}

/// Unclamped 0.45-law camera gamma corrector (equal to `tube::oetf` on `[0, 1]`).
pub fn gamma_correct(l: f64) -> f64 {
    if l < GAMMA_BREAK_L {
        4.5 * l
    } else {
        1.099 * l.powf(0.45) - 0.099
    }
}

/// Inverse of `gamma_correct`, unclamped.
pub fn gamma_inverse(v: f64) -> f64 {
    if v < GAMMA_BREAK_V {
        v / 4.5
    } else {
        ((v + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// Magnitude of the analog second-order Butterworth low-pass at `f` Hz.
#[inline]
fn analog_butter2_mag(band_hz: f64, f: f64) -> f64 {
    1.0 / (1.0 + (f / band_hz).powi(4)).sqrt()
}

/// Zeroth-order modified Bessel function of the first kind `I_0(x)`.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// `k_h` of the restoration rule: restores the green channel's horizontal spot and head-amplifier
/// response at `f_p = 1 / (2 T)`.
pub fn horizontal_gain(delay_s: f64, sigma_h_s: f64, band_hz: f64) -> f64 {
    let f_p = 1.0 / (2.0 * delay_s);
    let spot = (-2.0 * PI * PI * sigma_h_s * sigma_h_s * f_p * f_p).exp();
    let amp = analog_butter2_mag(band_hz, f_p);
    (1.0 / (spot * amp) - 1.0) / 2.0
}

/// Gamma-corrected video of one frame: per active row, per channel, per sample; 1.0 = peak white.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraVideo {
    pub rows: Vec<[Vec<f32>; 3]>,
}

/// Camera head amplifier: adds signal shot noise, feedback-resistor thermal noise and rising
/// FET input-capacitance noise, then applies the delay-compensated analog second-order Butterworth
/// low-pass band as a symmetric linear-phase FIR.
#[derive(Clone)]
pub struct HeadAmplifier {
    seed: u64,
    s_fb_na2_hz: f64,
    sigma_u_nc: f64,
    band_taps: [f64; BAND_FIR_TAPS],
    diff_taps: [f64; DIFF_HALF],
}

impl HeadAmplifier {
    /// Build a head amplifier from `spec` and a deterministic `seed`.
    pub fn new(spec: &HeadAmpSpec, seed: u64) -> Self {
        let s_fb_na2_hz = (4.0 * K_B * spec.temp_k / spec.r_f_ohm) * NA2_PER_A2;
        let s_u_nc2_hz =
            (4.0 * K_B * spec.temp_k * spec.r_n_ohm * spec.input_c_f * spec.input_c_f) * NA2_PER_A2;
        let sigma_u_nc = (s_u_nc2_hz * 0.5 * FS_BB).sqrt();

        // Frequency-sample |H_a(f)| on a 4096-point grid up to FS_BB/2, inverse real DFT,
        // take the centred N = 21 taps, apply Kaiser window beta 6, and normalise to DC gain 1.
        let half_grid = BAND_FIR_GRID / 2;
        let h_mag: Vec<f64> = (0..=half_grid)
            .map(|k| analog_butter2_mag(spec.band_hz, k as f64 * FS_BB / BAND_FIR_GRID as f64))
            .collect();
        let half_taps = BAND_FIR_TAPS / 2;
        let i0_beta = bessel_i0(BAND_FIR_BETA);
        let mut band_taps = [0.0; BAND_FIR_TAPS];
        for (i, tap) in band_taps.iter_mut().enumerate() {
            let m = i as isize - half_taps as isize;
            let nyq_sign = if m & 1 == 0 { 1.0 } else { -1.0 };
            let mut sum = h_mag[0] + nyq_sign * h_mag[half_grid];
            let step = 2.0 * PI * m as f64 / BAND_FIR_GRID as f64;
            for k in 1..half_grid {
                sum += 2.0 * h_mag[k] * (step * k as f64).cos();
            }
            let h_ideal = sum / BAND_FIR_GRID as f64;
            let r = m as f64 / half_taps as f64;
            let w = bessel_i0(BAND_FIR_BETA * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
            *tap = h_ideal * w;
        }
        let dc_gain: f64 = band_taps.iter().sum();
        for tap in band_taps.iter_mut() {
            *tap /= dc_gain;
        }

        let diff_taps = std::array::from_fn(|idx| {
            let k = idx + 1;
            let a = PI * k as f64 / DIFF_HALF as f64;
            let w = 0.42 + 0.5 * a.cos() + 0.08 * (2.0 * a).cos();
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            FS_BB * sign / k as f64 * w
        });
        Self { seed, s_fb_na2_hz, sigma_u_nc, band_taps, diff_taps }
    }

    /// Noise and band on every row of `sig` (nA in, nA out), for frame `n`.
    pub fn amplify(&self, sig: &mut TargetSignal, n: i64) {
        let row_stride = (ACTIVE_H * 3) as u64;
        sig.rows.par_iter_mut().enumerate().for_each(|(r, chans)| {
            for (ch, row) in chans.iter_mut().enumerate() {
                if row.is_empty() {
                    continue;
                }
                let index = (n as u64)
                    .wrapping_mul(row_stride)
                    .wrapping_add((r * 3 + ch) as u64) as i64;
                let mut rng = Rng::new(self.seed, STREAM_CAMERA, index);
                // Generate padded white charge noise u = C_in * v so every sample has a full
                // symmetric differentiator neighbourhood without boundary transients.
                let len = row.len();
                let u: Vec<f64> = (0..len + 2 * DIFF_HALF)
                    .map(|_| rng.gauss() as f64 * self.sigma_u_nc)
                    .collect();
                let mut noisy = Vec::with_capacity(len);
                for (x, &sample) in row.iter().enumerate() {
                    let i_na = (sample as f64).max(0.0);
                    let s_shot = 2.0 * Q_E * i_na * 1.0e-9 * NA2_PER_A2;
                    let sigma_white = ((s_shot + self.s_fb_na2_hz) * 0.5 * FS_BB).sqrt();
                    let i_white = rng.gauss() as f64 * sigma_white;
                    let c = DIFF_HALF + x;
                    let mut i_fet = 0.0;
                    for k in 1..=DIFF_HALF {
                        i_fet += self.diff_taps[k - 1] * (u[c - k] - u[c + k]);
                    }
                    noisy.push(sample as f64 + i_white + i_fet);
                }
                let half = BAND_FIR_TAPS / 2;
                let last = len - 1;
                for x in 0..len {
                    let acc = if x >= half && x + half < len {
                        noisy[x - half..=x + half]
                            .iter()
                            .zip(self.band_taps.iter())
                            .map(|(a, b)| a * b)
                            .sum()
                    } else {
                        let mut s = 0.0;
                        for (k, &tap) in self.band_taps.iter().enumerate() {
                            let j = (x as isize + k as isize - half as isize)
                                .clamp(0, last as isize) as usize;
                            s += tap * noisy[j];
                        }
                        s
                    };
                    row[x] = acc as f32;
                }
            }
        });
    }
}

/// Camera processing amplifier: clamp, gain, linearise, knee, gamma, aperture correction with coring, clip.
#[derive(Clone)]
pub struct VideoProcessor {
    spec: ProcessingSpec,
    peak_na: [f64; 3],
    black_na: [f64; 3],
    inv_gamma: [Option<f64>; 3],
    knee_slope: Option<f64>,
    k_h: f64,
}

impl VideoProcessor {
    /// `black` is the clamp level per channel, nA; `k_h` from the green channel's spot.
    pub fn new(processing: Processing, channels: &[TubeChannel; 3], black: [f64; 3]) -> Self {
        let spec = processing.spec();
        let peak_na = std::array::from_fn(|k| channels[k].peak_signal_na);
        let inv_gamma = std::array::from_fn(|k| channels[k].linearise.then(|| 1.0 / channels[k].gamma));
        let knee_slope = spec.knee_range.map(|r| (gamma_inverse(WHITE_CLIP) - 1.0) / (r - 1.0));
        let k_h = horizontal_gain(spec.aperture.delay_s, channels[1].sigma_h_s, spec.head_amp.band_hz);
        Self { spec, peak_na, black_na: black, inv_gamma, knee_slope, k_h }
    }

    /// Horizontal aperture gain `k_h` for this processor.
    pub fn horizontal_gain(&self) -> f64 {
        self.k_h
    }

    /// Clamp, gain, linearise, knee, gamma, aperture correction, clip.
    pub fn process(&self, sig: &TargetSignal) -> CameraVideo {
        let gamma_rows: Vec<[Vec<f32>; 3]> = sig
            .rows
            .par_iter()
            .map(|row| {
                std::array::from_fn(|k| {
                    let black = self.black_na[k];
                    let peak = self.peak_na[k];
                    let inv_gamma = self.inv_gamma[k];
                    row[k]
                        .iter()
                        .map(|&i_na| {
                            let mut l = (i_na as f64 - black) / peak;
                            if let Some(exp) = inv_gamma {
                                l = if l == 0.0 { 0.0 } else { l.abs().powf(exp).copysign(l) };
                            }
                            if let Some(s) = self.knee_slope {
                                if l > 1.0 {
                                    l = 1.0 + (l - 1.0) * s;
                                }
                            }
                            gamma_correct(l) as f32
                        })
                        .collect()
                })
            })
            .collect();

        let n_rows = gamma_rows.len();
        let d_samples = self.spec.aperture.delay_s * FS_BB;
        let k_v = self.spec.aperture.vertical;
        let coring = self.spec.aperture.coring;

        let rows: Vec<[Vec<f32>; 3]> = (0..n_rows)
            .into_par_iter()
            .map(|r| {
                let g_row = &gamma_rows[r][1];
                let g_up = if r >= 2 { &gamma_rows[r - 2][1] } else { g_row };
                let g_dn = if r + 2 < n_rows { &gamma_rows[r + 2][1] } else { g_row };
                let w = g_row.len();
                let mut out: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0f32; w]);
                for x in 0..w {
                    let s_minus = sample_cubic(g_row, x as f64 - d_samples) as f64;
                    let s_plus = sample_cubic(g_row, x as f64 + d_samples) as f64;
                    let g_c = g_row[x] as f64;
                    let d_h = g_c - 0.5 * (s_minus + s_plus);
                    let d_v = g_c - 0.5 * (g_up[x] as f64 + g_dn[x] as f64);
                    let detail = self.k_h * core(d_h, coring) + k_v * core(d_v, coring);
                    for k in 0..3 {
                        out[k][x] = (gamma_rows[r][k][x] as f64 + detail)
                            .clamp(BLACK_CLIP, WHITE_CLIP) as f32;
                    }
                }
                out
            })
            .collect();

        CameraVideo { rows }
    }
}

#[inline]
fn core(d: f64, c: f64) -> f64 {
    let a = d.abs();
    if a <= c {
        0.0
    } else {
        d.signum() * (a - c)
    }
}
