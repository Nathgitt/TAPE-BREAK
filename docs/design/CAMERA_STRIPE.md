# CONTRACT

# The stripe colour camera

Part of the camera design (`docs/design/CAMERA_SCAN.md`).

## Optics and decoder

Source: `docs/CAMERA_RESEARCH.md`, "Single-tube camcorders trade colour bandwidth for a striped
carrier" (Panasonic WV-3110, frequency separation with cyan and yellow stripes). One tube sees the
scene through a stripe filter on its faceplate; scanning the stripes turns colour into a carrier on
the line signal, and the camera's decoder takes colour back out of it. This stage builds the optics
and the decoder as their own module; joining them to the scanned target and the processing amplifier
is the next stage.

- **Carrier.** `f_c = w / (p t)` with `w = 7.49 mm`, `p = 40 um`, `t = 52.3 us`: `STRIPE_CARRIER_HZ =
  3.58e6`, the same in both engines (a home camera's stripes are fixed in its optics).
- **Stripes.** Cyan stripes block red, yellow stripes block blue, green passes both. The beam reads
  only the stripes' fundamental (the harmonics lie far above its spot), so the filter is
  `T_G = 1`, `T_R = (1 + cos th_R) / 2`, `T_B = (1 + cos th_B) / 2` with
  `th = 2 pi f_c x / FS_BB + phi(r)`. The two stripe sets slant oppositely: `phi_R(r) = +pi r / 4`,
  `phi_B(r) = -pi r / 4` over active frame rows `r`, so from one line of a field to the next (two frame
  rows) red's carrier turns +90 deg and blue's -90 deg. The element's light is `sum_c T_c L_c`.
- **Optical low-pass.** A birefringent plate before the stripes splits the image into two copies
  displaced along the line by half a carrier period, `d = 1 / (2 f_c)` of line time:
  `L'(x) = (L(x - d/2) + L(x + d/2)) / 2` per colour, with fractional positions interpolated on the row
  and the row ends repeating their end sample. Its response `cos(pi f d)` is zero at the carrier, so
  scene detail there makes no false colour, at a cost in sharpness. This acts on the scene's light before
  the target, as the optics do.
- **Decoder** (on the clamped line signal `s`, one active row at a time, filters linear-phase with
  their delay compensated, row ends repeating their end sample):
  1. Chroma `c = BP(s)`: Gaussian band-pass at `f_c`, -3 dB at `f_c +- 0.5 MHz`.
     Luminance `y = LP_2.8(s - c)`: the trap is the band-pass's complement, then a Gaussian low-pass
     with -3 dB at 2.8 MHz.
  2. 1H separation. With `c_0` this row's chroma, `c_1` the chroma of the previous line of the same
     field (frame row `r - 2`; blank before a field's first line) and `D` a delay of a quarter carrier
     period (`1 / (4 f_c)`, fractional positions interpolated): red carrier `D(c_0) + c_1`, blue carrier
     `D(c_0) - c_1`.
  3. Detection. Each carrier's envelope is the magnitude of its analytic signal (the carrier and its
     Hilbert transform, a linear-phase FIR), times `carrier_gain`, then a Gaussian low-pass with -3 dB at
     0.5 MHz: `R_c`, `B_c`. (A rectifier would alias: at 525 lines the carrier is a quarter of the sample
     rate.)
  4. Matrix with mixed highs. `y_c = LP_0.5(y)`, `G_c = y_c - (R_c + B_c) / 2`, `h = (y - y_c) / 2`
     (white light of 1 in every colour gives `y = 2`, so halving puts the highs on the channels' scale);
     the outputs are `R_c + h`, `G_c + h`, `B_c + h`. A flat field of light `(R, G, B)` decodes to
     `(R, G, B)`, and a grey edge to a grey edge.
  `carrier_gain` is the camera's alignment: the reciprocal of the response of the read (spot and head
  amplifier) at the carrier, set by whoever joins the decoder to a tube; 1 for an ideal read.
- **What follows from it** (and the tests check): colour about 0.5 MHz wide, luminance 2.8 MHz; a
  colour change between lines mixes red and blue on the line after it; fine detail near the carrier
  makes false colour without the optical low-pass; a target that cannot follow the carrier's peaks (the
  beam limit) loses carrier and the highlight turns green.

Interfaces, in a new engine module `stripe.rs`:

```rust
pub const STRIPE_CARRIER_HZ: f64 = 3.58e6;
pub const LUMA_BAND_HZ: f64 = 2.8e6;
pub const COLOUR_BAND_HZ: f64 = 0.5e6;
pub const CHROMA_HALF_WIDTH_HZ: f64 = 0.5e6;
/// Stripe transmittances `[R, G, B]` at sample `x` of active frame row `r`.
pub fn stripe_transmittance(r: usize, x: usize) -> [f64; 3];
/// One colour of one row of scene light behind the optical low-pass.
pub fn optical_lowpass(row: &[f64]) -> Vec<f64>;
/// The light reaching the target at `(r, x)` from scene light `rgb` there: `sum_c T_c rgb_c`.
pub fn target_light(r: usize, x: usize, rgb: [f64; 3]) -> f64;
pub struct StripeDecoder { .. }
impl StripeDecoder {
    pub fn new(carrier_gain: f64) -> Self;
    /// Decode a frame of clamped line signals (`rows[r][x]`, active rows in frame order) into
    /// `[R, G, B]` rows on the same scale.
    pub fn decode(&self, rows: &[Vec<f32>]) -> Vec<[Vec<f32>; 3]>;
}
```

