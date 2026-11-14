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
use super::video::{encode_from_rgb, row_to_line, Baseband, Frame};
use crate::tube::{
    oetf_inverse, photo_current, scan, TubeChannel, TubeKind, BEAM_RATIO_DEFAULT, MAX_TRAPS,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Sub-rows per active frame row on each target, so the beam's vertical profile overlaps the
/// other field's lines.
pub const SUB_ROWS: usize = 4;

/// Dark scanning time before frame 0; exceeds 30 times the slowest dark settling constant.
const DARK_SETTLE_S: f64 = 10.0;

/// Cap on the blooming share: a larger figure is held here, so an overexposed element always
/// keeps most of its own excess. [est]
const BLOOMING_MAX: f64 = 0.45;
/// Share of the spilled current that lands on each of the two nearest neighbours of an element.
const BLOOM_NEAR_SHARE: f64 = 0.35;
/// Share of the spilled current that lands on each of the two second neighbours; the four
/// shares add up to all of it.
const BLOOM_FAR_SHARE: f64 = 0.15;

/// Signal currents read from the targets in one frame: per active frame row, per channel, one
/// value per baseband sample of the active line, nA (before the head amplifier).
#[derive(Clone, Debug, PartialEq)]
pub struct TargetSignal {
    pub rows: Vec<[Vec<f32>; 3]>,
}

/// Seconds from the session start at which the beam centre reads sample `x` of active row `r`
/// in frame `n`.
pub fn read_time_s(n: i64, r: usize, x: usize) -> f64 {
    let (line, _) = row_to_line(r);
    n as f64 / FRAME_RATE + line as f64 / FH + (ACTIVE_START + x) as f64 / FS_BB
}

/// The camera: which pickup tubes it has and how it is set up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraParams {
    /// The pickup tube (`off`, `plumbicon`, `plumbicon-bias`, `plumbicon-act`, `vidicon`,
    /// `newvicon`, `saticon`, `chalnicon`).
    pub tube: TubeKind,
    /// Exposure of the scene on the tube targets, in stops (powers of two of the light).
    /// Positive values overexpose: highlights then carry more than the beam can take off.
    pub iris_stops: f64,
    /// Beam current as a multiple of what peak white needs (the data sheet sets 2.0).
    pub beam_ratio: f64,
    /// Beam-bending blooming: the share of the photo current above the beam current that spills
    /// onto the neighbouring elements of the line (0.0 = off, the default; capped at 0.45).
    pub blooming: f64,
    /// Camera electronics profile (`studio`, `consumer`).
    pub processing: Processing,
    /// Target temperature in degrees Celsius (default 25.0).
    pub temperature_c: f64,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self {
            tube: TubeKind::Off,
            iris_stops: 0.0,
            beam_ratio: BEAM_RATIO_DEFAULT,
            blooming: 0.0,
            processing: Processing::default(),
            temperature_c: 25.0,
        }
    }
}

/// Precomputed vertical Gaussian beam shares `g(y_k - c_r) / S_k` for one active row `r`.
#[derive(Clone)]
struct RowWeights {
    k_start: usize,
    shares: Vec<f64>,
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
        .map(|(k_start, mut gs)| {
            for (j, g) in gs.iter_mut().enumerate() {
                *g /= s_k[k_start + j];
            }
            RowWeights { k_start, shares: gs }
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
    let beam_frame_nas = ch.beam_na * frame_s;
    let bias_na = ch.bias_na;
    let dark_plus_bias = ch.dark_na + bias_na;
    let plate_nas = ch.plate_nas;
    let k_nas = ch.k_nas;
    let act = ch.act;
    let has_traps = !traps_col.is_empty();

    for field in 0..2 {
        for r in (field..ACTIVE_H).step_by(2) {
            let t_now = read_time_s(frame, r, x);
            let rw = &weights[r];
            let mut landed_sum = 0.0;
            for (j, &share) in rw.shares.iter().enumerate() {
                let k = rw.k_start + j;
                let t_prev = t_col[k];
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
                        if has_traps {
                            let j_light = p_na + bias_na;
                            let occ = &mut traps_col[k];
                            for (p, trap) in ch.traps.iter().enumerate() {
                                if trap.capture > 0.0 {
                                    let b = trap.capture * j_light / trap.capacity_nas
                                        + 1.0 / trap.release_s;
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
                let mut before =
                    (q_col[k] + photo_nas + dark_plus_bias * dt - trap_dn).min(plate_nas);
                if act {
                    before = before.min(beam_frame_nas);
                }
                let beam_share = beam_frame_nas * share;
                let after = scan(beam_share, k_nas, before);
                landed_sum += before - after;
                q_col[k] = after;
                t_col[k] = t_now;
            }
            raw_col[r] = landed_sum / (SUB_ROWS as f64 * frame_s);
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

    /// Scan one frame with a uniform photocurrent over the target, returning each row's signal in nA.
    pub fn read(&mut self, photo: &dyn Fn(f64) -> f64) -> Vec<f64> {
        let mut raw = vec![0.0; ACTIVE_H];
        let current = |_: usize, _: usize, _: usize, t: f64| photo(t);
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
}

/// The pickup tubes of the camera with the charge and last-read time on every sub-element of
/// their targets.
#[derive(Clone)]
pub struct CameraTube {
    channels: [TubeChannel; 3],
    /// Signal of a dark target at equilibrium, nA: the level the camera clamps to black.
    black: [f64; 3],
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
    /// Held photocurrent map of the previous frame's field 1 (`ACTIVE_H x ACTIVE_W x 3`), nA.
    /// Empty before the first `read`, which holds the first picture over `t < 0`.
    prev_f1_photo: Vec<f64>,
    blooming: f64,
    frame: i64,
    head_amp: HeadAmplifier,
    processor: VideoProcessor,
}

impl CameraTube {
    /// The stage for `params` with deterministic noise `seed`, or `None` when the camera has no
    /// tube. Every sub-element starts at dark equilibrium, found by scanning a dark target until
    /// the reading settles.
    pub fn seeded(params: &CameraParams, seed: u64) -> Option<Self> {
        let channels = params.tube.channels_at(params.beam_ratio, params.temperature_c)?;
        let exposure = params.iris_stops.exp2();
        let blooming = params.blooming;
        let n_sub = SUB_ROWS * ACTIVE_H;

        let weights: [Vec<RowWeights>; 3] =
            std::array::from_fn(|ch| build_vertical_weights(channels[ch].sigma_v_lines));
        let horiz: [(usize, Vec<f64>); 3] =
            std::array::from_fn(|ch| build_horizontal_kernel(channels[ch].sigma_h_s));

        let mut black = [0.0; 3];
        let mut photo = [[0.0; 256]; 3];
        let mut charge: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; ACTIVE_W * n_sub]);
        let mut last_t: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; ACTIVE_W * n_sub]);
        let mut traps: [Vec<[f64; MAX_TRAPS]>; 3] = std::array::from_fn(|_| Vec::new());

        for k in 0..3 {
            let settled = UniformColumn::new(&channels[k]);
            let q_col = settled.charge;
            let t_col = settled.last_t;
            let tr_col = settled.traps;
            let row_dark = settled.dark;
            black[k] = row_dark.iter().sum::<f64>() / ACTIVE_H as f64;
            if !tr_col.is_empty() {
                traps[k] = vec![[0.0; MAX_TRAPS]; ACTIVE_W * n_sub];
            }
            for x in 0..ACTIVE_W {
                let off = x * n_sub;
                charge[k][off..off + n_sub].copy_from_slice(&q_col);
                let dx_s = (x as f64 - (ACTIVE_W / 2) as f64) / FS_BB;
                for i in 0..n_sub {
                    last_t[k][off + i] = t_col[i] + dx_s;
                }
                if !tr_col.is_empty() {
                    traps[k][off..off + n_sub].copy_from_slice(&tr_col);
                }
            }
            for (v, p) in photo[k].iter_mut().enumerate() {
                *p = photo_current(&channels[k], oetf_inverse(v as f64 / 255.0) * exposure);
            }
        }

        let raw_cols = std::array::from_fn(|_| vec![0.0; ACTIVE_W * ACTIVE_H]);
        let prev_f1_photo = Vec::new();
        let spec = params.processing.spec();
        let head_amp = HeadAmplifier::new(&spec.head_amp, seed);
        let processor = VideoProcessor::new(params.processing, &channels, black);
        Some(Self {
            channels,
            black,
            photo,
            weights,
            horiz,
            charge,
            last_t,
            traps,
            raw_cols,
            prev_f1_photo,
            blooming,
            frame: 0,
            head_amp,
            processor,
        })
    }

    /// The stage for `params` with `seed = 0`, or `None` when the camera has no tube.
    pub fn new(params: &CameraParams) -> Option<Self> {
        Self::seeded(params, 0)
    }

    /// Settled dark-equilibrium reading per channel, nA (the camera's black clamp).
    pub fn black(&self) -> [f64; 3] {
        self.black
    }

    /// Update the iris exposure setting for subsequent frames.
    pub fn set_iris_stops(&mut self, iris_stops: f64) {
        let exposure = iris_stops.exp2();
        for k in 0..3 {
            for (v, p) in self.photo[k].iter_mut().enumerate() {
                *p = photo_current(&self.channels[k], oetf_inverse(v as f64 / 255.0) * exposure);
            }
        }
    }

    /// The channels as built (operating point, `K`, spot widths).
    pub fn channels(&self) -> &[TubeChannel; 3] {
        &self.channels
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
        let n_sub = SUB_ROWS * ACTIVE_H;
        let frame = self.frame;
        let channels = &self.channels;
        let weights = &self.weights;
        let horiz = &self.horiz;
        let raw_cols = &mut self.raw_cols;

        self.charge
            .par_iter_mut()
            .zip(self.last_t.par_iter_mut())
            .zip(self.traps.par_iter_mut())
            .zip(raw_cols.par_iter_mut())
            .enumerate()
            .for_each(|(ch_idx, (((q_all, t_all), traps_all), raw_all))| {
                let ch = &channels[ch_idx];
                let w_ch = &weights[ch_idx];
                if traps_all.is_empty() {
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

    /// Convert an RGB frame into per-element photocurrents (nA), applying lateral blooming if set.
    fn frame_photo(&self, frame: &Frame) -> Vec<f64> {
        let expected = ACTIVE_W * ACTIVE_H * 3;
        assert_eq!(frame.rgb.len(), expected, "the camera stage takes frames of the engine's active size");
        let mut out = vec![0.0; expected];
        let photo = &self.photo;
        let channels = &self.channels;
        let blooming = self.blooming;
        let row_len = ACTIVE_W * 3;
        out.par_chunks_mut(row_len)
            .zip(frame.rgb.par_chunks(row_len))
            .for_each(|(dst, src)| {
                for (i, &v) in src.iter().enumerate() {
                    dst[i] = photo[i % 3][v as usize];
                }
                if blooming > 0.0 {
                    Self::apply_blooming(dst, channels, blooming);
                }
            });
        out
    }

    /// Read one frame from pictures: field 0's interval sees `f0`, field 1's sees `f1`.
    pub fn read(&mut self, f0: &Frame, f1: &Frame) -> TargetSignal {
        let cur_f0 = self.frame_photo(f0);
        let cur_f1 = if std::ptr::eq(f0, f1) { None } else { Some(self.frame_photo(f1)) };
        let f1_ref: &[f64] = cur_f1.as_deref().unwrap_or(&cur_f0);
        let t_f0 = self.frame as f64 / FRAME_RATE;
        let t_f1 = (self.frame as f64 + 0.5) / FRAME_RATE;
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

    /// Scan one frame from pictures (`f0` for field 0, `f1` for field 1), amplify and process
    /// the line signal through the camera electronics.
    pub fn video(&mut self, f0: &Frame, f1: &Frame) -> CameraVideo {
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

    /// Beam-bending lateral charge deflection across adjacent elements on the scanned target line.
    fn apply_blooming(row_photo: &mut [f64], channels: &[TubeChannel; 3], blooming: f64) {
        let b = blooming.clamp(0.0, BLOOMING_MAX);
        for k in 0..3 {
            let beam_k = channels[k].beam_na;
            let mut diffs = [0.0f64; ACTIVE_W];
            for x in 0..ACTIVE_W {
                let p = row_photo[x * 3 + k];
                if p > beam_k {
                    diffs[x] = (p - beam_k) * b;
                }
            }
            for x in 0..ACTIVE_W {
                let d = diffs[x];
                if d > 0.0 {
                    row_photo[x * 3 + k] -= d;
                    let d1 = BLOOM_NEAR_SHARE * d;
                    let d2 = BLOOM_FAR_SHARE * d;
                    if x > 0 { row_photo[(x - 1) * 3 + k] += d1; } else { row_photo[x * 3 + k] += d1; }
                    if x + 1 < ACTIVE_W { row_photo[(x + 1) * 3 + k] += d1; } else { row_photo[x * 3 + k] += d1; }
                    if x > 1 { row_photo[(x - 2) * 3 + k] += d2; } else { row_photo[x * 3 + k] += d2; }
                    if x + 2 < ACTIVE_W { row_photo[(x + 2) * 3 + k] += d2; } else { row_photo[x * 3 + k] += d2; }
                }
            }
        }
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
