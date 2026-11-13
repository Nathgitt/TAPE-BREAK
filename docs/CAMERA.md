# Camera pickup tubes

By default the video that goes into the recorder is taken as it is. With a camera tube on,
it is first treated as what a three-tube Plumbicon camera made of the scene: each channel's
target holds charge across frames, so moving highlights leave lag and, when they are bright
enough, comet tails. The tube physics is in `tube.rs`; the scanned target stage is in
`camera.rs`. See `docs/design/CAMERA_SCAN.md` for the design contract and
`docs/CAMERA_RESEARCH.md` for the underlying data and measurements.

## Chain

The input pictures are the camera's gamma-corrected output, held over their own intervals
(for `push(f)`, both fields see `f`; for `push_fields(f0, f1)`, field 0's half-frame
sees `f0` and field 1's half-frame sees `f1`). Per channel:

1. `v = rgb / 255`; scene light `L = oetf_inverse(v) * 2^iris_stops` (BT.709 transfer, so
   `L = 1` gives peak white at an iris of 0), giving photocurrent `peak * L^0.95` (the
   light transfer exponent of the data sheet). When `blooming > 0`, photocurrent above the
   beam current spills laterally to neighbouring columns of the row before storage.
2. Each target is divided into `ACTIVE_W` columns by `4 * ACTIVE_H` sub-rows (`SUB_ROWS = 4`),
   so the beam overlaps adjacent lines of the other field. Between beam visits, each
   sub-element integrates `(j_photo + j_dark + j_bias) * dt` over the held pictures covering
   its interval, up to the signal-plate limit `C * V_target`.
3. Rows are read in raster order - field 0 top-down, then field 1 top-down - with sample `x`
   of active row `r` in frame `n` read at its own time `read_time_s(n, r, x)`. Each row pass
   offers sub-rows within 3 sigma (`sigma_v = 0.65` frame-line pitches) a vertical Gaussian
   share of the beam using the scan law in `tube.rs` (with ACT flyback clipping when fitted).
4. The charge landed across the pass's sub-rows is summed and divided by `SUB_ROWS * T_frame`
   to give the row signal current in nA, then convolved along the line with a horizontal
   Gaussian beam spot (`sigma_h = 40 ns`, renormalised inside the active line at the ends).
5. `lin = (signal - dark_signal) / peak`, where `dark_signal` is the dark-equilibrium
   reading (the camera's black clamp), and `oetf(lin)` is encoded directly to baseband Y / C.

The stage keeps the charge and last-read time of every sub-element between frames. It begins
with zero charge, then scans a dark column for 10 seconds before frame 0. It is built only when
a tube is selected, so with `tube = "off"` (the default) the picture is untouched and the
golden renders do not change. The same code runs at 525/60 and 625/50: read times, frame
period and sample rate come from the engine's raster constants.

Because the tube's exponent is 0.95 and not 1, and the beam spot has finite width, a still
scene is not returned identically even with the lag at rest: mid greys shift by a couple
of levels and fine spatial detail is softened slightly by the spot.

## Physics

Each element of the target is a small capacitor. Light discharges it through the
photoconductor between scans (in the model: adds charge, counted positive), and the scanning
beam recharges it to the cathode potential. The charge the beam delivers is the signal. Two
limits of the beam make the picture remember the past.

**Discharge (capacitive) lag.** Near the cathode potential only the faster electrons of the
beam can land, because their energies are spread thermally. The landing current falls
exponentially as the element approaches the cathode potential: `i = I_b exp(u / V_T)` for
`u <= 0`, and `i = I_b` above. An element left slightly charged after a scan gives up the
rest over the next scans. In target-equivalent charge `q = C u` (nA s), one scan of one
element is exact:

1. above the cathode potential the whole beam lands: `r1 = min(q, I_b)`, `q -= r1`;
2. with the beam charge `I_b - r1` left, below it: `exp(-q'/K) = exp(-q/K) + (I_b - r1)/K`.

`K = C V_T` is the charge constant in nA s and the signal is the charge removed. A scan leaves
the element slightly below the cathode potential, so a tube at rest holds negative charge;
the first rows of a new light are weak while that charge is worked off. A light bias adds a
steady charge that keeps elements off the slow part of the curve, which is how the XQ1410 cuts
the lag.

**Comet tails.** The beam is set to twice what peak white needs. A highlight beyond that
(the sun on chrome, a lamp, an overexposed face) leaves charge the scan cannot remove. It
accumulates until the element reaches the signal-plate voltage and photoconduction stops
(`C * 45 V`, the target plate limit), and after the highlight has gone it drains at the beam
current, reading above peak white (clipped white) until it is gone. A
longer or brighter highlight stores more; a stronger beam (`beam_ratio`) shortens it.

**Anti-comet-tail (ACT) gun.** It recharges, during the line flyback, every element above the
level the read beam can stabilise, so nothing is left over: the stored charge is cut to the
beam current before the scan and the highlight never outlasts its frame. Only the ordinary
lag remains.

## Tube constants in seconds

The beam lands with an exponential energy distribution. Its temperature voltage is
`V_T = k T_b / e`: `0.17 V` for the triode guns with crossover (1900 to 2350 K, US
4,388,556), and `0.10 V` for a diode gun (about 1200 K, the same patent). All three currents
here use the triode value; the ACT gun is a triode. The fitted target capacitance is shared by
the XQ1020, XQ1410 and XQ1520 of each colour.

The scan charge constant is `K = C V_T` in nA s. The plate limit is `C * 45 V`; neither value
depends on frame rate. Capacitance was fitted on the 625/50 scanned target using the XQ1520 red
decay family against bias (least squares on log ratios) together with the 40 to 80 ms build-up,
and the 60 and 200 ms decay plus 60 ms build-up table figures for green and blue. The data-sheet
read is a uniformly lit column at `ACTIVE_W / 2`, with each active row's target-equivalent
current measured in nA by the same sub-row scan as the camera. See
[`docs/design/CAMERA_SCAN.md`](design/CAMERA_SCAN.md), “Tube constants in seconds”.

| Channel | `C` (nF) | `K = C V_T` (nA s) | `V_T` (V) |
|---|---:|---:|---:|
| R | 1.38 | 0.235 | 0.17 |
| G | 1.15 | 0.196 | 0.17 |
| B | 1.76 | 0.299 | 0.17 |

Blue's larger capacitance stands in for trap lag in the blue layer until trap populations are
modelled. The diode-gun value is provided for later tube types and is not used by the current
Plumbicon family.

The following table records the lag checks from `tests/camera_lag.rs`; percentages are measured
at the indicated time after a light transition. Decay is relative to the signal above dark and
bias; build-up is relative to the final signal. Values are reported in measured (handbook) form.

| Check | Measure | Data (%) | Scanned model (%) |
|---|---|---:|---:|
| XQ1520 R, 0 nA bias | decay 60 / 80 / 120 / 200 ms | 13 / 9 / 5.3 / 3.3 | 14.67 / 10.83 / 6.80 / 3.46 |
| XQ1520 R, 1 nA bias | decay 60 / 80 / 120 / 200 ms | 10 / 7.3 / 3.2 / 1.5 | 12.61 / 8.84 / 4.99 / 2.02 |
| XQ1520 R, 3 nA bias | decay 60 / 80 / 120 / 200 ms | 8.3 / 5.5 / 2.4 / 0.6 | 9.18 / 5.73 / 2.53 / 0.59 |
| XQ1520 R, 8 nA bias | decay 60 / 80 / 120 ms | 3 / 1.9 / 0.25 | 3.84 / 1.69 / 0.35 |
| XQ1520 R, 0 nA bias | build-up 40 / 60 / 80 ms | 52 / 85 / 96 | 56.53 / 85.96 / 97.11 |
| XQ1020 / XQ1520 R | decay 60 / 200 ms; build-up 60 ms | 12.5 / 3.75; 85 | 14.67 / 3.46; 85.96 |
| XQ1020 / XQ1520 G | decay 60 / 200 ms; build-up 60 ms | 9.5 / 3.25; 95 | 12.31 / 2.65; 93.12 |
| XQ1020 / XQ1520 B | decay 60 / 200 ms; build-up 60 ms | 15 / 5.25; 70 | 18.30 / 4.82; 71.33 |
| XQ1410 R / G / B | decay 60 ms | 8 / 7 / 11 | 9.18 / 8.54 / 11.79 |
| XQ1410 G, 40/600 nA | decay 60 ms, no bias / 10 nA bias | 8 / 1–1.7 | 6.84 / 0.98 |
| XQ1020 G, 300/600 nA | decay 60 / 200 ms | 1.2 / 0.4 | 1.04 / 0.19 |
| 525/60 vs 625/50 | XQ1520 R decay 60 / 80 / 120 / 200 ms; build-up 60 ms | same-time curves within 12%; build-up within 10 points | passed within both limits |

The XQ1410 and high-key figures are predictions, not fit targets. The 525-line engine uses the
same constants and is checked at the same elapsed times.

## Known limits

* The slow tail with light bias is too short: at 200 ms the model leaves under 1 % where the
  XQ1410 data shows 2 to 3.5 %. That tail is the PbO photoconductive lag, which is left out
  because the data sheet calls it negligible for the plain tube; with bias it is what is left.
* Blooming by beam bending (a highlight spreading sideways) and burn-in are not modelled. The
  charge stays on its own element.
* The capacitances are fitted at 625/50 and used unchanged at 525/60; elapsed-time lag is
  checked on both raster engines.
* Only the three types of the data sheet. Saticon, Newvicon and Vidicon tubes (a photoconductor
  with a lag of its own) are not here.

## Why tails end magenta

Each colour has its own fitted capacitance and the same signal-plate voltage. Red and blue
carry half the peak current of green with half the beam on them. Their higher stored overload
and capacitance make their tails longer, while blue's larger capacitance also stands in for
trap lag in its layer until traps are modelled. Green runs out first and the tail ends in red
plus blue: magenta.

For the uniform-column highlight checks in `tests/camera_tube.rs`, a 4x highlight leaves
clipped-white tails of 1 frame on green, 4 on red and 6 on blue; at 8x and 100x, green has
1 frame in both cases because the plate limit caps the stored charge. The ACT gun leaves no
clipped-white tail on any channel.

## Why there is no phosphor stage

An earlier version also smeared the decoded picture with a CRT phosphor afterglow of 40 % per
frame. The P22 phosphors of a colour tube decay in microseconds (blue and green) to about a
millisecond (red), far below the 20 ms of a field, so a display does not leave visible trails
and the stage had nothing to model. The trails on old recordings are the camera's, and they
are on the tape.

## Using it

    vhsphys run in.mp4 out.mp4 --preset worn --camera-tube plumbicon --iris-stops 2

`--camera-tube` takes `off`, `plumbicon` (XQ1020), `plumbicon-bias` (XQ1410) and
`plumbicon-act` (XQ1520). `--iris-stops N` is the exposure in stops: 0 is the light that
gives peak white, 2 overexposes by a factor of four, so that bright parts of the picture are
beyond what the beam can take off and leave tails. In a config file:

    [camera]
    tube = "plumbicon"
    iris_stops = 2.0
    beam_ratio = 2.0

The tube's memory starts after its dark settling scan when a session starts. A render that starts
part-way into a clip (`--start`) with a tube on therefore needs a few frames of pre-roll
before its first output frame to match a render of the whole clip; the lag of a long tail is
a few tens of frames.
