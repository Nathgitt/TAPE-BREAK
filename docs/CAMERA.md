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
5. The line signal passes through the head amplifier and the processing amplifier
   (`camera_processing.rs`), then the three clipped gamma-corrected signals are encoded directly
   to baseband Y / C (`encode_from_rgb`).

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
| B | 1.32 | 0.224 | 0.17 |

PbO blue's capacitance sits within 15 % of the red/green mean (1.265 nF); its extra lag comes from
two trap populations in the blue layer (see "Target materials and traps") rather than from a larger
capacitance. The diode-gun value is provided for later tube types and is not used by the current
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
| XQ1020 / XQ1520 B | decay 60 / 200 ms; build-up 60 ms | 15 / 5.25; 70 | 18.60 / 5.82; 77.42 |
| XQ1410 R / G / B | decay 60 ms | 8 / 7 / 11 | 9.18 / 8.54 / 13.24 |
| XQ1410 G, 40/600 nA | decay 60 ms, no bias / 10 nA bias | 8 / 1–1.7 | 6.84 / 0.98 |
| XQ1020 G, 300/600 nA | decay 60 / 200 ms | 1.2 / 0.4 | 1.04 / 0.19 |
| 525/60 vs 625/50 | XQ1520 R decay 60 / 80 / 120 / 200 ms; build-up 60 ms | same-time curves within 12%; build-up within 10 points | passed within both limits |

The XQ1410 and high-key figures are predictions, not fit targets. The 525-line engine uses the
same constants and is checked at the same elapsed times.

## Target materials and traps

Four target materials join lead oxide (`plumbicon`, `plumbicon-bias`, `plumbicon-act`): `vidicon`
(Sb2S3), `newvicon` (ZnSe/ZnCdTe), `saticon` (Se-As-Te) and `chalnicon` (CdSe). A camera with one
of them carries three identical tubes behind the prism, each at 200 nA peak white with a triode
gun (`V_T = 0.17 V`). See [`docs/design/CAMERA_SCAN.md`](design/CAMERA_SCAN.md), "Target
materials and traps", and [`docs/CAMERA_RESEARCH.md`](CAMERA_RESEARCH.md), "Six photoconductors
differ in gamma, dark current and lag mechanism" and "Traps add an exposure-dependent tail".

| Kind | `gamma` | Linearise | Peak (nA) | `V_target` (V) | `dark_25` (nA) | Doubling (deg C) | `C` (nF) | Sources |
|---|---:|---|---:|---:|---:|---:|---:|---|
| `plumbicon` (R / G / B) | 0.95 | no | 150 / 300 / 150 | 45 | 1.0 | none | 1.38 / 1.15 / 1.32 | Philips T10, Mullard 1987; no temperature law published |
| `vidicon` (Sb2S3) | 0.65 | yes | 200 | 30 `[est]` | 20 | 7.5 | 9.9 | Philips T12 / XQ1271, Panasonic WV-3110; `V_target` 10-60 V, doubling 7-8 deg C |
| `newvicon` (ZnSe/ZnCdTe) | 1.0 | no | 200 | 25 `[est]` | 5 | 7.5 | 7.6 | Philips T12, Matsushita S4075; `V_target` 10-35 V, doubling 7-8 deg C |
| `saticon` (Se-As-Te) | 1.0 | no | 200 | 50 | 0.215 | 9.6 | 0.64 | Hitachi H8397A (0.15 nA at 20 C, 2.7 nA at 60 C); layer 0.58-1.16 nF (Hitachi Hyoron 1976) |
| `chalnicon` (CdSe) | 0.925 | no | 200 `[est]` | 35 | 0.681 | 9.0 | 6.4 | Toshiba 1973 (gamma 0.90-0.95; dark 0.1 / 1 / 10 nA at 0 / 30 / 60 C) |

`dark_25` and the doubling step are set so the law `dark(T) = dark_25 * 2^((T - 25) / doubling)`
passes through the published points; `T` is `CameraParams::temperature_c` (deg C, default 25).
Dark current acts on the target like bias light, so a warm tube lags less. Every capacitance
except PbO red and green is fitted on the scanned target together with the traps; the Newvicon's
is more than twice PbO green's ("much larger target capacitance"). For `vidicon` the processing
amplifier raises the clamped, gained signal to `1 / gamma` (keeping its sign) before the knee, as
a vidicon camera's gamma corrector was set for its tube, so the overall law stays the 0.45 one;
lead oxide and the Chalnicon keep their small departure from 1.

**Traps.** Some layers hold part of the light they absorb in traps and give it back later. A
channel has up to `MAX_TRAPS = 2` trap populations on every sub-element, each with a capture
share `f` (0 to 1), a capacity `N` (nA s, counted over the whole target like the stored charge)
and a release time `tau` (s). Light, meaning photocurrent plus bias light but not dark current,
of current `j` fills a population as `dn/dt = f j (1 - n / N) - n / tau`: a share of the light is
caught while there is room, and what is caught leaks out at its own rate. Over each stretch in
which the light is constant (the same field pieces the light is integrated in) this has the
closed form `n(t) = n_inf + (n_0 - n_inf) exp(-b t)` with `b = f j / N + 1 / tau` and
`n_inf = f j / b`, and the element gains `j dt` less what the traps took, `sum(n_end - n_start)`.
Nothing is lost: everything trapped reaches the element later, which is the lag. Under steady
light a population holds `N j / (j + N / (f tau))`, so a small population is filled by a little
bias light and then adds no lag, while a large one stays proportional to the light and bias light
does not touch it. The traps settle with the target in the dark scan before frame 0, with any
bias light on. Only PbO blue, the Saticon and the vidicon have traps; PbO red and green
("negligible"), the Newvicon ("free of photoconductive lag") and the Chalnicon ("mainly
capacitive") have none.

| Channel | Population | `f` | `N` (nA s) | `tau` (s) |
|---|---|---:|---:|---:|
| PbO blue | fast | 0.12 | 0.37 | 0.066 |
| PbO blue | slow | 0.035 | 3.9 | 0.45 |
| Saticon | fast | 0.54 | 0.066 | 0.061 |
| Saticon | slow | 0.09 | 0.40 | 0.82 |
| Vidicon | one | 0.95 | 5.0 | 0.25 |

The blue tube's fast population fills within a few fields and gives the 50 ms excess of short
exposures; the slow one fills over about a second and keeps the 200 ms floor near 2 % that bias
light does not remove. The vidicon's single strong population carries most of its lag, and bias
light cuts its 200 ms tail more than its first fields.

The measured lag beside each figure of `tests/camera_materials.rs` (lag in percent of the signal
above dark and bias, middle rows of the scanned target). The RCA exposure families run on a dark
target lit for the given number of fields at 30 nA with 5 nA bias and a 100 nA beam at 60 Hz;
the XQ1427B is a 2/3-inch tube, so for it blue's capacitance and trap capacities are scaled to
that scanned area.

| Check | Measure | Data (%) | Scanned model (%) |
|---|---|---:|---:|
| XQ1410B blue (20/300 nA, 50 Hz) | decay 200 ms at 0 / 1 / 2 / 3 / 4 / 5 / 8 / 12 nA bias | 5.2 / 3.6 / 2.9 / 2.6 / 2.4 / 2.2 / 2.1 / 2.0 | 5.82 / 4.64 / 3.87 / 3.37 / 3.05 / 2.84 / 2.49 / 2.28 |
| XQ1427B blue | decay 50, 200 ms after 2 / 4 / 16 / 64 / 256 fields | 1.9, 0.3 / 2.4, 0.45 / 3.4, 1.25 / 4.5, 1.95 / 4.7, 2.25 | 1.60, 0.21 / 1.96, 0.36 / 2.80, 0.93 / 3.56, 1.48 / 3.61, 1.52 |
| Saticon BC4391 | decay 50, 200 ms after 2 / 4 / 16 / 64 / 256 fields | 1.5, 0.2 / 1.8, 0.25 / 2.3, 0.45 / 2.45, 0.5 / 2.55, 0.5 | 2.41, 0.21 / 2.61, 0.36 / 3.07, 0.73 / 3.16, 0.80 / 3.16, 0.80 |
| Saticon H8397A (200 nA, 3.5x beam, 30 C, 60 Hz) | decay 50 ms at 0 / 5 / 10 / 16 nA bias; relative to 0 nA | 6.7 / 3.9 / 2.0 / 0.8; 1 / 0.58 / 0.30 / 0.12 | 1.40 / 0.57 / 0.30 / 0.21; 1 / 0.41 / 0.21 / 0.15 |
| Newvicon S4075 / T12 (200 nA, 25 C) | decay 50 ms (60 Hz) / 60 ms (50 Hz) | 10 / 8-13 | 10.93 / 9.17 |
| Newvicon at 45 C | decay 50 ms (60 Hz) | below 25 C by 1.2x | 6.05 (1.81x) |
| Chalnicon (200 nA, 25 C, 60 Hz) `[est]` | decay 50 ms | 10 | 10.28 |
| Vidicon (200 nA, 20 nA dark, 60 Hz) | decay 50 / 200 ms | 20 / 8 (5-12) | 15.98 / 5.32 |
| Vidicon, 20 nA bias | cut of the 50 / 200 ms lag | 13 to 9.2 (1.4x) / 3.3 to 1.2 (2.8x) | 1.30x / 2.15x |

The two Saticon sets do not agree with each other in this model. In both the capacitive law and
the traps, lag in percent of the signal can only fall as the signal rises, but the H8397A at
200 nA lags more than the BC4391 at 30 nA, and Hitachi's own 0.05 uA figure (2-3 %) is below its
0.2 uA one (`docs/CAMERA_RESEARCH.md` lists this as unexplained). The Saticon is therefore fitted
to the BC4391 family as measured and to the H8397A family by how much bias light cuts its lag,
relative to the unbiased value; its absolute H8397A level comes out lower than the data.

## Electronics

After the beam reads each row's current in nA, the line signal passes through the head
amplifier and processing amplifier (`camera_processing.rs`) in the order of US 4032974: head
amplifier (noise and band) -> clamp -> gain -> knee -> gamma -> aperture correction with
coring -> clip. See `docs/design/CAMERA_SCAN.md` ("Camera electronics") for the contract and
`docs/CAMERA_RESEARCH.md` ("Head-amplifier noise and processing lie mostly above 2.5 MHz") for
the measurements and sources. The colour matrix waits for the targets' spectral taking
characteristics, and flare and shading with their corrections wait for the optics and geometry
work (without the error they cancel, adding them would only add a fault).

**Head amplifier.** The tube acts as a current source into the input capacitance `C_in` of
tube, coil, FET and strays. The amplifier adds a noise current of one-sided spectral density
`S(f) = 2 q I + 4 k T / R_f + (2 pi f C_in)^2 4 k T R_n` (A^2/Hz): white shot noise of the
sample's own current `I` (clamped at zero), white thermal noise of the feedback resistor `R_f`,
and the FET voltage noise (`R_n`) differentiated across `C_in`. Because the FET term rises in
amplitude with frequency (the "triangular" spectrum, US 4355335), 87.5 % of the FET noise
power in a flat 0 to 5 MHz band lies above 2.5 MHz; across 15 to 30 pF over 5 MHz this gives
0.55 to 1.6 nA rms, bracketing the "about 1 nA, as low as 0.5 nA" of good preamplifiers
(US 4704635). Noise is deterministic and independent per `(seed, frame, row, channel)` via
`rng::Rng` stream `STREAM_CAMERA = 61`. Signal plus noise then pass the amplifier's band
(`band_hz = 5.5 MHz`, flat to about 5 MHz, US 3743766): the magnitude of an analog second-order
Butterworth low-pass, `|H(f)| = 1 / sqrt(1 + (f / band_hz)^4)`, delay-compensated and realised as
a 21-tap symmetric linear-phase FIR whose line ends repeat their end sample.

**Clamp and gain.** Each channel subtracts its settled dark-equilibrium current (`black`) so a
capped camera averages zero, then divides by `peak_signal_na` (150 / 300 / 150 nA for R / G / B
in the 30 mm Plumbicon) so peak white is 1.0 in linear light `L`.

**Knee.** Applied per channel on the linear signal when a knee range `R` is set (US 4470067).
Values `L <= 1` are unchanged, and highlights above `1` follow `1 + (L - 1) s` with
`s = (gamma_inverse(WHITE_CLIP) - 1) / (R - 1)` so `L = R` reaches the white clip. Without a
knee, highlights pass unchanged to gamma and clip there.

**Gamma.** Each channel passes through the unclamped 0.45 law (`gamma_correct(L)`): `4.5 L`
below `0.018` (negative excursions included, multiplying low-key noise by 4.5) and
`1.099 L^0.45 - 0.099` above (EBU Tech 3335; equal to `tube::oetf` on `[0, 1]`).

**Aperture correction and coring.** Detail is extracted from the gamma-corrected green channel
and added equally to all three channels so edges receive no colour fringes. Horizontal detail
uses delay `T` along the line, `d_h(t) = s(t) - (s(t - T) + s(t + T)) / 2`, with fractional
delays interpolated by cubic spline and end samples repeated at the line ends (BBC Engineering
91). Vertical detail uses 1H delays within the same field,
`d_v(r) = s(r) - (s(r - 2) + s(r + 2)) / 2`, with rows that lack a neighbour using their own
value (BBC Monograph 47). Coring suppresses small detail below threshold `c`,
`core(d) = sign(d) max(|d| - c, 0)`, before adding `k_h core(d_h) + k_v core(d_v)`. The
horizontal gain `k_h` restores the green channel's combined horizontal spot `M(f)` and
head-amplifier response `|H(f)|` at `f_p = 1 / (2 T)`:
`k_h = (1 / (M(f_p) |H(f_p)|) - 1) / 2`.

**Clip.** Each channel is clamped to `[BLACK_CLIP, WHITE_CLIP] = [-0.03, 1.05]` of peak white
after the detail is added, so highlight overshoots are clipped at white while dark undershoots
remain down to `-0.03`.

**Profiles.** Selected by `CameraParams::processing` (config key `processing`, default
`studio`):

| Parameter | `studio` (three-tube broadcast) | `consumer` (home camera) | Source / notes |
|---|---|---|---|
| `C_in`, `R_n`, `R_f`, `T` | 20 pF, 75 ohm, 1 Mohm, 300 K | 30 pF, 100 ohm, 470 kohm, 300 K | Published ranges 15-30 pF, 50-100 ohm, 0.3-1 Mohm (`docs/CAMERA_RESEARCH.md`); consumer at the poor end (~45 dB at 300 nA, matching PK-958 "> 45 dB") |
| Band (-3 dB) | 5.5 MHz | 5.5 MHz | Flat to about 5 MHz (US 3743766) |
| Knee range `R` | 2.3 | none `[est]` | 1984 studio practice: linear to 100 %, 230 % to white clip (US 4470067) |
| Aperture `T` | 150 ns | 200 ns | 150 ns (BBC Engineering 91); 0.2 us delay line (Panasonic) |
| Vertical gain `k_v` | 0.5 | 0 | `g = 0.25` (BBC Monograph 47) |
| Coring `c` | 0.008 `[est]` | 0.015 `[est]` | Threshold in gamma-corrected units |

## Known limits

* The slow tail with light bias is too short: at 200 ms the model leaves under 1 % where the
  XQ1410 data shows 2 to 3.5 %. That tail is the PbO photoconductive lag, which is left out
  because the data sheet calls it negligible for the plain tube; with bias it is what is left.
* Blooming by beam bending (a highlight spreading sideways) and burn-in are not modelled. The
  charge stays on its own element.
* The capacitances are fitted at 625/50 and used unchanged at 525/60; elapsed-time lag is
  checked on both raster engines.
* Only the Plumbicon variants and monochrome-style target curves (R = G = B channels for
  Vidicon, Newvicon, Saticon and Chalnicon) are modelled; spectral responses and the colour
  matrix are left for later.

## Why tails end magenta

Each colour has its own fitted capacitance and the same signal-plate voltage. Red and blue
carry half the peak current of green with half the beam on them. Their higher stored overload
and capacitance make their tails longer, and blue adds photoconductive trap lag in its layer.
Green runs out first and the tail ends in red plus blue: magenta.

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
