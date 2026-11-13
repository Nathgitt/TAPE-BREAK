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
  before the line's pass (as now; refined by a later card).
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

## Cards

| Card | Content |
|---|---|
| cam-03-scan-engine | Target grid, beam passes, read times, held pictures, signal normalisation, horizontal spot; the existing tube parameters converted to `K`. |
| cam-04-tube-in-seconds | Tube parameters in seconds: physical `V_T` per gun, `C` fitted per channel to the handbook curves (decay against bias, build-up at 40 to 80 ms, high key) on the scanned model; the lag measurement re-run on the engine; 50 and 60 Hz from the same constants. |
| cam-05-electronics | Head amplifier noise (amplitude rising with frequency), clamp, flare, shading correction, matrix, knee, gamma, aperture correction from line delays with coring, clip; studio and consumer profiles. |
| cam-06-materials | Sb2S3, Newvicon, Saticon, Chalnicon targets: transfer exponent, dark current against temperature, capacitance, trap populations, spectral weighting of the channels. |
| cam-07-stripe-camera | Single-tube colour: stripe filter on the target behind an optical low-pass, carrier from scan speed, decoder (trap, band-pass, 1H separation, detection, 2.8 MHz Y and 0.5 MHz colour). |
| cam-08-highlights | ACT flyback spanning several lines, dynamic beam control, beam bending from element potential, trail colour. |
| cam-09-geometry | Deflection distortion, three-tube registration, black and white shading, corner focus. |
| cam-10-retention | Trap memory and burn-in, recovery, warm-up and temperature. |
| cam-11-front-ends | Command line, configuration, GUI choices and `docs/CAMERA.md`. |
