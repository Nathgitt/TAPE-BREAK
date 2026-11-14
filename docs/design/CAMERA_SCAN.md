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
- The ACT gun, when fitted, discharges highlights during the line flyback before each pass over a
  span of lines (see "Highlights"; this replaces the clip at the stabilisable charge).
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

## Highlights

Source: `docs/CAMERA_RESEARCH.md`, "Beam capacity of twice peak white turns highlights into comet
tails". A highlight the read beam cannot take off leaves charge on the target. Three things decide
what then happens: the gun (a flyback that discharges it, or a beam that rises to meet it), the
element's potential (an unstabilised element bends the beam towards itself), and the target (how much
it stores and in which colour). All three act on the target and the beam during the scan; nothing is
added to the picture.

### Decisions

- **ACT flyback over a span of lines.** The read-time clip of the ACT gun is replaced by a flyback
  before every pass: a defocused beam spread over a band of lines discharges every element above the
  cutting level, so it reaches highlights a few lines before the read beam does (US 4,143,305). Only
  active lines have a pass, so only they have a flyback: an overload at the top of the picture sees one
  flyback before it is read instead of several.
- **Dynamic beam control as its own kind.** A diode gun with dynamic beam control joins as
  `TubeKind::PlumbiconDbc`. Its beam follows the Ampex law (US 4,229,767) along the line. The diode
  gun's cooler beam, `V_T = 0.10 V`, comes with a target of more capacitance: the XQ3440 (diode) and
  XQ1520 (ACT triode) print the same low-key lag (Mullard 1987), so `C` is chosen to keep `K = C V_T`.
- **Beam bending replaces the lateral spill.** The spill moved photocurrent sideways in the input
  picture before the target. Light does not move; the beam does. The spill was keyed on the light of
  the moment, so it vanished as soon as the light went although the element stayed charged, and no gun
  could cut it. Beam bending is keyed on the stored potential of each element (Philips 1963: an
  unstabilised element "will start to attract electrons towards it when the beam approaches, and
  continue to do so after the beam should have passed"), so a bloom lasts as long as its comet tail,
  and ACT and dynamic beam control, which hold the potential down, cut it as the data say. The config
  key `blooming` stays and now sets the bending strength; `0.0` (the default) is off.
- **Trail colour needs no new mechanism.** It follows from each channel's capacitance, beam and traps:
  the red and blue PbO tubes run half the green current with half its beam on targets of about the
  same capacitance, so they store more overload per unit of beam and their tails outlast green's (red
  over green, "red-trail", RCA 1977); a Saticon camera has three identical tubes, so its residue is
  white, and its low lag clears it within about 2 s at a beam of 3 to 4 times the highlight (RCA 1977,
  Neuhauser 1978). Which of red and blue clears last depends on blue's capacitance (fitted in "Target
  materials and traps", 1.32 nF with its traps) and is not judged.

### One pass with the highlight physics

Each pass of row `r` in frame `n` (per channel, with `c_r = r + 0.5` and `y_k = (k + 0.5) / SUB_ROWS`
in frame-line pitches) runs, in this order:

1. **Flyback** (ACT only). At `t_fb = n T_frame + line / FH` (the start of the pass's line, `line` from
   `row_to_line(r)`, the same for every column), every element whose sub-row lies in the span
   `|y_k - c_r| <= h_act`, `h_act = ACT_SPAN_PH * ACTIVE_H / 2`, is integrated to `t_fb` by the usual
   rule (light in held pieces, dark, bias, traps, plate limit) and, if `q > q_act`,
   `q <- q_act + scan(b_fb, K, q - q_act)`; an element at or below `q_act` is left exactly as it is.
   The charge landed in flyback goes to the head amplifier during blanking and is not in
   `TargetSignal`.
2. **Integration** of every element of the pass's band (the sub-rows with a beam share in this pass)
   to its read time `read_time_s(n, r, x)`, as now. With bending on, the elements within
   `ceil(BEND_RADIUS_MAX_LINES * SUB_ROWS)` sub-rows above and below the band are integrated to the same
   times too (they may capture beam).
3. **Beam current per column** `I_b(x)`: `beam_na`, or the dynamic beam control's (below).
4. **Aims.** The beam aimed at element `(x, k)` of the band is `b(x, k) = I_b(x) T_frame w_rk` with
   `w_rk` the existing share `g(y_k - c_r) / S_k`.
5. **Capture** (bending on). Each aim lands on its captor (below), otherwise on its own element.
6. **Landing.** Each element receives `B_e`, the sum of the aims it captures, and lands once:
   `after = scan(B_e, K, before)`, `landed_e = before - after`. Elements of the band that receive no
   beam keep their charge.
7. **Signal.** `landed_e` is shared among the aims `e` captured in proportion to their `b`, and each
   share counts at the aim's column (the beam was there when it landed). Row `r` at column `x` reads the
   sum of the shares of the aims at column `x` divided by `SUB_ROWS * T_frame`, and the horizontal spot
   follows as now.

Without ACT, dynamic beam control or bending, steps 1, 3 and 5 do nothing and the pass is the present
one, bit for bit. Bending couples the columns of a pass, so with bending on the passes run row by row
(all columns of a pass, then the next pass); without it the column-by-column engine may stay.
`UniformColumn` runs the same pass on its one column; its neighbours are lit alike, so a capture by a
neighbouring column never beats one by its own column, and only captors in its own column count.

### ACT flyback

- Cutting level per channel `[R, G, B]`: `ACT_LEVEL_V = [3.5, 7.0, 3.5]` V, the XQ1520 cathode-pulse
  presets (Mullard 1987) [DS]. In charge, `q_act = C * act_level_v * 1e9` nA s (green 8.05, red 4.83).
- Flyback beam: `ACT_BEAM_NA = 100 000` nA, grid 1 raised to about 100 uA (Mullard 1987) [DS], all of it
  taken to reach the target [est]; for `ACT_FLYBACK_S = 8.0e-6` s [est, inside the 10.9 to 12 us line
  blanking]; spread evenly over a band of `ACT_SPAN_PH = 1/30` of the picture height [est: "many
  scanning lines", US 4,143,305; 16 lines at 525, 19.2 at 625]. Each element of the span is offered,
  per flyback, `b_fb = ACT_BEAM_NA * ACT_FLYBACK_S / ACT_SPAN_PH = 24.0` nA s in target-equivalent
  units, the same in both standards.
- Why these sizes [DER]. With `h_act` = 8 lines (525) an element is in the span of 8 passes of each
  field, 4 before its read and 4 after. A highlight's element is at the plate by the time the span
  reaches it (32 times peak white fills it in about 5 ms). Four flybacks (96 nA s) bring a full element
  to the cutting level in every channel (plate less `q_act`: green 43.7, red 57.3, blue at most 73 nA s),
  so the read beam finds at most `q_act` and the flybacks after it take what the read left: no tail at
  any overload, which covers the data's 32 times over a spot of 10 % of the picture height (5 stops).
  One flyback (24 nA s) does not: the first lines of each field are read with their overload in place,
  which is US 4,143,305's "not pre-discharged" at the top of the picture.
- With the cutting levels below the 10 V bending onset, ACT also cuts blooming wherever it has
  pre-discharged the highlight, and not at the top of the picture (US 4,143,305).

### Dynamic beam control

- `TubeKind::PlumbiconDbc` (config `plumbicon-dbc`): the PlumbiconAct channel with `act = false`,
  `v_t = V_T_DIODE_GUN_V`, `capacitance_f` times `V_T_TRIODE_GUN_V / V_T_DIODE_GUN_V` (1.7), so `k_nas`
  is unchanged and the plate charge grows by 1.7; `k_nas` and `plate_nas` recomputed from them as for
  every kind; `dbc = Some(DBC)`. The rest of the PbO material (gamma, dark current, traps) as the other
  Plumbicon kinds.
- The Ampex law: the beam stays at `beam_na` until the video passes `beam_na - headroom` and then
  stays `headroom` above it, up to the gun's largest beam. `DBC.headroom = 0.5` of `beam_na` (300 nA
  over a 600 nA base, US 4,229,767) [DS], `DBC.max_ratio = 17` times `peak_signal_na` [DER: 16 times
  peak white handled (Mullard 1987, 4 stops) needs a beam of 16 times peak plus the headroom of one
  peak]. The loop is taken as fast as the scan [est]; its delay is out of scope.
- Per column `x` of the pass, with the band's charges after step 2:
  `I_v(I) = sum_k (q_k - scan(I T_frame w_rk, K, q_k)) / (SUB_ROWS T_frame)` (the column's own reading,
  before any capture), `head = headroom * beam_na`, `max = max(max_ratio * peak_signal_na, beam_na)`,
  `g(I) = I - head - I_v(I)`. If `g(beam_na) >= 0`, `I_b = beam_na`; else if `g(max) <= 0`,
  `I_b = max`; else bisect on `[beam_na, max]` keeping `g(lo) < 0 <= g(hi)` until
  `hi - lo <= DBC_TOLERANCE_NA` (0.01 nA) and take `I_b = hi`.
- Consequence: a steady light reads linearly to 16 times peak white, a highlight up to 16 times leaves
  no tail, and beyond it the beam stops at `max`. In red and blue a full element holds more than one
  pass of the largest beam takes, so a stop beyond (32 times) leaves a tail; a green element at the
  plate fits in one pass and is cleared (the plate caps what it can store).

### Beam bending

- An element's potential above the cathode is `V = q / (capacitance_f * 1e9)` V (`q` in nA s). Above
  the onset `V_BEND_ONSET_V = 10` V (Philips kept the swing within about 10 V because more "deflects
  the approaching beam excessively", Philips 1963) [DS], it captures the beam aimed within its radius
  `rho(V) = min(bending * (V - V_BEND_ONSET_V) / V_BEND_ONSET_V, BEND_RADIUS_MAX_LINES)` frame-line
  pitches, `0` at or below the onset. `bending` is the free strength (no extent data exist, research
  "Open gaps"); at `bending = 1` an element at the 45 V plate reaches 3.5 lines. `BEND_RADIUS_MAX_LINES
  = 4` bounds the search [est].
- Distance between element `(x', k')` and aim `(x, k)`:
  `d = sqrt(((x' - x) p_h)^2 + ((k' - k) / SUB_ROWS)^2)`, `p_h = TARGET_ASPECT * ACTIVE_H / ACTIVE_W`
  (`sample_pitch_lines()`; 0.849 at 525, 1.038 at 625), `TARGET_ASPECT = 4/3`.
- The captor of an aim is, among the elements with `V > V_BEND_ONSET_V` and `d <= rho(V)` (an aim's own
  element if it is above the onset, at `d = 0`), the nearest; ties go to the higher `V`, then the smaller
  `x'`, then the smaller `k'`. An aim with no such element lands on its own element. The potentials are
  those after steps 1 and 2: each element's own read-time charge stands for its potential both as the
  beam approaches and after it has passed [est; an element that bends the beam is beyond what one pass
  stabilises, so it is still positive after the pass].
- `CameraParams.blooming` sets `bending` on every channel (negative values count as 0). `TubeKind`
  channels carry `bending = 0.0`.

### Defaults and what stays

- The camera off is untouched (the golden test). The default `blooming = 0.0` leaves `Plumbicon`,
  `PlumbiconBias` and the material kinds bit-identical to the engine before this stage. `PlumbiconAct`
  changes where highlights exceed the cutting level; below it, its readings are the plain gun's up to
  rounding, so its lag is unchanged.
- `apply_blooming`, `BLOOMING_MAX`, `BLOOM_NEAR_SHARE` and `BLOOM_FAR_SHARE` go; `frame_photo` maps
  levels to photocurrent only. `docs/CAMERA.md`'s paragraphs on ACT and blooming are rewritten to match.
  The command line and GUI learn `plumbicon-dbc` in "Front ends".
- Cost: the ACT flyback integrates about four times as many elements per pass; bending runs row by row.

### Interfaces

In `tube.rs`:

```rust
/// ACT cutting level per channel `[R, G, B]`, V (XQ1520 cathode-pulse presets).
pub const ACT_LEVEL_V: [f64; 3] = [3.5, 7.0, 3.5];
pub const ACT_BEAM_NA: f64 = 100_000.0;
pub const ACT_FLYBACK_S: f64 = 8.0e-6;
/// Height of the flyback band, as a fraction of the picture height.
pub const ACT_SPAN_PH: f64 = 1.0 / 30.0;
pub const V_BEND_ONSET_V: f64 = 10.0;
pub const BEND_RADIUS_MAX_LINES: f64 = 4.0;
pub const TARGET_ASPECT: f64 = 4.0 / 3.0;
pub const DBC_TOLERANCE_NA: f64 = 0.01;
/// Dynamic beam control: `headroom` as a fraction of `beam_na`, the largest beam as a multiple of
/// `peak_signal_na`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dbc { pub headroom: f64, pub max_ratio: f64 }
pub const DBC: Dbc = Dbc { headroom: 0.5, max_ratio: 17.0 };
// TubeKind gains (config name in brackets):
//     PlumbiconDbc ("plumbicon-dbc"): diode gun with dynamic beam control.
// TubeChannel gains:
//     act_level_v: f64   // ACT_LEVEL_V[k] for PlumbiconAct, 0.0 for every other kind
//     dbc: Option<Dbc>   // Some(DBC) for PlumbiconDbc only
//     bending: f64       // 0.0 from TubeKind; set by CameraTube from CameraParams::blooming
impl TubeChannel {
    /// `q / (capacitance_f * 1e9)`, V.
    pub fn potential_v(&self, q_nas: f64) -> f64;
    /// `capacitance_f * act_level_v * 1e9`, nA s.
    pub fn act_level_nas(&self) -> f64;
    /// `rho(V)` of this channel's `bending`, frame-line pitches.
    pub fn bend_radius_lines(&self, potential_v: f64) -> f64;
}
```

In `camera.rs` (both engines):

```rust
/// Charge offered to each element of the span per flyback, nA s:
/// `ACT_BEAM_NA * ACT_FLYBACK_S / ACT_SPAN_PH`.
pub fn act_flyback_nas() -> f64;
/// Horizontal sample pitch in frame-line pitches: `TARGET_ASPECT * ACTIVE_H / ACTIVE_W`.
pub fn sample_pitch_lines() -> f64;
impl UniformColumn {
    /// Read the next frame with photocurrent `photo(r, t)`, nA, the light of frame row `r` (each
    /// sub-row takes its row's light, as `CameraTube`), held within each field's interval. `read(p)`
    /// is `read_rows(|_, t| p(t))`.
    pub fn read_rows(&mut self, photo: &dyn Fn(usize, f64) -> f64) -> Vec<f64>;
}
```

`CameraParams.blooming` keeps its name and type; its doc becomes the bending strength.

### Out of scope

- The loop delay of dynamic beam control; spot growth, geometry shift and lag from a raised beam; the
  flyback current seen by the head amplifier; the ACT on vertical retrace lines (there is none).
- Bending's displacement of landing energy and any asymmetry between approach and departure; bending
  across channels.
- A red-specific trail mechanism (no data); highlight burn and sticking (Retention); optical flare and
  the anti-halation disc (Geometry).
- Highlight guns on the non-PbO kinds; return-beam tubes (ABO).

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
