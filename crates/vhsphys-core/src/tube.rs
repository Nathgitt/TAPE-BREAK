//! Camera pickup tube: the photoconductive target of a three-tube Plumbicon camera, the
//! source of the lag and the comet tails of 1970s and 1980s studio and outside-broadcast
//! video (the "trails" behind lamps and highlights).
//!
//! Each element of the target is a small capacitor. Light discharges it through the
//! photoconductor between scans (here: adds charge, counted positive); the scanning beam
//! recharges it to the cathode potential, and the charge it delivers is the signal. Two
//! physical limits of the beam make the picture remember the past:
//!
//! * **Discharge (capacitive) lag.** Near the cathode potential only the faster electrons
//!   of the beam can land (their energies are spread thermally, kT/e of a few tenths of a
//!   volt), so the landing current falls exponentially as the element approaches it:
//!   `i = I_b exp(u / V_T)` for `u <= 0`, `i = I_b` above. An element left slightly
//!   charged after a scan gives up the rest over the next scans. In target-equivalent nA s
//!   the scan of one element is, exactly:
//!     1. above the cathode potential the full beam lands: `r1 = min(q, I_b)`, `q -= r1`;
//!     2. with the beam charge left, `I_b - r1`, below it: `exp(-q'/K) = exp(-q/K) + (I_b - r1)/K`
//!   where `K = C V_T`. The plate limit is `C V_target`; the signal is charge removed.
//!   Light bias adds a steady charge that keeps elements off the slow part of the curve.
//! * **Comet tails.** The beam is set to twice what peak white needs (data sheet). A
//!   highlight beyond that leaves charge the scan cannot remove; it accumulates (up to the
//!   point where the element reaches the signal-plate voltage and photoconduction stops,
//!   `C V_target`) and drains at the beam current after the highlight has
//!   gone. An anti-comet-tail (ACT) gun recharges, during line flyback, every element above
//!   the level the read beam can stabilise, so nothing is left over.
//!
//! The target capacitance per channel is fitted on the scanned engine to handbook lag.
//! Source: Philips Data Handbook T10, Plumbicon camera tubes, 1985 (XQ1020, XQ1410, XQ1520),
//! lag at Is/Ib = 20/300 nA, low key, 50 Hz. See docs/CAMERA.md.

use serde::{Deserialize, Serialize};

/// Gamma of the light transfer characteristic (all three types: 0.95 +/- 0.05).
pub const GAMMA: f64 = 0.95;
/// The camera's gamma corrector follows the BT.709 transfer: linear (slope 4.5) below
/// 0.018, then `1.099 L^0.45 - 0.099`. The input video is taken through its inverse to get
/// the scene light and the tube's signal through it again, so a tube with gamma 1 and no
/// lag returns the input.
pub fn oetf(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l < 0.018 { 4.5 * l } else { 1.099 * l.powf(0.45) - 0.099 }
}

/// Inverse of `oetf`.
pub fn oetf_inverse(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v < 0.081 { v / 4.5 } else { ((v + 0.099) / 1.099).powf(1.0 / 0.45) }
}
/// Peak-white signal current per channel `[R, G, B]`, nA (30 mm tubes).
pub const PEAK_SIGNAL_NA: [f64; 3] = [150.0, 300.0, 150.0];
/// Beam current as a multiple of what peak white needs, as the data sheet sets it.
pub const BEAM_RATIO_DEFAULT: f64 = 2.0;
/// Dark current, nA. The data sheets give maxima of 1 to 3 nA. [est]
pub const DARK_NA: f64 = 1.0;
/// Internal light bias of the XQ1410, nA per channel `[R, G, B]` (data sheet setting).
pub const LIGHT_BIAS_XQ1410_NA: [f64; 3] = [3.0, 2.0, 3.5];
/// Signal-plate (target) voltage, V.
pub const TARGET_VOLTAGE_V: f64 = 45.0;
/// Triode gun beam temperature of 1900 to 2350 K with crossover, US 4,388,556.
pub const V_T_TRIODE_GUN_V: f64 = 0.17;
/// Diode gun beam temperature of about 1200 K, US 4,388,556.
pub const V_T_DIODE_GUN_V: f64 = 0.10;
/// Fitted target capacitance per colour `[R, G, B]`, F; see docs/CAMERA.md.
pub const TARGET_CAPACITANCE_F: [f64; 3] = [1.38e-9, 1.15e-9, 1.76e-9];
/// Vertical beam spot width (Gaussian sigma) in frame-line pitches [est].
/// Source: `docs/CAMERA_RESEARCH.md` ("Interlaced readout"), 0.55 to 0.73 across tube formats,
/// derived from Philips T10 and RCA 1977 400 TVL modulation depths.
pub const SIGMA_V_LINES: f64 = 0.65;
/// Horizontal beam spot width (Gaussian sigma) in seconds [est].
/// Source: `docs/CAMERA_RESEARCH.md` ("Interlaced readout"), 36 to 44 ns at 52 us active scan
/// for 30 mm Plumbicon tubes (XQ1520 ACT 36 ns, XQ1020 G 44 ns).
pub const SIGMA_H_S: f64 = 40e-9;
/// Which camera tube the picture was shot through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TubeKind {
    /// No camera stage: the input is the video signal itself.
    #[default]
    Off,
    /// Plumbicon, standard gun, no light bias (XQ1020): lag and comet tails.
    Plumbicon,
    /// Plumbicon with internal light bias (XQ1410): less lag, the same comet tails.
    PlumbiconBias,
    /// Plumbicon with an anti-comet-tail gun (XQ1520): lag, no comet tails.
    PlumbiconAct,
}

/// One tube of the camera (one colour channel) at its operating point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TubeChannel {
    /// Signal current at peak white, nA.
    pub peak_signal_na: f64,
    /// Beam current, nA.
    pub beam_na: f64,
    /// Target capacitance, F.
    pub capacitance_f: f64,
    /// Beam temperature voltage, V.
    pub v_t: f64,
    /// Charge constant `K = C V_T` in target-equivalent nA s.
    pub k_nas: f64,
    /// Vertical beam spot Gaussian sigma in frame-line pitches.
    pub sigma_v_lines: f64,
    /// Horizontal beam spot Gaussian sigma in seconds.
    pub sigma_h_s: f64,
    /// Plate limit `C * TARGET_VOLTAGE_V`, nA s.
    pub plate_nas: f64,
    pub dark_na: f64,
    pub bias_na: f64,
    /// Anti-comet-tail gun.
    pub act: bool,
}

impl TubeKind {
    /// The three channels `[R, G, B]`, or `None` for `Off`. `beam_ratio` scales the beam
    /// current to that multiple of each channel's peak-white current.
    pub fn channels(self, beam_ratio: f64) -> Option<[TubeChannel; 3]> {
        if self == TubeKind::Off { return None; }
        Some(std::array::from_fn(|k| TubeChannel {
            peak_signal_na: PEAK_SIGNAL_NA[k],
            beam_na: beam_ratio * PEAK_SIGNAL_NA[k],
            capacitance_f: TARGET_CAPACITANCE_F[k],
            v_t: V_T_TRIODE_GUN_V,
            k_nas: TARGET_CAPACITANCE_F[k] * V_T_TRIODE_GUN_V * 1e9,
            sigma_v_lines: SIGMA_V_LINES,
            sigma_h_s: SIGMA_H_S,
            plate_nas: TARGET_CAPACITANCE_F[k] * TARGET_VOLTAGE_V * 1e9,
            dark_na: DARK_NA,
            bias_na: if self == TubeKind::PlumbiconBias { LIGHT_BIAS_XQ1410_NA[k] } else { 0.0 },
            act: self == TubeKind::PlumbiconAct,
        }))
    }
}

/// Photo current for a linear scene light (1.0 = the light that gives peak white), nA.
pub fn photo_current(ch: &TubeChannel, light: f64) -> f64 {
    ch.peak_signal_na * light.max(0.0).powf(GAMMA)
}

/// The scan of one element holding charge `q`: the charge it holds afterwards.
pub fn scan(beam: f64, scale: f64, q: f64) -> f64 {
    if beam <= 0.0 {
        return q;
    }
    // above the cathode potential the whole beam lands, up to the charge there is
    let landed = q.clamp(0.0, beam);
    let q = q - landed;
    let left = beam - landed;
    if left <= 0.0 {
        return q;
    }
    // below it, exp(-q'/K) = exp(-q/K) + left/K
    let x = -q / scale;
    if x < 700.0 {
        return -scale * (x.exp() + left / scale).ln();
    }
    // an element left far below the cathode potential would overflow the exponential: the
    // same sum as a log-sum-exp
    let y = (left / scale).ln();
    let m = x.max(y);
    -scale * (m + ((x - m).exp() + (y - m).exp()).ln())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_element_far_below_the_cathode_potential_still_scans() {
        let ch = TubeKind::Plumbicon.channels(BEAM_RATIO_DEFAULT).unwrap()[1];
        // exp(-q/K) is out of range here; the beam takes the element to about -K ln(beam/K)
        let after = scan(ch.beam_na, ch.k_nas, -10_000.0);
        assert!(after.is_finite() && after <= -10_000.0 && after > -10_001.0, "{after}");
    }

    #[test]
    fn the_scan_removes_all_the_charge_the_beam_can_reach() {
        // a charge below the beam is taken off entirely, and the rest of the beam only pushes
        // the element a few K below the cathode potential
        let after = scan(600.0, 5.64, 100.0);
        assert!(after < 0.0 && after > -40.0, "{after}");
        // a charge above the beam loses exactly the beam
        assert_eq!(scan(600.0, 5.64, 1000.0), 400.0);
    }
}
