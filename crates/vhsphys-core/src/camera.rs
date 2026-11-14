//! The camera stage: the video that goes into the recorder is what a three-tube Plumbicon
//! camera made of the scene, with the lag, comet tails and finite beam aperture of its pickup
//! tubes (`tube`) followed by its head amplifier and processing amplifier (`camera_processing`).
//!
//! Each channel's target is simulated on a sub-row grid (`SUB_ROWS = 4` per active line)
//! charged continuously in time by the held input pictures and read line by line, field by
//! field, at each element's own read time (`read_time_s`). The beam has a vertical Gaussian
//! profile across sub-rows and a horizontal Gaussian spot along the line signal, producing
//! the continuous signal current in nA. That line signal then passes through the head
//! amplifier (shot, feedback and rising FET noise, Butterworth band) and processing amplifier
//! (clamp, gain, knee, gamma, aperture correction from delay lines with coring, clip) before
//! baseband encoding.
//!
//! The stage is off by default and then costs nothing: the session does not even build it.

use super::camera_processing::{CameraVideo, HeadAmplifier, Processing, VideoProcessor};
use super::consts::{ACTIVE_H, ACTIVE_START, ACTIVE_W, FH, FRAME_RATE, FS_BB};
use super::stripe::{optical_lowpass, target_light, StripeDecoder, STRIPE_WHITE_LIGHT};
use super::video::{encode_from_rgb, row_to_line, Baseband, Frame};
use crate::spectral::{colour_matrix, taking_matrix};
use crate::tube::{
    oetf_inverse, photo_current, scan, Dbc, TubeChannel, TubeKind, ACT_BEAM_NA, ACT_FLYBACK_S,
    ACT_SPAN_PH, BEAM_RATIO_DEFAULT, BEND_RADIUS_MAX_LINES, DBC_TOLERANCE_NA, MAX_TRAPS,
    TARGET_ASPECT, V_BEND_ONSET_V,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Sub-rows per active frame row on each target, so the beam's vertical profile overlaps the
/// other field's lines.
pub const SUB_ROWS: usize = 4;

/// Dark scanning time before frame 0; exceeds 30 times the slowest dark settling constant.
const DARK_SETTLE_S: f64 = 10.0;
const IDENTITY_MATRIX: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Signal currents read from the targets in one frame: per active frame row, per channel, one
/// value per baseband sample of the active line, nA (before the head amplifier).
#[derive(Clone, Debug, PartialEq)]
pub struct TargetSignal {
    pub rows: Vec<[Vec<f32>; 3]>,
}

/// Charge offered to each element of the ACT flyback span per flyback, nA s:
/// `ACT_BEAM_NA * ACT_FLYBACK_S / ACT_SPAN_PH`.
pub fn act_flyback_nas() -> f64 {
    ACT_BEAM_NA * ACT_FLYBACK_S / ACT_SPAN_PH
}

/// Horizontal sample pitch in frame-line pitches: `TARGET_ASPECT * ACTIVE_H / ACTIVE_W`.
pub fn sample_pitch_lines() -> f64 {
    TARGET_ASPECT * ACTIVE_H as f64 / ACTIVE_W as f64
}

/// Seconds from the session start at which the beam centre reads sample `x` of active row `r`
/// in frame `n`.
pub fn read_time_s(n: i64, r: usize, x: usize) -> f64 {
    let (line, _) = row_to_line(r);
    n as f64 / FRAME_RATE + line as f64 / FH + (ACTIVE_START + x) as f64 / FS_BB
}

/// Seconds from the session start at which the ACT flyback runs before pass `r` in frame `n`.
fn flyback_time_s(n: i64, r: usize) -> f64 {
    let (line, _) = row_to_line(r);
    n as f64 / FRAME_RATE + line as f64 / FH
}

/// How the camera's tubes see colour (config `layout`: `three-tube`, `stripe`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    #[default]
    ThreeTube,
    Stripe,
}

/// The slot of `TargetSignal` that carries the stripe camera's one target.
pub const STRIPE_SLOT: usize = 1;
/// Frames of white card read before the alignment measures it.
pub const ALIGN_FRAMES: usize = 8;
/// Head-amplifier frame indices of the alignment's white card and capped lens.
pub const ALIGN_WHITE_FRAME: i64 = -1;
pub const ALIGN_CAPPED_FRAME: i64 = -2;

/// The stripe camera's alignment: the decoder's carrier gain, and per channel `[R, G, B]` the decoded
/// level of a capped lens (`floor`) and of a white card above it (`white`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripeAlignment {
    pub carrier_gain: f64,
    pub floor: [f64; 3],
    pub white: [f64; 3],
}

/// The camera: which pickup tubes it has and how it is set up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraParams {
    /// The pickup tube (`off`, `plumbicon`, `plumbicon-bias`, `plumbicon-act`, `plumbicon-dbc`,
    /// `vidicon`, `newvicon`, `saticon`, `chalnicon`).
    pub tube: TubeKind,
    /// How the camera's tubes see colour (`three-tube`, `stripe`).
    pub layout: Layout,
    /// Exposure of the scene on the tube targets, in stops (powers of two of the light).
    /// Positive values overexpose: highlights then carry more than the beam can take off.
    pub iris_stops: f64,
    /// Beam current as a multiple of what peak white needs (the data sheet sets 2.0).
    pub beam_ratio: f64,
    /// Beam-bending strength: how strongly an element above the 10 V onset bends the
    /// approaching beam towards itself (`0.0` = off, the default; negative values count as `0`).
    pub blooming: f64,
    /// Camera electronics profile (`studio`, `consumer`).
    pub processing: Processing,
    /// Target temperature in degrees Celsius (default 25.0).
    pub temperature_c: f64,
    /// Spectral taking through the prism, IR-cut and material responses, with the processing
    /// amplifier's colour matrix when the profile carries one (default `false`: ideal taking).
    pub spectral: bool,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self {
            tube: TubeKind::Off,
            layout: Layout::default(),
            iris_stops: 0.0,
            beam_ratio: BEAM_RATIO_DEFAULT,
            blooming: 0.0,
            processing: Processing::default(),
            temperature_c: 25.0,
            spectral: false,
        }
    }
}

/// Precomputed vertical Gaussian beam shares `g(y_k - c_r) / S_k` and ACT flyback span for
/// one active row `r`.
#[derive(Clone)]
struct RowWeights {
    k_start: usize,
    shares: Vec<f64>,
    fb_range: std::ops::Range<usize>,
}

/// Sub-row range `|y_k - c_r| <= ACT_SPAN_PH * ACTIVE_H / 2` covered by row `r`'s ACT flyback.
fn act_flyback_range(r: usize) -> std::ops::Range<usize> {
    let n_sub = SUB_ROWS * ACTIVE_H;
    let c_r = r as f64 + 0.5;
    let h_act = ACT_SPAN_PH * ACTIVE_H as f64 / 2.0;
    let lo = ((c_r - h_act) * SUB_ROWS as f64 - 0.5).floor().max(0.0) as usize;
    let hi =
        (((c_r + h_act) * SUB_ROWS as f64 - 0.5).ceil() as isize).min((n_sub - 1) as isize);
    if hi < lo as isize {
        return 0..0;
    }
    let mut start = None;
    let mut end = 0;
    for k in lo..=hi as usize {
        let y_k = (k as f64 + 0.5) / SUB_ROWS as f64;
        if (y_k - c_r).abs() <= h_act {
            if start.is_none() {
                start = Some(k);
            }
            end = k + 1;
        }
    }
    match start {
        Some(s) => s..end,
        None => 0..0,
    }
}

/// Build the vertical beam shares for every active row, truncated at `3 * sigma_v` and
/// normalised by `S_k` (the sum of `g` over every pass of a frame at sub-row `k`).
fn build_vertical_weights(sigma_v: f64) -> Vec<RowWeights> {
    let n_sub = SUB_ROWS * ACTIVE_H;
    let cutoff = 3.0 * sigma_v;
    let mut raw: Vec<(usize, Vec<f64>)> = Vec::with_capacity(ACTIVE_H);
    let mut s_k = vec![0.0; n_sub];
    for r in 0..ACTIVE_H {
        let c_r = r as f64 + 0.5;
        let k_min = (((c_r - cutoff) * SUB_ROWS as f64 - 0.5).ceil() as isize).max(0) as usize;
        let k_max =
            (((c_r + cutoff) * SUB_ROWS as f64 - 0.5).floor() as isize).min((n_sub - 1) as isize);
        if k_max < k_min as isize {
            raw.push((0, Vec::new()));
            continue;
        }
        let k_max = k_max as usize;
        let mut gs = Vec::with_capacity(k_max - k_min + 1);
        for k in k_min..=k_max {
            let y_k = (k as f64 + 0.5) / SUB_ROWS as f64;
            let d = (y_k - c_r) / sigma_v;
            let g = (-0.5 * d * d).exp();
            s_k[k] += g;
            gs.push(g);
        }
        raw.push((k_min, gs));
    }
    raw.into_iter()
        .enumerate()
        .map(|(r, (k_start, mut gs))| {
            for (j, g) in gs.iter_mut().enumerate() {
                *g /= s_k[k_start + j];
            }
            RowWeights { k_start, shares: gs, fb_range: act_flyback_range(r) }
        })
        .collect()
}

/// Precompute the horizontal Gaussian spot kernel in baseband sample steps.
fn build_horizontal_kernel(sigma_h_s: f64) -> (usize, Vec<f64>) {
    let sigma_samples = sigma_h_s * FS_BB;
    if sigma_samples <= 0.0 {
        return (0, vec![1.0]);
    }
    let radius = (4.0 * sigma_samples).ceil() as usize;
    let kernel = (-(radius as isize)..=(radius as isize))
        .map(|d| (-0.5 * (d as f64 / sigma_samples).powi(2)).exp())
        .collect();
    (radius, kernel)
}

/// Convolve one active row's signal with the horizontal spot kernel, renormalising at the line
/// ends inside `[0, raw.len())`.
fn convolve_horizontal(raw: &[f64], radius: usize, kernel: &[f64], out: &mut [f32]) {
    let w = raw.len();
    if radius == 0 || w == 0 {
        for (o, &v) in out.iter_mut().zip(raw.iter()) {
            *o = v as f32;
        }
        return;
    }
    let full_sum: f64 = kernel.iter().sum();
    let r_edge = radius.min(w);
    for x in 0..r_edge {
        out[x] = convolve_edge(raw, x, radius, kernel) as f32;
    }
    if w > 2 * radius {
        for x in radius..w - radius {
            let mut acc = 0.0;
            let src = &raw[x - radius..=x + radius];
            for (a, b) in src.iter().zip(kernel.iter()) {
                acc += a * b;
            }
            out[x] = (acc / full_sum) as f32;
        }
    }
    let right_start = r_edge.max(w.saturating_sub(radius));
    for x in right_start..w {
        out[x] = convolve_edge(raw, x, radius, kernel) as f32;
    }
}

fn convolve_edge(raw: &[f64], x: usize, radius: usize, kernel: &[f64]) -> f64 {
    let x0 = x.saturating_sub(radius);
    let x1 = (x + radius + 1).min(raw.len());
    let mut num = 0.0;
    let mut den = 0.0;
    for xi in x0..x1 {
        let w = kernel[xi + radius - x];
        num += w * raw[xi];
        den += w;
    }
    num / den
}

/// Integrate one sub-element `(x, k)` from its last-landed timestamp to `t_now`, updating
/// stored charge, timestamp and trap occupancies in place, and return the integrated charge
/// capped at `ch.plate_nas`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn integrate_sub_row<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    x: usize,
    k: usize,
    t_now: f64,
    q: &mut f64,
    t_prev_ref: &mut f64,
    mut trap_occ: Option<&mut [f64; MAX_TRAPS]>,
    photo: &F,
    field_s: f64,
) -> f64
where
    F: Fn(usize, usize, usize, f64) -> f64 + ?Sized,
{
    let t_prev = *t_prev_ref;
    if t_now <= t_prev {
        return *q;
    }
    let bias_na = ch.bias_na;
    let dark_plus_bias = ch.dark_na + bias_na;
    let plate_nas = ch.plate_nas;
    let mut t0 = t_prev;
    let mut m = (t0 / field_s).floor() as i64;
    let mut photo_nas = 0.0;
    let mut trap_dn = 0.0;
    while t0 < t_now {
        let t_end = ((m + 1) as f64 * field_s).min(t_now);
        let dt_piece = t_end - t0;
        if dt_piece > 0.0 {
            let t_mid = 0.5 * (t0 + t_end);
            let p_na = photo(ch_idx, k / SUB_ROWS, x, t_mid).max(0.0);
            photo_nas += p_na * dt_piece;
            if let Some(occ) = trap_occ.as_deref_mut() {
                let j_light = p_na + bias_na;
                for (p, trap) in ch.traps.iter().enumerate() {
                    if trap.capture > 0.0 {
                        let b = trap.capture * j_light / trap.capacity_nas + 1.0 / trap.release_s;
                        let n_inf = trap.capture * j_light / b;
                        let n_0 = occ[p];
                        let n_1 = n_inf + (n_0 - n_inf) * (-b * dt_piece).exp();
                        trap_dn += n_1 - n_0;
                        occ[p] = n_1;
                    }
                }
            }
        }
        t0 = t_end;
        m += 1;
    }
    let dt = t_now - t_prev;
    let before = (*q + photo_nas + dark_plus_bias * dt - trap_dn).min(plate_nas);
    *q = before;
    *t_prev_ref = t_now;
    before
}

/// Step 1 (ACT only): integrate elements in `fb_range` to `t_fb` and discharge those above
/// `q_act` with flyback beam `b_fb`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn act_flyback_step<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    x: usize,
    fb_range: std::ops::Range<usize>,
    t_fb: f64,
    q_col: &mut [f64],
    t_col: &mut [f64],
    traps_col: &mut [[f64; MAX_TRAPS]],
    photo: &F,
    field_s: f64,
    b_fb: f64,
    q_act: f64,
) where
    F: Fn(usize, usize, usize, f64) -> f64 + ?Sized,
{
    let has_traps = !traps_col.is_empty();
    let k_nas = ch.k_nas;
    for k in fb_range {
        let trap_occ = if has_traps { Some(&mut traps_col[k]) } else { None };
        let q = integrate_sub_row(
            ch_idx,
            ch,
            x,
            k,
            t_fb,
            &mut q_col[k],
            &mut t_col[k],
            trap_occ,
            photo,
            field_s,
        );
        if q > q_act {
            q_col[k] = q_act + scan(b_fb, k_nas, q - q_act);
        }
    }
}

/// Step 3 (DBC only): Ampex beam law per column by bisection on the band's integrated charges.
fn dbc_beam(
    ch: &TubeChannel,
    dbc: Dbc,
    q_band: &[f64],
    shares: &[f64],
    frame_s: f64,
) -> f64 {
    let k_nas = ch.k_nas;
    let denom = SUB_ROWS as f64 * frame_s;
    let head = dbc.headroom * ch.beam_na;
    let g = |beam_i: f64| -> f64 {
        let mut landed = 0.0;
        let b_frame = beam_i * frame_s;
        for (&q_k, &share) in q_band.iter().zip(shares.iter()) {
            landed += q_k - scan(b_frame * share, k_nas, q_k);
        }
        beam_i - head - landed / denom
    };
    let mut lo = ch.beam_na;
    if g(lo) >= 0.0 {
        return lo;
    }
    let mut hi = (dbc.max_ratio * ch.peak_signal_na).max(lo);
    if g(hi) <= 0.0 {
        return hi;
    }
    while hi - lo > DBC_TOLERANCE_NA {
        let mid = 0.5 * (lo + hi);
        if g(mid) >= 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Steps 1 to 3 for one column `x` on pass `r`: flyback, integration over `int_start..int_end`,
/// and column beam current `I_b(x)`. Returns `(i_b, has_hot)` where `has_hot` is true if any
/// integrated element exceeds `V_BEND_ONSET_V`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn prepare_column<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    rw: &RowWeights,
    frame: i64,
    r: usize,
    x: usize,
    int_start: usize,
    int_end: usize,
    q_col: &mut [f64],
    t_col: &mut [f64],
    traps_col: &mut [[f64; MAX_TRAPS]],
    photo: &F,
    frame_s: f64,
    field_s: f64,
    b_fb: f64,
    q_act: f64,
) -> (f64, bool)
where
    F: Fn(usize, usize, usize, f64) -> f64 + ?Sized,
{
    let has_traps = !traps_col.is_empty();
    if ch.act {
        let t_fb = flyback_time_s(frame, r);
        act_flyback_step(
            ch_idx,
            ch,
            x,
            rw.fb_range.clone(),
            t_fb,
            q_col,
            t_col,
            traps_col,
            photo,
            field_s,
            b_fb,
            q_act,
        );
    }
    let t_now = read_time_s(frame, r, x);
    let check_hot = ch.bending > 0.0;
    let mut hot = false;
    for k in int_start..int_end {
        let trap_occ = if has_traps { Some(&mut traps_col[k]) } else { None };
        let q = integrate_sub_row(
            ch_idx,
            ch,
            x,
            k,
            t_now,
            &mut q_col[k],
            &mut t_col[k],
            trap_occ,
            photo,
            field_s,
        );
        if check_hot && ch.potential_v(q) > V_BEND_ONSET_V {
            hot = true;
        }
    }
    let band_end = rw.k_start + rw.shares.len();
    let i_b = match ch.dbc {
        Some(dbc) => dbc_beam(ch, dbc, &q_col[rw.k_start..band_end], &rw.shares, frame_s),
        None => ch.beam_na,
    };
    (i_b, hot)
}

/// Step 5 (bending on): find the captor `(x', k')` for aim `(x, k)` among elements above
/// `V_BEND_ONSET_V` within their radius `rho(V)`, nearest first, breaking ties by higher `V`,
/// then smaller `x'`, then smaller `k'`.
#[allow(clippy::too_many_arguments)]
fn find_captor(
    ch: &TubeChannel,
    q_all: &[f64],
    col_stride: usize,
    col_hot: &[bool],
    x_lo: usize,
    x_hi: usize,
    x: usize,
    k: usize,
    int_start: usize,
    int_end: usize,
    dk_max: usize,
    p_h: f64,
) -> (usize, usize) {
    if ch.potential_v(q_all[x * col_stride + k]) > V_BEND_ONSET_V {
        return (x, k);
    }
    if !col_hot[x_lo..=x_hi].iter().any(|&h| h) {
        return (x, k);
    }
    let k_lo = k.saturating_sub(dk_max).max(int_start);
    let k_hi = (k + dk_max).min(int_end - 1);
    let mut best: Option<(f64, f64, usize, usize)> = None;
    for xp in x_lo..=x_hi {
        if !col_hot[xp] {
            continue;
        }
        let dx = (xp as f64 - x as f64) * p_h;
        let dx2 = dx * dx;
        let col_q = &q_all[xp * col_stride..];
        for kp in k_lo..=k_hi {
            let v = ch.potential_v(col_q[kp]);
            if v <= V_BEND_ONSET_V {
                continue;
            }
            let rho = ch.bend_radius_lines(v);
            let dk = (kp as f64 - k as f64) / SUB_ROWS as f64;
            let d = (dx2 + dk * dk).sqrt();
            if d <= rho {
                let better = match best {
                    None => true,
                    Some((bd, bv, bx, bk)) => {
                        d < bd
                            || (d == bd
                                && (v > bv || (v == bv && (xp < bx || (xp == bx && kp < bk)))))
                    }
                };
                if better {
                    best = Some((d, v, xp, kp));
                }
            }
        }
    }
    match best {
        Some((_, _, xp, kp)) => (xp, kp),
        None => (x, k),
    }
}

/// Scan all active rows of one frame for a single column `x` and channel `ch_idx`, updating the
/// column's stored sub-row charges `q_col`, last-landed times `t_col`, and (when the channel has
/// traps) trap occupancies `traps_col` in place.
#[allow(clippy::too_many_arguments)]
fn scan_column<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    weights: &[RowWeights],
    frame: i64,
    x: usize,
    q_col: &mut [f64],
    t_col: &mut [f64],
    traps_col: &mut [[f64; MAX_TRAPS]],
    raw_col: &mut [f64],
    photo: &F,
) where
    F: Fn(usize, usize, usize, f64) -> f64 + ?Sized,
{
    let frame_s = 1.0 / FRAME_RATE;
    let field_s = 0.5 * frame_s;
    let k_nas = ch.k_nas;
    let b_fb = act_flyback_nas();
    let q_act = ch.act_level_nas();
    let bending = ch.bending > 0.0;
    let n_sub = SUB_ROWS * ACTIVE_H;
    let pad = if bending { (BEND_RADIUS_MAX_LINES * SUB_ROWS as f64).ceil() as usize } else { 0 };
    let dk_max = pad;
    let p_h = sample_pitch_lines();

    let max_band =
        if bending { weights.iter().map(|rw| rw.shares.len()).max().unwrap_or(0) } else { 0 };
    let max_int = max_band + 2 * pad;
    let mut captors = if bending { vec![0usize; max_band] } else { Vec::new() };
    let mut b_received = if bending { vec![0.0; max_int] } else { Vec::new() };
    let mut landed_elem = if bending { vec![0.0; max_int] } else { Vec::new() };

    for field in 0..2 {
        for r in (field..ACTIVE_H).step_by(2) {
            let rw = &weights[r];
            let band_start = rw.k_start;
            let band_len = rw.shares.len();
            if band_len == 0 {
                raw_col[r] = 0.0;
                continue;
            }
            let band_end = band_start + band_len;
            let int_start = band_start.saturating_sub(pad);
            let int_end = (band_end + pad).min(n_sub);
            let (i_b, hot) = prepare_column(
                ch_idx,
                ch,
                rw,
                frame,
                r,
                x,
                int_start,
                int_end,
                q_col,
                t_col,
                traps_col,
                photo,
                frame_s,
                field_s,
                b_fb,
                q_act,
            );
            let beam_frame_nas = i_b * frame_s;
            if !hot {
                let mut landed_sum = 0.0;
                for (j, &share) in rw.shares.iter().enumerate() {
                    let k = band_start + j;
                    let before = q_col[k];
                    let after = scan(beam_frame_nas * share, k_nas, before);
                    landed_sum += before - after;
                    q_col[k] = after;
                }
                raw_col[r] = landed_sum / (SUB_ROWS as f64 * frame_s);
            } else {
                let int_len = int_end - int_start;
                b_received[..int_len].fill(0.0);
                for (j, &share) in rw.shares.iter().enumerate() {
                    let k = band_start + j;
                    let (_, ck) = find_captor(
                        ch,
                        q_col,
                        n_sub,
                        &[true],
                        0,
                        0,
                        0,
                        k,
                        int_start,
                        int_end,
                        dk_max,
                        p_h,
                    );
                    let off = ck - int_start;
                    captors[j] = off;
                    b_received[off] += beam_frame_nas * share;
                }
                for offset in 0..int_len {
                    let b_e = b_received[offset];
                    if b_e > 0.0 {
                        let ck = int_start + offset;
                        let before = q_col[ck];
                        let after = scan(b_e, k_nas, before);
                        q_col[ck] = after;
                        landed_elem[offset] = before - after;
                    } else {
                        landed_elem[offset] = 0.0;
                    }
                }
                let mut landed_sum = 0.0;
                for (j, &share) in rw.shares.iter().enumerate() {
                    let idx = captors[j];
                    let b_e = b_received[idx];
                    if b_e > 0.0 {
                        let b = beam_frame_nas * share;
                        landed_sum += landed_elem[idx] * (b / b_e);
                    }
                }
                raw_col[r] = landed_sum / (SUB_ROWS as f64 * frame_s);
            }
        }
    }
}

/// Scan all active rows of one frame row by row for channel `ch_idx` when beam bending is active,
/// parallelising across columns within each pass.
#[allow(clippy::too_many_arguments)]
fn scan_channel_row_by_row<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    weights: &[RowWeights],
    frame: i64,
    q_all: &mut [f64],
    t_all: &mut [f64],
    traps_all: &mut [[f64; MAX_TRAPS]],
    raw_all: &mut [f64],
    photo: &F,
) where
    F: Fn(usize, usize, usize, f64) -> f64 + Sync + ?Sized,
{
    let n_sub = SUB_ROWS * ACTIVE_H;
    let frame_s = 1.0 / FRAME_RATE;
    let field_s = 0.5 * frame_s;
    let k_nas = ch.k_nas;
    let b_fb = act_flyback_nas();
    let q_act = ch.act_level_nas();
    let pad = (BEND_RADIUS_MAX_LINES * SUB_ROWS as f64).ceil() as usize;
    let p_h = sample_pitch_lines();
    let dx_max = (BEND_RADIUS_MAX_LINES / p_h).floor() as usize;
    let dk_max = pad;

    let max_band = weights.iter().map(|rw| rw.shares.len()).max().unwrap_or(0);
    let max_int = max_band + 2 * pad;
    let mut i_b = vec![0.0f64; ACTIVE_W];
    let mut col_hot = vec![false; ACTIVE_W];
    let mut captors = vec![(0usize, 0usize); ACTIVE_W * max_band];
    let mut b_received = vec![0.0f64; ACTIVE_W * max_int];
    let mut landed_elem = vec![0.0f64; ACTIVE_W * max_int];

    for field in 0..2 {
        for r in (field..ACTIVE_H).step_by(2) {
            let rw = &weights[r];
            let band_start = rw.k_start;
            let band_len = rw.shares.len();
            if band_len == 0 {
                raw_all.par_chunks_mut(ACTIVE_H).for_each(|col| col[r] = 0.0);
                continue;
            }
            let band_end = band_start + band_len;
            let int_start = band_start.saturating_sub(pad);
            let int_end = (band_end + pad).min(n_sub);
            let int_len = int_end - int_start;

            if traps_all.is_empty() {
                q_all
                    .par_chunks_mut(n_sub)
                    .zip(t_all.par_chunks_mut(n_sub))
                    .zip(i_b.par_iter_mut())
                    .zip(col_hot.par_iter_mut())
                    .enumerate()
                    .for_each(|(x, (((q_col, t_col), ib_out), hot_out))| {
                        let (ib, hot) = prepare_column(
                            ch_idx,
                            ch,
                            rw,
                            frame,
                            r,
                            x,
                            int_start,
                            int_end,
                            q_col,
                            t_col,
                            &mut [],
                            photo,
                            frame_s,
                            field_s,
                            b_fb,
                            q_act,
                        );
                        *ib_out = ib;
                        *hot_out = hot;
                    });
            } else {
                q_all
                    .par_chunks_mut(n_sub)
                    .zip(t_all.par_chunks_mut(n_sub))
                    .zip(traps_all.par_chunks_mut(n_sub))
                    .zip(i_b.par_iter_mut())
                    .zip(col_hot.par_iter_mut())
                    .enumerate()
                    .for_each(|(x, ((((q_col, t_col), trap_col), ib_out), hot_out))| {
                        let (ib, hot) = prepare_column(
                            ch_idx,
                            ch,
                            rw,
                            frame,
                            r,
                            x,
                            int_start,
                            int_end,
                            q_col,
                            t_col,
                            trap_col,
                            photo,
                            frame_s,
                            field_s,
                            b_fb,
                            q_act,
                        );
                        *ib_out = ib;
                        *hot_out = hot;
                    });
            }

            if !col_hot.iter().any(|&h| h) {
                q_all
                    .par_chunks_mut(n_sub)
                    .zip(raw_all.par_chunks_mut(ACTIVE_H))
                    .zip(i_b.par_iter())
                    .for_each(|((q_col, raw_col), &ib)| {
                        let beam_frame_nas = ib * frame_s;
                        let mut landed_sum = 0.0;
                        for (j, &share) in rw.shares.iter().enumerate() {
                            let k = band_start + j;
                            let before = q_col[k];
                            let after = scan(beam_frame_nas * share, k_nas, before);
                            landed_sum += before - after;
                            q_col[k] = after;
                        }
                        raw_col[r] = landed_sum / (SUB_ROWS as f64 * frame_s);
                    });
                continue;
            }

            // Step 5: capture across columns (read-only on q_all).
            {
                let q_ro: &[f64] = q_all;
                let hot_ro: &[bool] = &col_hot;
                captors[..ACTIVE_W * band_len]
                    .par_chunks_mut(band_len)
                    .enumerate()
                    .for_each(|(x, cap_col)| {
                        let x_lo = x.saturating_sub(dx_max);
                        let x_hi = (x + dx_max).min(ACTIVE_W - 1);
                        for j in 0..band_len {
                            cap_col[j] = find_captor(
                                ch,
                                q_ro,
                                n_sub,
                                hot_ro,
                                x_lo,
                                x_hi,
                                x,
                                band_start + j,
                                int_start,
                                int_end,
                                dk_max,
                                p_h,
                            );
                        }
                    });
            }

            // Step 6: accumulate captured beam per element, then land once per element.
            b_received[..ACTIVE_W * int_len].fill(0.0);
            for x in 0..ACTIVE_W {
                let beam_frame_nas = i_b[x] * frame_s;
                let cap_col = &captors[x * band_len..(x + 1) * band_len];
                for (j, &share) in rw.shares.iter().enumerate() {
                    let (cx, ck) = cap_col[j];
                    b_received[cx * int_len + (ck - int_start)] += beam_frame_nas * share;
                }
            }

            q_all
                .par_chunks_mut(n_sub)
                .zip(b_received[..ACTIVE_W * int_len].par_chunks(int_len))
                .zip(landed_elem[..ACTIVE_W * int_len].par_chunks_mut(int_len))
                .for_each(|((q_col, b_col), land_col)| {
                    for offset in 0..int_len {
                        let b_e = b_col[offset];
                        if b_e > 0.0 {
                            let ck = int_start + offset;
                            let before = q_col[ck];
                            let after = scan(b_e, k_nas, before);
                            q_col[ck] = after;
                            land_col[offset] = before - after;
                        } else {
                            land_col[offset] = 0.0;
                        }
                    }
                });

            // Step 7: share landed charge back to each aim's column.
            let b_ro = &b_received[..ACTIVE_W * int_len];
            let land_ro = &landed_elem[..ACTIVE_W * int_len];
            raw_all
                .par_chunks_mut(ACTIVE_H)
                .zip(captors[..ACTIVE_W * band_len].par_chunks(band_len))
                .zip(i_b.par_iter())
                .for_each(|((raw_col, cap_col), &ib)| {
                    let beam_frame_nas = ib * frame_s;
                    let mut landed_sum = 0.0;
                    for (j, &share) in rw.shares.iter().enumerate() {
                        let (cx, ck) = cap_col[j];
                        let idx = cx * int_len + (ck - int_start);
                        let b_e = b_ro[idx];
                        if b_e > 0.0 {
                            let b = beam_frame_nas * share;
                            landed_sum += land_ro[idx] * (b / b_e);
                        }
                    }
                    raw_col[r] = landed_sum / (SUB_ROWS as f64 * frame_s);
                });
        }
    }
}

/// One uniformly lit target column scanned by the same row passes as the full camera.
pub struct UniformColumn {
    ch: TubeChannel,
    weights: Vec<RowWeights>,
    charge: Vec<f64>,
    last_t: Vec<f64>,
    traps: Vec<[f64; MAX_TRAPS]>,
    dark: Vec<f64>,
    frame: i64,
}

impl UniformColumn {
    /// Start at zero charge and scan the column dark until its state settles.
    pub fn new(ch: &TubeChannel) -> Self {
        let weights = build_vertical_weights(ch.sigma_v_lines);
        let n_sub = SUB_ROWS * ACTIVE_H;
        let mut charge = vec![0.0; n_sub];
        let mut last_t = vec![-DARK_SETTLE_S; n_sub];
        let has_traps = ch.traps.iter().any(|t| t.capture > 0.0);
        let mut traps = if has_traps { vec![[0.0; MAX_TRAPS]; n_sub] } else { Vec::new() };
        let mut raw = vec![0.0; ACTIVE_H];
        let dark = |_: usize, _: usize, _: usize, _: f64| 0.0;
        let settle_frames = (DARK_SETTLE_S * FRAME_RATE).ceil() as i64;
        for frame in -settle_frames..0 {
            scan_column(
                0,
                ch,
                &weights,
                frame,
                ACTIVE_W / 2,
                &mut charge,
                &mut last_t,
                &mut traps,
                &mut raw,
                &dark,
            );
        }
        Self { ch: *ch, weights, charge, last_t, traps, dark: raw, frame: 0 }
    }

    /// Read the next frame with photocurrent `photo(r, t)`, nA, the light of frame row `r`
    /// (each sub-row takes its row's light, as `CameraTube`), held within each field's interval.
    pub fn read_rows(&mut self, photo: &dyn Fn(usize, f64) -> f64) -> Vec<f64> {
        let mut raw = vec![0.0; ACTIVE_H];
        let current = |_: usize, r: usize, _: usize, t: f64| photo(r, t);
        scan_column(
            0,
            &self.ch,
            &self.weights,
            self.frame,
            ACTIVE_W / 2,
            &mut self.charge,
            &mut self.last_t,
            &mut self.traps,
            &mut raw,
            &current,
        );
        self.frame += 1;
        raw
    }

    /// Scan one frame with a uniform photocurrent over the target, returning each row's signal in nA.
    pub fn read(&mut self, photo: &dyn Fn(f64) -> f64) -> Vec<f64> {
        self.read_rows(&|_, t| photo(t))
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_channel_dispatch<F>(
    ch_idx: usize,
    ch: &TubeChannel,
    w_ch: &[RowWeights],
    frame: i64,
    q_all: &mut [f64],
    t_all: &mut [f64],
    traps_all: &mut [[f64; MAX_TRAPS]],
    raw_all: &mut [f64],
    photo: &F,
) where
    F: Fn(usize, usize, usize, f64) -> f64 + Sync + ?Sized,
{
    let n_sub = SUB_ROWS * ACTIVE_H;
    if ch.bending > 0.0 {
        scan_channel_row_by_row(
            ch_idx,
            ch,
            w_ch,
            frame,
            q_all,
            t_all,
            traps_all,
            raw_all,
            photo,
        );
    } else if traps_all.is_empty() {
        q_all
            .par_chunks_mut(n_sub)
            .zip(t_all.par_chunks_mut(n_sub))
            .zip(raw_all.par_chunks_mut(ACTIVE_H))
            .enumerate()
            .for_each(|(x, ((q_col, t_col), raw_col))| {
                scan_column(
                    ch_idx,
                    ch,
                    w_ch,
                    frame,
                    x,
                    q_col,
                    t_col,
                    &mut [],
                    raw_col,
                    photo,
                );
            });
    } else {
        q_all
            .par_chunks_mut(n_sub)
            .zip(t_all.par_chunks_mut(n_sub))
            .zip(traps_all.par_chunks_mut(n_sub))
            .zip(raw_all.par_chunks_mut(ACTIVE_H))
            .enumerate()
            .for_each(|(x, (((q_col, t_col), trap_col), raw_col))| {
                scan_column(
                    ch_idx,
                    ch,
                    w_ch,
                    frame,
                    x,
                    q_col,
                    t_col,
                    trap_col,
                    raw_col,
                    photo,
                );
            });
    }
}

/// Subtract dark current and scale slot `STRIPE_SLOT` to the decoder scale where white averages `STRIPE_WHITE_LIGHT`.
fn clamp_and_scale(sig: &TargetSignal, black: f64, peak_na: f64) -> Vec<Vec<f32>> {
    let scale = STRIPE_WHITE_LIGHT / peak_na;
    sig.rows
        .iter()
        .map(|chans| {
            chans[STRIPE_SLOT]
                .iter()
                .map(|&i| ((i as f64 - black) * scale) as f32)
                .collect()
        })
        .collect()
}

/// Mean of each decoded channel over the middle half of the rows and of the line.
fn centre_mean(rows: &[[Vec<f32>; 3]]) -> [f64; 3] {
    let mut sum = [0.0; 3];
    let mut n = 0.0;
    for row in &rows[ACTIVE_H / 4..3 * ACTIVE_H / 4] {
        for x in ACTIVE_W / 4..3 * ACTIVE_W / 4 {
            for k in 0..3 {
                sum[k] += row[k][x] as f64;
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

/// Align the stripe camera on a white card and a capped lens without modifying `base`.
fn align_stripe(base: &CameraTube) -> (StripeAlignment, StripeDecoder) {
    let mut card = base.clone();
    card.set_iris_stops(0.0);
    let mut white_frame = Frame::new(ACTIVE_W, ACTIVE_H);
    white_frame.rgb.fill(255);
    let mut white_sig = card.read(&white_frame, &white_frame);
    for _ in 1..ALIGN_FRAMES {
        white_sig = card.read(&white_frame, &white_frame);
    }
    card.head_amp.amplify(&mut white_sig, ALIGN_WHITE_FRAME);

    let dark_val = base.black[STRIPE_SLOT] as f32;
    let mut capped_sig = TargetSignal {
        rows: (0..ACTIVE_H)
            .map(|_| {
                let mut chans = [Vec::new(), Vec::new(), Vec::new()];
                chans[STRIPE_SLOT] = vec![dark_val; ACTIVE_W];
                chans
            })
            .collect(),
    };
    base.head_amp.amplify(&mut capped_sig, ALIGN_CAPPED_FRAME);

    let peak_na = base.channels[STRIPE_SLOT].peak_signal_na;
    let white_s = clamp_and_scale(&white_sig, base.black[STRIPE_SLOT], peak_na);
    let capped_s = clamp_and_scale(&capped_sig, base.black[STRIPE_SLOT], peak_na);

    let dec1 = StripeDecoder::new(1.0);
    let w1 = centre_mean(&dec1.decode(&white_s));
    let c1 = centre_mean(&dec1.decode(&capped_s));
    let a = 0.5 * ((w1[0] - c1[0]) + (w1[2] - c1[2]));
    let carrier_gain = 1.0 / a;

    let decoder = StripeDecoder::new(carrier_gain);
    let w = centre_mean(&decoder.decode(&white_s));
    let c = centre_mean(&decoder.decode(&capped_s));
    let floor = c;
    let white = [w[0] - c[0], w[1] - c[1], w[2] - c[2]];
    (StripeAlignment { carrier_gain, floor, white }, decoder)
}

/// The pickup tubes of the camera with the charge and last-read time on every sub-element of
/// their targets.
#[derive(Clone)]
pub struct CameraTube {
    layout: Layout,
    spectral: bool,
    taking: [[f64; 3]; 3],
    channels: [TubeChannel; 3],
    /// Signal of a dark target at equilibrium, nA: the level the camera clamps to black.
    black: [f64; 3],
    /// Linear scene light (exposure included) for each 8-bit input level.
    linear_light: [f64; 256],
    /// Photo current for each input level, nA, per channel: the iris, the inverse gamma
    /// corrector and the target's transfer exponent, evaluated once for the 256 levels.
    photo: [[f64; 256]; 3],
    /// Precomputed vertical beam weights per channel and active row.
    weights: [Vec<RowWeights>; 3],
    /// Precomputed horizontal beam spot kernel per channel: `(radius, kernel)`.
    horiz: [(usize, Vec<f64>); 3],
    /// Stored charge per sub-element in nA s, column-major (`x * SUB_ROWS * ACTIVE_H + k`).
    charge: [Vec<f64>; 3],
    /// Last-landed timestamp per sub-element in seconds, column-major.
    last_t: [Vec<f64>; 3],
    /// Trap occupancies per sub-element in nA s, column-major; empty when the channel has no traps.
    traps: [Vec<[f64; MAX_TRAPS]>; 3],
    /// Pre-spot signal scratch per channel, column-major (`x * ACTIVE_H + r`).
    raw_cols: [Vec<f64>; 3],
    /// Held photocurrent map of the previous frame's field 1 (`ACTIVE_H x ACTIVE_W x 3` for
    /// `ThreeTube`, `ACTIVE_H x ACTIVE_W` for `Stripe`), nA. Empty before the first `read`.
    prev_f1_photo: Vec<f64>,
    frame: i64,
    head_amp: HeadAmplifier,
    processor: VideoProcessor,
    alignment: Option<StripeAlignment>,
    decoder: Option<StripeDecoder>,
}

impl CameraTube {
    /// The stage for `params` with deterministic noise `seed`, or `None` when the camera has no
    /// tube. Every sub-element starts at dark equilibrium, found by scanning a dark target until
    /// the reading settles.
    pub fn seeded(params: &CameraParams, seed: u64) -> Option<Self> {
        let mut channels = params.tube.channels_at(params.beam_ratio, params.temperature_c)?;
        let materials = params.tube.materials()?;
        let bending = params.blooming.max(0.0);
        for ch in &mut channels {
            ch.bending = bending;
        }
        if params.layout == Layout::Stripe {
            channels = [channels[STRIPE_SLOT]; 3];
        }
        let spectral = params.spectral && params.layout == Layout::ThreeTube;
        let taking = if spectral { taking_matrix(materials) } else { IDENTITY_MATRIX };
        let exposure = params.iris_stops.exp2();
        let linear_light: [f64; 256] =
            std::array::from_fn(|v| oetf_inverse(v as f64 / 255.0) * exposure);
        let n_sub = SUB_ROWS * ACTIVE_H;

        let (weights, horiz) = match params.layout {
            Layout::ThreeTube => (
                std::array::from_fn(|ch| build_vertical_weights(channels[ch].sigma_v_lines)),
                std::array::from_fn(|ch| build_horizontal_kernel(channels[ch].sigma_h_s)),
            ),
            Layout::Stripe => {
                let mut weights: [Vec<RowWeights>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                let mut horiz: [(usize, Vec<f64>); 3] =
                    [(0, Vec::new()), (0, Vec::new()), (0, Vec::new())];
                weights[STRIPE_SLOT] = build_vertical_weights(channels[STRIPE_SLOT].sigma_v_lines);
                horiz[STRIPE_SLOT] = build_horizontal_kernel(channels[STRIPE_SLOT].sigma_h_s);
                (weights, horiz)
            }
        };

        let mut black = [0.0; 3];
        let mut photo = [[0.0; 256]; 3];
        let mut charge: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut last_t: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut traps: [Vec<[f64; MAX_TRAPS]>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut raw_cols: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];

        let init_slot = |ch: &TubeChannel,
                         charge_k: &mut Vec<f64>,
                         last_t_k: &mut Vec<f64>,
                         traps_k: &mut Vec<[f64; MAX_TRAPS]>,
                         raw_k: &mut Vec<f64>|
         -> f64 {
            let settled = UniformColumn::new(ch);
            let q_col = settled.charge;
            let t_col = settled.last_t;
            let tr_col = settled.traps;
            let dark = settled.dark.iter().sum::<f64>() / ACTIVE_H as f64;
            *charge_k = vec![0.0; ACTIVE_W * n_sub];
            *last_t_k = vec![0.0; ACTIVE_W * n_sub];
            if !tr_col.is_empty() {
                *traps_k = vec![[0.0; MAX_TRAPS]; ACTIVE_W * n_sub];
            }
            *raw_k = vec![0.0; ACTIVE_W * ACTIVE_H];
            for x in 0..ACTIVE_W {
                let off = x * n_sub;
                charge_k[off..off + n_sub].copy_from_slice(&q_col);
                let dx_s = (x as f64 - (ACTIVE_W / 2) as f64) / FS_BB;
                for i in 0..n_sub {
                    last_t_k[off + i] = t_col[i] + dx_s;
                }
                if !tr_col.is_empty() {
                    traps_k[off..off + n_sub].copy_from_slice(&tr_col);
                }
            }
            dark
        };

        match params.layout {
            Layout::ThreeTube => {
                for k in 0..3 {
                    black[k] = init_slot(
                        &channels[k],
                        &mut charge[k],
                        &mut last_t[k],
                        &mut traps[k],
                        &mut raw_cols[k],
                    );
                }
            }
            Layout::Stripe => {
                let dark = init_slot(
                    &channels[STRIPE_SLOT],
                    &mut charge[STRIPE_SLOT],
                    &mut last_t[STRIPE_SLOT],
                    &mut traps[STRIPE_SLOT],
                    &mut raw_cols[STRIPE_SLOT],
                );
                black = [dark; 3];
            }
        }
        for k in 0..3 {
            for (v, p) in photo[k].iter_mut().enumerate() {
                *p = photo_current(&channels[k], linear_light[v]);
            }
        }

        let prev_f1_photo = Vec::new();
        let spec = params.processing.spec();
        let head_amp = HeadAmplifier::new(&spec.head_amp, seed);
        let mut processor = VideoProcessor::new(params.processing, &channels, black);
        if spectral && spec.matrix {
            processor = processor.with_matrix(colour_matrix(materials));
        }
        let mut cam = Self {
            layout: params.layout,
            spectral,
            taking,
            channels,
            black,
            linear_light,
            photo,
            weights,
            horiz,
            charge,
            last_t,
            traps,
            raw_cols,
            prev_f1_photo,
            frame: 0,
            head_amp,
            processor,
            alignment: None,
            decoder: None,
        };
        if params.layout == Layout::Stripe {
            let (alignment, decoder) = align_stripe(&cam);
            cam.processor = VideoProcessor::with_levels(
                params.processing,
                &cam.channels,
                alignment.floor,
                alignment.white,
            );
            cam.alignment = Some(alignment);
            cam.decoder = Some(decoder);
        }
        Some(cam)
    }

    /// The stage for `params` with `seed = 0`, or `None` when the camera has no tube.
    pub fn new(params: &CameraParams) -> Option<Self> {
        Self::seeded(params, 0)
    }

    /// How the camera's tubes see colour.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The stripe camera's alignment, or `None` with three tubes.
    pub fn stripe_alignment(&self) -> Option<StripeAlignment> {
        self.alignment
    }

    /// Settled dark-equilibrium reading per channel, nA (the camera's black clamp).
    pub fn black(&self) -> [f64; 3] {
        self.black
    }

    /// Update the iris exposure setting for subsequent frames.
    pub fn set_iris_stops(&mut self, iris_stops: f64) {
        let exposure = iris_stops.exp2();
        for (v, l) in self.linear_light.iter_mut().enumerate() {
            *l = oetf_inverse(v as f64 / 255.0) * exposure;
        }
        for k in 0..3 {
            for (v, p) in self.photo[k].iter_mut().enumerate() {
                *p = photo_current(&self.channels[k], self.linear_light[v]);
            }
        }
    }

    /// The channels as built (operating point, `K`, spot widths).
    pub fn channels(&self) -> &[TubeChannel; 3] {
        &self.channels
    }

    /// The taking matrix in use: `taking_matrix(kind.materials())` with `spectral`, else the identity.
    pub fn taking(&self) -> [[f64; 3]; 3] {
        self.taking
    }

    /// The processor's colour matrix: `colour_matrix(..)` with `spectral` and a profile that has one,
    /// else the identity.
    pub fn colour_matrix(&self) -> [[f64; 3]; 3] {
        self.processor.matrix()
    }

    /// The photocurrent the picture `f` drives on each target element, nA, indexed
    /// `(r * ACTIVE_W + x) * 3 + ch`: the light through the taking, then the target's sensitivity
    /// and transfer. What `read` integrates for a picture held over a field.
    pub fn photo_currents(&self, f: &Frame) -> Vec<f64> {
        if self.layout == Layout::Stripe {
            let mono = self.stripe_frame_photo(f);
            let mut out = vec![0.0; ACTIVE_W * ACTIVE_H * 3];
            for (i, &p) in mono.iter().enumerate() {
                out[i * 3 + STRIPE_SLOT] = p;
            }
            return out;
        }
        self.frame_photo(f)
    }

    /// Read one frame from photocurrents: `photo(ch, r, x, t)` is the target-equivalent photo
    /// current density, nA, at element (row r, sample x) at time t. It is constant over each
    /// field's interval (the pictures are held), so the engine integrates it in pieces split at
    /// the field boundaries, evaluating it once inside each piece. Advances the frame counter.
    pub fn read_currents(
        &mut self,
        photo: &(dyn Fn(usize, usize, usize, f64) -> f64 + Sync),
    ) -> TargetSignal {
        self.read_currents_impl(photo)
    }

    fn read_currents_impl<F>(&mut self, photo: &F) -> TargetSignal
    where
        F: Fn(usize, usize, usize, f64) -> f64 + Sync + ?Sized,
    {
        let frame = self.frame;
        let channels = &self.channels;
        let weights = &self.weights;
        let horiz = &self.horiz;
        let raw_cols = &mut self.raw_cols;

        if self.layout == Layout::Stripe {
            scan_channel_dispatch(
                STRIPE_SLOT,
                &channels[STRIPE_SLOT],
                &weights[STRIPE_SLOT],
                frame,
                &mut self.charge[STRIPE_SLOT],
                &mut self.last_t[STRIPE_SLOT],
                &mut self.traps[STRIPE_SLOT],
                &mut raw_cols[STRIPE_SLOT],
                photo,
            );
            let (radius, ref kernel) = horiz[STRIPE_SLOT];
            let raw_ch = &raw_cols[STRIPE_SLOT];
            let rows: Vec<[Vec<f32>; 3]> = (0..ACTIVE_H)
                .into_par_iter()
                .map_init(
                    || vec![0.0; ACTIVE_W],
                    |scratch, r| {
                        for x in 0..ACTIVE_W {
                            scratch[x] = raw_ch[x * ACTIVE_H + r];
                        }
                        let mut out = vec![0.0f32; ACTIVE_W];
                        convolve_horizontal(scratch, radius, kernel, &mut out);
                        let mut chans = [Vec::new(), Vec::new(), Vec::new()];
                        chans[STRIPE_SLOT] = out;
                        chans
                    },
                )
                .collect();
            self.frame += 1;
            return TargetSignal { rows };
        }

        self.charge
            .par_iter_mut()
            .zip(self.last_t.par_iter_mut())
            .zip(self.traps.par_iter_mut())
            .zip(raw_cols.par_iter_mut())
            .enumerate()
            .for_each(|(ch_idx, (((q_all, t_all), traps_all), raw_all))| {
                scan_channel_dispatch(
                    ch_idx,
                    &channels[ch_idx],
                    &weights[ch_idx],
                    frame,
                    q_all,
                    t_all,
                    traps_all,
                    raw_all,
                    photo,
                );
            });

        let rows: Vec<[Vec<f32>; 3]> = (0..ACTIVE_H)
            .into_par_iter()
            .map_init(
                || vec![0.0; ACTIVE_W],
                |scratch, r| {
                    std::array::from_fn(|ch| {
                        let raw_ch = &raw_cols[ch];
                        for x in 0..ACTIVE_W {
                            scratch[x] = raw_ch[x * ACTIVE_H + r];
                        }
                        let mut out = vec![0.0f32; ACTIVE_W];
                        let (radius, ref kernel) = horiz[ch];
                        convolve_horizontal(scratch, radius, kernel, &mut out);
                        out
                    })
                },
            )
            .collect();

        self.frame += 1;
        TargetSignal { rows }
    }

    /// Convert an RGB frame into per-element photocurrents (nA) for three-tube mode.
    fn frame_photo(&self, frame: &Frame) -> Vec<f64> {
        let expected = ACTIVE_W * ACTIVE_H * 3;
        assert_eq!(frame.rgb.len(), expected, "the camera stage takes frames of the engine's active size");
        let mut out = vec![0.0; expected];
        let row_len = ACTIVE_W * 3;
        if !self.spectral {
            let photo = &self.photo;
            out.par_chunks_mut(row_len)
                .zip(frame.rgb.par_chunks(row_len))
                .for_each(|(dst, src)| {
                    for (i, &v) in src.iter().enumerate() {
                        dst[i] = photo[i % 3][v as usize];
                    }
                });
            return out;
        }
        let linear = &self.linear_light;
        let t = &self.taking;
        let chs = &self.channels;
        out.par_chunks_mut(row_len)
            .zip(frame.rgb.par_chunks(row_len))
            .for_each(|(dst, src)| {
                for x in 0..ACTIVE_W {
                    let p = x * 3;
                    let l_in = [
                        linear[src[p] as usize],
                        linear[src[p + 1] as usize],
                        linear[src[p + 2] as usize],
                    ];
                    for ch in 0..3 {
                        let l_ch = t[ch][0] * l_in[0] + t[ch][1] * l_in[1] + t[ch][2] * l_in[2];
                        dst[p + ch] = photo_current(&chs[ch], l_ch);
                    }
                }
            });
        out
    }

    /// Convert an RGB frame into per-element photocurrents (nA) through the optical low-pass and stripe filter.
    fn stripe_frame_photo(&self, frame: &Frame) -> Vec<f64> {
        let expected = ACTIVE_W * ACTIVE_H * 3;
        assert_eq!(frame.rgb.len(), expected, "the camera stage takes frames of the engine's active size");
        let mut out = vec![0.0; ACTIVE_W * ACTIVE_H];
        let linear = &self.linear_light;
        let ch = &self.channels[STRIPE_SLOT];
        let row_rgb_len = ACTIVE_W * 3;
        out.par_chunks_mut(ACTIVE_W)
            .zip(frame.rgb.par_chunks(row_rgb_len))
            .enumerate()
            .for_each(|(r, (dst, src))| {
                let lp: [Vec<f64>; 3] = std::array::from_fn(|c| {
                    let row: Vec<f64> = (0..ACTIVE_W).map(|x| linear[src[x * 3 + c] as usize]).collect();
                    optical_lowpass(&row)
                });
                for x in 0..ACTIVE_W {
                    let light = target_light(r, x, [lp[0][x], lp[1][x], lp[2][x]]);
                    dst[x] = photo_current(ch, light / STRIPE_WHITE_LIGHT);
                }
            });
        out
    }

    /// Read one frame from pictures: field 0's interval sees `f0`, field 1's sees `f1`.
    pub fn read(&mut self, f0: &Frame, f1: &Frame) -> TargetSignal {
        let t_f0 = self.frame as f64 / FRAME_RATE;
        let t_f1 = (self.frame as f64 + 0.5) / FRAME_RATE;
        if self.layout == Layout::Stripe {
            let cur_f0 = self.stripe_frame_photo(f0);
            let cur_f1 = if std::ptr::eq(f0, f1) { None } else { Some(self.stripe_frame_photo(f1)) };
            let f1_ref: &[f64] = cur_f1.as_deref().unwrap_or(&cur_f0);
            let prev_f1 = std::mem::take(&mut self.prev_f1_photo);
            let sig = self.read_currents_impl(&|_ch, r, x, t| {
                let idx = r * ACTIVE_W + x;
                if t < t_f0 && !prev_f1.is_empty() {
                    prev_f1[idx]
                } else if t < t_f1 {
                    cur_f0[idx]
                } else {
                    f1_ref[idx]
                }
            });
            self.prev_f1_photo = cur_f1.unwrap_or(cur_f0);
            return sig;
        }
        let cur_f0 = self.frame_photo(f0);
        let cur_f1 = if std::ptr::eq(f0, f1) { None } else { Some(self.frame_photo(f1)) };
        let f1_ref: &[f64] = cur_f1.as_deref().unwrap_or(&cur_f0);
        let prev_f1 = std::mem::take(&mut self.prev_f1_photo);
        let sig = self.read_currents_impl(&|ch, r, x, t| {
            let idx = (r * ACTIVE_W + x) * 3 + ch;
            if t < t_f0 && !prev_f1.is_empty() {
                prev_f1[idx]
            } else if t < t_f1 {
                cur_f0[idx]
            } else {
                f1_ref[idx]
            }
        });
        self.prev_f1_photo = cur_f1.unwrap_or(cur_f0);
        sig
    }

    fn stripe_decode_raw(&mut self, f0: &Frame, f1: &Frame) -> Vec<[Vec<f32>; 3]> {
        let frame_idx = self.frame;
        let mut sig = self.read(f0, f1);
        self.head_amp.amplify(&mut sig, frame_idx);
        let s = clamp_and_scale(&sig, self.black[STRIPE_SLOT], self.channels[STRIPE_SLOT].peak_signal_na);
        self.decoder.as_ref().expect("stripe decoder").decode(&s)
    }

    /// The stripe camera's colour of one frame: read, head amplifier, clamp and scale, decoder, then
    /// `(v - floor) / white` per channel. That is linear light with white 1.0 (in the tube's own
    /// transfer for a tube that linearises), before linearise, knee, gamma, aperture correction and clip.
    /// Advances the frame as `video` does. `None` with three tubes, which then reads nothing.
    pub fn stripe_colour(&mut self, f0: &Frame, f1: &Frame) -> Option<Vec<[Vec<f32>; 3]>> {
        if self.layout != Layout::Stripe {
            return None;
        }
        let alignment = self.alignment?;
        let mut rows = self.stripe_decode_raw(f0, f1);
        for row in &mut rows {
            for k in 0..3 {
                let floor = alignment.floor[k];
                let white = alignment.white[k];
                for v in &mut row[k] {
                    *v = ((*v as f64 - floor) / white) as f32;
                }
            }
        }
        Some(rows)
    }

    /// Scan one frame from pictures (`f0` for field 0, `f1` for field 1), amplify and process
    /// the line signal through the camera electronics.
    pub fn video(&mut self, f0: &Frame, f1: &Frame) -> CameraVideo {
        if self.layout == Layout::Stripe {
            let rows = self.stripe_decode_raw(f0, f1);
            return self.processor.process(&TargetSignal { rows });
        }
        let frame_idx = self.frame;
        let mut sig = self.read(f0, f1);
        self.head_amp.amplify(&mut sig, frame_idx);
        self.processor.process(&sig)
    }

    /// The picture path: `read`, then the camera electronics, then the encoder.
    pub fn encode_fields(&mut self, f0: &Frame, f1: &Frame) -> Baseband {
        let vid = self.video(f0, f1);
        encode_from_rgb(|row, x| {
            let r = &vid.rows[row];
            [r[0][x], r[1][x], r[2][x]]
        })
    }

    /// Expose the targets to `frame` for both fields, scan them, and encode the continuous
    /// camera output signal directly to baseband Y / C without writing an intermediate `u8`
    /// video frame.
    pub fn encode_baseband(&mut self, frame: &Frame) -> Baseband {
        self.encode_fields(frame, frame)
    }

    /// Expose the targets to `frame` for both fields, scan them, and return what the
    /// camera outputs. The target state is kept for the next frame.
    pub fn process_frame(&mut self, frame: &Frame) -> Frame {
        let vid = self.video(frame, frame);
        let mut out = Frame::new(ACTIVE_W, ACTIVE_H);
        for (row, r) in vid.rows.iter().enumerate() {
            for x in 0..ACTIVE_W {
                let p = (row * ACTIVE_W + x) * 3;
                for k in 0..3 {
                    out.rgb[p + k] = (255.0f32 * r[k][x]).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: u8) -> Frame {
        let mut f = Frame::new(ACTIVE_W, ACTIVE_H);
        f.rgb.fill(v);
        f
    }

    #[test]
    fn the_default_camera_has_no_tube() {
        let p = CameraParams::default();
        assert_eq!(p.tube, TubeKind::Off);
        assert_eq!(p.iris_stops, 0.0);
        assert_eq!(p.beam_ratio, BEAM_RATIO_DEFAULT);
        assert!(CameraTube::new(&p).is_none());
    }

    #[test]
    fn the_tube_is_named_in_the_config_as_the_data_sheet_types_are() {
        let p: CameraParams = toml::from_str("tube = \"plumbicon-act\"\niris_stops = 1.5").unwrap();
        assert_eq!(p.tube, TubeKind::PlumbiconAct);
        assert_eq!(p.iris_stops, 1.5);
        assert_eq!(p.beam_ratio, BEAM_RATIO_DEFAULT);
    }

    #[test]
    fn a_light_that_goes_out_leaves_a_fading_trace_and_the_charge_is_kept_between_frames() {
        let p = CameraParams { tube: TubeKind::Plumbicon, ..CameraParams::default() };
        let mut cam = CameraTube::new(&p).unwrap();
        for _ in 0..40 {
            cam.process_frame(&flat(128));
        }
        // Check field 1 (row 1), which integrates the dark half-frame before it is read.
        let row1_green = |f: Frame| {
            let margin = 16;
            let sum: f64 =
                (margin..ACTIVE_W - margin).map(|x| f.rgb[(ACTIVE_W + x) * 3 + 1] as f64).sum();
            sum / (ACTIVE_W - 2 * margin) as f64
        };
        let first = row1_green(cam.process_frame(&flat(0)));
        let second = row1_green(cam.process_frame(&flat(0)));
        let third = row1_green(cam.process_frame(&flat(0)));
        assert!(first > second && second >= third, "{first} {second} {third}");
        assert!(first > 0.0 && first < 64.0, "{first}");
    }
}
