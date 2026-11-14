# CONTRACT

# The scanned camera tube

Design of the camera stage rebuilt from `docs/CAMERA_RESEARCH.md`. The tube is simulated as it
works: light charges a capacitive target continuously, and an electron beam reads it line by
line, field by field, at the instant each element passes under it. The camera's signal is the
current the beam lands, a continuous signal in time; every later process (head amplifier,
noise, correction, gamma, colour encoding) acts on that signal. The picture is written only
after the recorder, as for everything else in the project.

## Decisions (lead, 2026-11-13)

- Both camera classes. Shared physics first, then the single-tube stripe camera (home
  camcorders), then the further studio tube types. Trinicon waits for data.
- The scanned model replaces the per-element frame shortcut. It is slower; the camera stays off
  by default, so renders without a tube are unchanged.
- The scene is the input pictures, each held for its own time: a line integrates whatever
  pictures cover the interval since its elements were last read. Nothing is interpolated.

## Time and raster

- Time `t` is in seconds from the start of the session. Frame `n` covers
  `[n T_frame, (n + 1) T_frame)`; its two fields are read in raster order.
- Active frame row `r` is frame line `line` of field `p` (`video::row_to_line(r)`). Sample `x`
  of the row is read at `t = n T_frame + line / FH + (ACTIVE_START + x) / FS_BB`
  (`read_time_s`). Rows of field 0 are therefore read before rows of field 1, and within a
  field from the top down.
- With `push(f)` both fields see the picture `f`. With `push_fields(f0, f1)` field 0's interval
  `[n T_frame, n T_frame + T_frame / 2)` sees `f0` and the rest of the frame sees `f1`. There is
  no weaving: the target integrates each picture over its own interval.
- Each line integrates the light of the frame period before its read, so a frame's read holds
  light from the previous picture too (most of it in the top rows): the camera runs up to a frame
  behind the scene, as a tube camera does. Before the session (`t < 0`) the scene is the first
  picture, so a camera starting on a still scene reads it in every row of its first frame.

## Target

- Per colour channel, a grid of `ACTIVE_W` columns by `SUB_ROWS * ACTIVE_H` sub-rows
  (`SUB_ROWS = 4`): finer than the line pitch, so the beam's Gaussian profile can overlap the
  other field's lines.
- Charge is in target-equivalent nA s: an element of a target lit uniformly by a photocurrent of
  `I` nA (the current the whole target would give) gains `I * dt` in `dt` seconds. In these units
  the charge constant is `K = C * V_T` (nC = nA s) and the plate limit is `C * V_target`.
- Between two visits of the beam an element gains `(j_photo + j_dark + j_bias) * dt`, the photo
  term integrated exactly over the pictures that cover the interval, up to the plate limit.

## Beam

- Each row read is one pass of the beam along the row centre. Its vertical profile is a
  Gaussian of `sigma_v` (in frame-line pitches, per channel) over the sub-rows, truncated at
  3 sigma and normalised so that a uniform target receives, over one frame, beam charge
  `I_b * T_frame` per element (target-equivalent units).
- Landing on each sub-element is the existing exact scan of `tube.rs`: the whole beam share above
  the cathode potential, then `exp(-q'/K) = exp(-q/K) + left/K` below it, with the share of beam
  charge as `left`.
- The ACT gun, when fitted, clips each sub-element at the stabilisable charge during the flyback
  before the line's pass (as now; refined by a later stage).
- Beam charge is in the same target-equivalent units as the stored charge. With `g` the Gaussian
  and `S_k` the sum of `g` over every pass of a frame (both fields) at sub-row `k`, the pass of row
  `r` offers sub-row `k` the beam charge `I_b * T_frame * g(y_k - c_r) / S_k`.
- The signal of the row at sample `x` is the charge landed on column `x` over all sub-rows during
  the pass divided by `SUB_ROWS * T_frame`, so that a uniform steady photocurrent `I` reads
  `I + j_dark + j_bias`. `TargetSignal` holds these currents in nA: no black level is subtracted
  and nothing is normalised to peak white there (that is the electronics' job).
- The horizontal spot is the beam's width moving at scan speed: the row's signal is convolved in
  time with a Gaussian of `sigma_h_s` seconds (per channel). This is an operation on the line
  signal, not on a picture.

## Interfaces

In `camera.rs` (both engines compile it):

```rust
/// Signal currents read from the targets in one frame: per active frame row, per channel, one
/// value per baseband sample of the active line, nA (before the head amplifier).
pub struct TargetSignal { pub rows: Vec<[Vec<f32>; 3]> }

/// Seconds from the session start at which the beam centre reads sample `x` of active row `r`
/// in frame `n`.
pub fn read_time_s(n: i64, r: usize, x: usize) -> f64;

impl CameraTube {
    /// Read one frame from photocurrents: `photo(ch, r, x, t)` is the target-equivalent photo
    /// current density, nA, at element (row r, sample x) at time t. It is constant over each
    /// field's interval (the pictures are held), so the engine integrates it in pieces split at
    /// the field boundaries, evaluating it once inside each piece. Advances the frame counter.
    pub fn read_currents(&mut self, photo: &(dyn Fn(usize, usize, usize, f64) -> f64 + Sync)) -> TargetSignal;
    /// Read one frame from pictures: field 0's interval sees `f0`, field 1's sees `f1`.
    pub fn read(&mut self, f0: &Frame, f1: &Frame) -> TargetSignal;
    /// The picture path: `read`, then the camera electronics, then the encoder.
    pub fn encode_fields(&mut self, f0: &Frame, f1: &Frame) -> Baseband;
    /// `encode_fields(f, f)`.
    pub fn encode_baseband(&mut self, f: &Frame) -> Baseband;
    /// The channels as built (operating point, `K`, spot widths).
    pub fn channels(&self) -> &[TubeChannel; 3];
}
```

`Session::push_fields(f0, f1)` calls `encode_fields(f0, f1)` directly.

## Tube constants in seconds

- The charge constant is `K = C V_T` (nA s) with both factors physical. `V_T = k T_b / e` is set by
  the gun: about 0.17 V for the triode guns with a crossover of the three Plumbicon types
  (1900 to 2350 K) and about 0.10 V for a diode gun (about 1200 K); the ACT gun is a triode.
  `C` is the target capacitance per colour (whole target, F), fitted on the scanned engine to the
  handbook lag and checked against the 1000 to 2000 pF of the data. A colour's target is the same
  in the XQ1020, XQ1410 and XQ1520, so `C` and `V_T` are shared by the three types.
- The plate limit is `C * TARGET_VOLTAGE_V`, so it follows the fitted `C`.
- No constant depends on the frame or field period: 50 and 60 Hz use the same values.
- The per-frame engine (`frame_step`, `dark_equilibrium`, `lag_measurement`, the charge scales
  in nA, `saturation_na`) is retired; the data-sheet tables move into the tests. A target starts at
  dark equilibrium by scanning it dark from the cathode potential until it settles.

In `camera.rs`, the measurement surface of the data sheets: one column of a uniformly lit target,
read by the same scan as `CameraTube` (sub-rows, beam passes at their read times, ACT clip, plate
limit) on column `ACTIVE_W / 2`.

```rust
pub struct UniformColumn { /* sub-row charges and landing times of one column, frame counter */ }
impl UniformColumn {
    /// One channel's target at dark equilibrium; the next read is frame 0.
    pub fn new(ch: &TubeChannel) -> Self;
    /// Read the next frame with photocurrent `photo(t)`, nA, the same over the whole target
    /// and held within each field's interval (evaluated once per piece, as `read_currents`).
    /// Returns the signal of every active row, nA, as `TargetSignal` would hold it.
    pub fn read(&mut self, photo: &dyn Fn(f64) -> f64) -> Vec<f64>;
}
```

In `tube.rs`, `TubeChannel` carries `capacitance_f` (F) and `v_t` (V), with `k_nas` and `plate_nas`
derived from them, and `TubeKind::channels(beam_ratio)` no longer takes a scan time.

## Camera electronics

The head amplifier and the processing amplifier act on the line signal, as the camera's circuits
did: after the tube's read and before the encoder. Source: `docs/CAMERA_RESEARCH.md`, "Head-amplifier
noise and processing lie mostly above 2.5 MHz". Order (US 4032974): head amplifier (noise, band),
clamp, gain to peak white per channel, knee, gamma, aperture correction, clip. The colour matrix
waits for the spectral taking characteristics of the target materials, and flare and shading with their corrections
go with the optics and geometry: without the error they correct, they would only add a fault.

- **Head amplifier.** The tube is a current source into the input capacitance `C_in` of tube, coil,
  FET and strays. The amplifier adds a noise current of one-sided density
  `S(f) = 2 q I + 4 k T / R_f + (2 pi f C_in)^2 4 k T R_n` (A^2/Hz): the shot noise of the signal
  current `I` (the sample's own current, so it follows the picture), the feedback resistor's thermal
  noise, and the FET's voltage noise across `C_in`, which rises in amplitude with frequency (the
  "triangular" spectrum). Signal and noise then pass the amplifier's band: the magnitude of an analog
  second-order Butterworth low-pass, `|H(f)| = 1 / sqrt(1 + (f / band_hz)^4)`, with its delay
  compensated (the camera matches the delays of its channels), realised as a short linear-phase FIR on
  the line signal whose ends repeat their end sample. A bilinear-transform biquad is not used: with its
  corner at 0.38 of the sample rate its poles sit at radius 0.6 and a highlight rings about three times
  longer than through the analog amplifier. The noise is generated along the line signal, independent from
  row to row, channel to channel and frame to frame, and fixed by `(seed, frame, row, channel)` through
  `rng::Rng` stream `STREAM_CAMERA = 61`. Its measured density must follow `S(f) |H(f)|^2`.
- **Clamp.** The dark reading of the settled target (`black`, as now) is subtracted: a capped camera
  reads 0 on average.
- **Gain.** Each channel is divided by its peak-white current, so peak white is 1.0 (white balance on
  a white scene).
- **Knee.** On the linear signal, per channel. With a knee range `R` (the 1984 practice: linear to
  100 % of rated light, 230 % reaching the white clip, US 4470067), `L <= 1` is unchanged and above it
  `1 + (L - 1) s`, `s = (gamma_inverse(WHITE_CLIP) - 1) / (R - 1)`. Without a knee the signal goes to the
  gamma corrector as it is and clips there.
- **Gamma.** `gamma_correct(L)`: `4.5 L` below `0.018` (negative values included), `1.099 L^0.45 - 0.099`
  above, with no clamp (the clip comes last). Equal to `tube::oetf` on `[0, 1]`.
- **Aperture correction.** From the gamma-corrected green signal, added equally to all three channels,
  so edges get no coloured fringe from the corrector. Horizontal: `d_h(t) = s(t) - (s(t - T) + s(t + T)) / 2`
  with the delay `T` in seconds (fractional delays interpolated on the line signal; the line ends
  repeat their end sample). Vertical: `d_v(r) = s(r) - (s(r - 2) + s(r + 2)) / 2` over frame rows, that
  is the lines one line period before and after in the same field (1H delays); rows without such a
  neighbour use their own value. Coring removes small detail, `core(d) = sign(d) max(|d| - c, 0)`.
  The detail added is `k_h core(d_h) + k_v core(d_v)`, so the horizontal response is
  `1 + k_h (1 - cos 2 pi f T)` and the vertical one `1 + k_v (1 - cos 2 pi nu)` for `nu` in cycles per
  field line. The horizontal gain restores the green channel's response at `1 / (2T)`:
  `k_h = (1 / (M(f_p) |H(f_p)|) - 1) / 2` with `M(f) = exp(-2 pi^2 sigma_h^2 f^2)` the spot's response,
  `H` the head amplifier's and `f_p = 1 / (2T)`.
- **Clip.** `[BLACK_CLIP, WHITE_CLIP] = [-0.03, 1.05]` of peak white, after the detail is added (so a
  highlight's overshoot is clipped and its dark undershoot stays).

Profiles (`CameraParams::processing`, config key `processing`, default `studio`):

| | studio (three-tube broadcast camera) | consumer (home camera) |
|---|---|---|
| `C_in`, `R_n`, `R_f`, `T` | 20 pF, 75 ohm, 1 Mohm, 300 K (ranges 15 to 30 pF, 50 to 100 ohm, 0.3 to 1 Mohm) | 30 pF, 100 ohm, 470 kohm, 300 K (the poor end of the ranges: about 45 dB at 300 nA, as the PK-958's "> 45 dB") |
| band (-3 dB) | 5.5 MHz (flat to about 5 MHz, US 3743766) | 5.5 MHz |
| knee range | 2.3 | none [est] |
| aperture `T`, `k_v`, `c` | 150 ns (BBC), 0.5 (`g` = 0.25, BBC Monograph 47), 0.008 [est] | 200 ns (0.2 us line, Panasonic), 0, 0.015 [est] |

Interfaces, in a new engine module `camera_processing.rs`:

```rust
pub const STREAM_CAMERA: u64 = 61;
pub const WHITE_CLIP: f64 = 1.05;
pub const BLACK_CLIP: f64 = -0.03;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Processing { #[default] Studio, Consumer }
pub struct HeadAmpSpec { pub input_c_f: f64, pub r_n_ohm: f64, pub r_f_ohm: f64, pub temp_k: f64, pub band_hz: f64 }
pub struct ApertureSpec { pub delay_s: f64, pub vertical: f64, pub coring: f64 }
pub struct ProcessingSpec { pub head_amp: HeadAmpSpec, pub knee_range: Option<f64>, pub aperture: ApertureSpec }
impl Processing { pub fn spec(self) -> ProcessingSpec; }
pub fn gamma_correct(l: f64) -> f64;
pub fn gamma_inverse(v: f64) -> f64;
/// `k_h` of the restoration rule.
pub fn horizontal_gain(delay_s: f64, sigma_h_s: f64, band_hz: f64) -> f64;
/// Gamma-corrected video of one frame: per active row, per channel, per sample; 1.0 = peak white.
pub struct CameraVideo { pub rows: Vec<[Vec<f32>; 3]> }
pub struct HeadAmplifier { .. }
impl HeadAmplifier {
    pub fn new(spec: &HeadAmpSpec, seed: u64) -> Self;
    /// Noise and band on every row of `sig` (nA in, nA out), for frame `n`.
    pub fn amplify(&self, sig: &mut TargetSignal, n: i64);
}
pub struct VideoProcessor { .. }
impl VideoProcessor {
    /// `black` is the clamp level per channel, nA; `k_h` from the green channel's spot.
    pub fn new(processing: Processing, channels: &[TubeChannel; 3], black: [f64; 3]) -> Self;
    pub fn horizontal_gain(&self) -> f64;
    /// Clamp, gain, knee, gamma, aperture correction, clip.
    pub fn process(&self, sig: &TargetSignal) -> CameraVideo;
}
```

In `camera.rs`: `CameraParams` gains `processing: Processing`; `CameraTube::seeded(params, seed)`
(`new(params)` is `seeded(params, 0)`; `Session` passes its seed); `CameraTube::black() -> [f64; 3]`;
`CameraTube::video(f0, f1) -> CameraVideo` (read, amplify with the frame index of the read, process).
`encode_fields` encodes `video`; `process_frame` writes `video(f, f)` as `(255.0f32 * v).round().clamp(0.0, 255.0) as u8`.

## Target materials and traps

Source: `docs/CAMERA_RESEARCH.md`, "Six photoconductors differ in gamma, dark current and lag
mechanism" and "Traps add an exposure-dependent tail". Four further targets join the lead-oxide
Plumbicon. Each is a material: its transfer exponent, its dark current and how fast that grows with
temperature, its layer capacitance, and its traps. Until the single-tube stripe camera, a camera with
one of them carries three identical tubes behind the prism.

- **Kinds** (`TubeKind`, config names in brackets): `Vidicon` (`vidicon`, Sb2S3, a 2/3-inch home
  camera tube), `Newvicon` (`newvicon`, ZnSe/ZnCdTe, 2/3 inch), `Saticon` (`saticon`, Se-As-Te,
  the low-lag H8397A, 2/3 inch), `Chalnicon` (`chalnicon`, CdSe). The three Plumbicon kinds keep
  every constant they have, except blue's capacitance (below).
- **Transfer.** `photo_current = peak * light^gamma` with `gamma` per channel: PbO 0.95, Sb2S3 0.65
  (the home camera's tube, Panasonic WV-3110), Newvicon 1.0, Saticon 1.0, Chalnicon 0.925 (0.90 to
  0.95). The camera's gamma corrector is set for its tube: a vidicon camera's correction gamma is about
  0.7 on a 0.65 tube, an overall law near 0.45, so for a channel with `linearise` the processing
  amplifier raises the clamped, gained signal to `1 / gamma` (keeping its sign) before the knee. Only the
  vidicon linearises; the 0.95 of PbO and Chalnicon was left in by the cameras.
- **Operating points** (all channels of a non-PbO kind alike): peak signal 200 nA (H8397A 0.2 uA,
  S4075 0.2 uA, home vidicons 200 nA; Chalnicon [est]). Target voltage: PbO 45 V, Saticon 50 V,
  Chalnicon 35 V, Newvicon 25 V [est, 10 to 35 V], vidicon 30 V [est, 10 to 60 V]. Every gun is a
  triode: `V_T = V_T_TRIODE_GUN_V`. The beam is `beam_ratio` times peak as now (the Saticon's
  practice of 3 to 4 times is used in its lag measurement).
- **Dark current against temperature.** `dark(T) = dark_25 * 2^((T - 25) / doubling)`, `T` in deg C.
  Saticon: through the H8397A curve, 0.15 nA at 20 deg C and 2.7 nA at 60 deg C. Chalnicon: 0.1 / 1 / 10
  nA at 0 / 30 / 60 deg C. Newvicon: 5 nA at 25 deg C, doubling every 7.5 deg C (7 to 8). Vidicon: 20 nA
  at 25 deg C (the voltage is set for it), doubling every 7.5 deg C. PbO: 1 nA, no temperature law (none
  is published). `CameraParams.temperature_c` (default 25) sets it; `TubeKind::channels(beam_ratio)` is
  `channels_at(beam_ratio, 25.0)`.
- **Capacitance.** One `C` per material (per colour for PbO), fitted on the scanned engine with
  `V_T = 0.17` together with the traps. Checks against the data: Saticon within 0.58 to 1.16 nF (1000
  to 2000 pF/cm^2 on 6.6 x 8.8 mm); Newvicon more than twice PbO green ("much larger target
  capacitance"); PbO blue within 15 % of the mean of red and green (its excess was standing in for trap
  lag). Red and green PbO stay as fitted.
- **Traps.** Up to `MAX_TRAPS = 2` populations per channel, each with a capture share `f` (0 to 1),
  a capacity `N` (target-equivalent nA s) and a release time `tau` (s). Light (the photocurrent and the
  bias light, not the dark current) of current `j` at an element fills them:
  `dn/dt = f j (1 - n / N) - n / tau`, and the element stores the light less what the traps keep:
  over an interval in which `j` is constant the element gains `j dt - sum(n_end - n_start)`, with
  `n(t) = n_inf + (n_0 - n_inf) exp(-b t)`, `b = f j / N + 1 / tau`, `n_inf = f j / b`. Charge is
  conserved: all trapped charge arrives, late. Each sub-element carries its own `n`, settled with the
  dark settle (bias light on) before frame 0. Traps: PbO blue, Saticon, vidicon. None in PbO red and
  green ("negligible"), Newvicon ("free of photoconductive lag") and Chalnicon ("mainly capacitive").
- **Fit data** (`tests/camera_materials.rs`; the RCA procedure is a 30 nA signal, 5 nA bias, a
  100 nA beam, 60 Hz, a dark target exposed for a number of fields, lag in percent of the signal
  current): XQ1410B blue floor at 200 ms against bias (20/300 nA, 50 Hz); XQ1427B blue and BC4391
  Saticon exposure families; H8397A against bias at 50 ms (0.2 uA, 60 Hz); Newvicon S4075 10 % at
  50 ms (0.2 uA, 60 Hz) and T12 8 to 13 % at 60 ms (50 Hz); Chalnicon 10 % at 50 ms (200 nA, 60 Hz
  [est conditions]); home vidicon 20 % at 50 ms (200 nA, 20 nA dark, 60 Hz), XQ1240 8 % at 200 ms, and
  the bias cut of RCA's vidicon (13 to 9.2 % at 50 ms, 3.3 to 1.2 % at 200 ms with 20 nA).

Interfaces, in `tube.rs`:

```rust
pub const MAX_TRAPS: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trap { pub capture: f64, pub capacity_nas: f64, pub release_s: f64 }
impl Trap { pub const NONE: Trap; }
// TubeChannel gains: gamma, linearise, target_voltage_v, dark_doubling_c (f64::INFINITY for none),
// traps: [Trap; MAX_TRAPS]; plate_nas = capacitance_f * target_voltage_v * 1e9.
impl TubeKind {
    pub fn channels_at(self, beam_ratio: f64, temperature_c: f64) -> Option<[TubeChannel; 3]>;
}
/// Dark current of `kind`'s channels at `temperature_c`, nA.
pub fn dark_at(kind: TubeKind, temperature_c: f64) -> Option<f64>;
pub fn photo_current(ch: &TubeChannel, light: f64) -> f64; // peak * light^ch.gamma
```

`UniformColumn` and `CameraTube` integrate the traps in the same pieces as the light. `GAMMA` stays as
the PbO value.

## Stages

| Stage | Content |
|---|---|
| Scan engine | Target grid, beam passes, read times, held pictures, signal normalisation, horizontal spot; the existing tube parameters converted to `K`. |
| Tube in seconds | Tube parameters in seconds: physical `V_T` per gun, `C` fitted per channel to the handbook curves (decay against bias, build-up at 40 to 80 ms, high key) on the scanned model; the lag measurement re-run on the engine; 50 and 60 Hz from the same constants. |
| Electronics | Head amplifier noise (amplitude rising with frequency) and band, clamp, knee, gamma, aperture correction from line delays with coring, clip; studio and consumer profiles. |
| Materials | Sb2S3, Newvicon, Saticon, Chalnicon targets: transfer exponent, dark current against temperature, capacitance, trap populations. |
| Spectra | Spectral weighting of the channels per material and the colour matrix that goes with it. |
| Stripe camera | Single-tube colour: stripe filter on the target behind an optical low-pass, carrier from scan speed, decoder (trap, band-pass, 1H separation, detection, 2.8 MHz Y and 0.5 MHz colour). |
| Highlights | ACT flyback spanning several lines, dynamic beam control, beam bending from element potential, trail colour. |
| Geometry | Deflection distortion, three-tube registration, black and white shading and its correction, lens flare and its correction, corner focus. |
| Retention | Trap memory and burn-in, recovery, warm-up and temperature. |
| Front ends | Command line, configuration, GUI choices and `docs/CAMERA.md`. |
