//! The camera pickup tube: data-sheet lag, comet tails from the beam limit, the ACT gun,
//! and the stage in the chain (off = untouched; a static scene is not restyled).

mod common;
use vhsphys_core::tube::*;
use vhsphys_core::{config, consts::*, pipeline::Session, video::Frame};

const FRAME_50: f64 = 0.04;
/// Still-scene checks: the flat greys, how many frames they are held first, the edge rows left
/// out, and the tolerances in 8-bit levels (flat: the rounding of the output; blocks: the tube's
/// gamma, about 5 levels in the shadows and mid greys, plus the spot on the finest stripes).
const STILL_GREYS: [u8; 3] = [32, 128, 200];
const SETTLE_FRAMES: usize = 20;
const EDGE_ROWS: usize = 8;
const FLAT_TOLERANCE: f64 = 1.0;
const TONE_BLOCK: usize = 16;
const TONE_TOLERANCE: f64 = 5.0;
/// Active scan time per frame at 625/50: 52/64 of the line, 575/625 of the lines.
const ACTIVE_50: f64 = 0.04 * (52.0 / 64.0) * (575.0 / 625.0);

fn chans(kind: TubeKind) -> [TubeChannel; 3] {
    kind.channels(BEAM_RATIO_DEFAULT, ACTIVE_50).expect("a tube")
}

fn within(got: f64, want: f64, rel: f64) -> bool {
    (got - want).abs() <= rel * want
}

#[test]
fn off_has_no_channels() {
    assert!(TubeKind::Off.channels(2.0, ACTIVE_50).is_none());
}

#[test]
fn operating_points_are_the_data_sheets() {
    for kind in [TubeKind::Plumbicon, TubeKind::PlumbiconBias, TubeKind::PlumbiconAct] {
        let c = chans(kind);
        for k in 0..3 {
            assert_eq!(c[k].peak_signal_na, PEAK_SIGNAL_NA[k]);
            assert!((c[k].beam_na - 2.0 * PEAK_SIGNAL_NA[k]).abs() < 1e-9);
            let sat = TARGET_CAPACITANCE_F * TARGET_VOLTAGE_V / ACTIVE_50 * 1e9;
            assert!((c[k].saturation_na - sat).abs() < 1e-6 * sat);
            assert_eq!(c[k].act, kind == TubeKind::PlumbiconAct);
            let bias = if kind == TubeKind::PlumbiconBias { LIGHT_BIAS_XQ1410_NA[k] } else { 0.0 };
            assert_eq!(c[k].bias_na, bias);
        }
    }
}

#[test]
fn decay_lag_matches_the_data_sheet() {
    for (kind, data) in [(TubeKind::Plumbicon, XQ1020_LAG), (TubeKind::PlumbiconAct, XQ1520_LAG)] {
        let c = chans(kind);
        for k in 0..3 {
            let m = lag_measurement(&c[k], FRAME_50);
            assert!(within(m.decay_60, data[k].decay_60, 0.30), "{kind:?} ch{k}: decay 60 ms {} vs {}", m.decay_60, data[k].decay_60);
            assert!(within(m.decay_200, data[k].decay_200, 0.40), "{kind:?} ch{k}: decay 200 ms {} vs {}", m.decay_200, data[k].decay_200);
        }
    }
}

/// Not fitted: the XQ1410 is the XQ1020 with light bias. The model has to predict that the
/// bias cuts the lag, and by about the data sheet's amount at 60 ms.
#[test]
fn light_bias_cuts_the_lag_as_the_xq1410_shows() {
    let plain = chans(TubeKind::Plumbicon);
    let biased = chans(TubeKind::PlumbiconBias);
    for k in 0..3 {
        let a = lag_measurement(&plain[k], FRAME_50);
        let b = lag_measurement(&biased[k], FRAME_50);
        assert!(b.decay_60 < a.decay_60 && b.decay_200 < a.decay_200, "ch{k}: {a:?} -> {b:?}");
        assert!(b.build_60 > a.build_60, "ch{k}: {a:?} -> {b:?}");
        assert!(within(b.decay_60, XQ1410_LAG[k].decay_60, 0.35), "ch{k}: {} vs {}", b.decay_60, XQ1410_LAG[k].decay_60);
    }
}

#[test]
fn a_steady_scene_reads_back_its_light() {
    let c = chans(TubeKind::Plumbicon);
    for k in 0..3 {
        let mut q = dark_equilibrium(&c[k]);
        let photo = photo_current(&c[k], 0.5);
        let mut s = 0.0;
        for _ in 0..200 {
            s = frame_step(&c[k], &mut q, photo);
        }
        // in the steady state the beam removes what the frame added
        let added = photo + c[k].dark_na + c[k].bias_na;
        assert!((s - added).abs() < 1e-6 * added.max(1.0), "ch{k}: read {s}, added {added}");
    }
}

/// Signal of channel `k` per frame: `on` frames of `light`, then darkness.
fn after_highlight(ch: &TubeChannel, light: f64, on: usize, dark: usize) -> Vec<f64> {
    let mut q = dark_equilibrium(ch);
    let p = photo_current(ch, light);
    for _ in 0..on {
        frame_step(ch, &mut q, p);
    }
    (0..dark).map(|_| frame_step(ch, &mut q, 0.0)).collect()
}

/// Frames after the highlight has gone that still read above peak white (clipped white).
fn tail_frames(ch: &TubeChannel, light: f64, on: usize) -> usize {
    after_highlight(ch, light, on, 200).iter().take_while(|&&s| s > ch.peak_signal_na).count()
}

#[test]
fn a_highlight_beyond_the_beam_leaves_a_comet_tail() {
    let g = chans(TubeKind::Plumbicon)[1];
    let s = after_highlight(&g, 4.0, 20, 5);
    assert!(s[0] > 0.95 * g.beam_na, "first dark frame reads {} (beam {})", s[0], g.beam_na);
    assert!(tail_frames(&g, 4.0, 20) >= 3);
    // within the beam's reach there is only lag
    assert_eq!(tail_frames(&g, 1.0, 20), 0);
    assert!(after_highlight(&g, 1.0, 20, 1)[0] < 0.5 * g.peak_signal_na);
}

#[test]
fn the_tail_grows_with_the_overload_and_shrinks_with_the_beam() {
    let g = chans(TubeKind::Plumbicon)[1];
    let t: Vec<usize> = [2.0, 4.0, 8.0].iter().map(|&l| tail_frames(&g, l, 30)).collect();
    assert!(t[0] <= t[1] && t[1] <= t[2] && t[0] < t[2], "{t:?}");
    let strong = TubeKind::Plumbicon.channels(3.0, ACTIVE_50).unwrap()[1];
    assert!(tail_frames(&strong, 4.0, 30) < tail_frames(&g, 4.0, 30));
    // the stored charge stops at the signal-plate voltage
    let mut q = dark_equilibrium(&g);
    for _ in 0..500 {
        frame_step(&g, &mut q, photo_current(&g, 100.0));
    }
    assert!(q <= g.saturation_na + 1e-9);
}

#[test]
fn the_act_gun_leaves_no_tail() {
    let g = chans(TubeKind::PlumbiconAct)[1];
    assert_eq!(tail_frames(&g, 8.0, 30), 0);
    assert!(after_highlight(&g, 8.0, 30, 1)[0] < 0.25 * g.peak_signal_na);
}

/// The red and blue tubes carry half the peak current with half the beam on the same
/// target, so they store twice the overload per unit of beam: a white highlight's tail
/// outlasts on red and blue (it ends magenta).
#[test]
fn red_and_blue_tails_outlast_green() {
    let c = chans(TubeKind::Plumbicon);
    let (r, g, b) = (tail_frames(&c[0], 4.0, 40), tail_frames(&c[1], 4.0, 40), tail_frames(&c[2], 4.0, 40));
    assert!(r > g && b > g, "R {r} G {g} B {b}");
}

// ------------------------------------------------------------------ in the chain

fn square_at(cx: usize) -> Frame {
    let mut f = Frame::new(ACTIVE_W, ACTIVE_H);
    for y in ACTIVE_H / 2 - 16..ACTIVE_H / 2 + 16 {
        for x in cx - 16..cx + 16 {
            let i = (y * ACTIVE_W + x) * 3;
            f.rgb[i..i + 3].fill(255);
        }
    }
    f
}

/// A white square held for 8 frames, then moved right; the luma at the old position in the
/// first frame lit only by the new picture and two frames later. Each line integrates the light
/// of the frame period before it is read, so frame 8 still holds the square's own light from
/// picture 7: the trail is measured from frame 9.
fn moved_square(camera: &str) -> [f64; 2] {
    let p = config::build(Some("pristine"), camera, 3).unwrap();
    let mut input: Vec<Frame> = (0..8).map(|_| square_at(200)).collect();
    input.extend((0..4).map(|_| square_at(500)));
    let out = Session::run(p, &input);
    [9, 11].map(|k| {
        let f = &out[k];
        let mut acc = 0.0;
        for y in ACTIVE_H / 2 - 8..ACTIVE_H / 2 + 8 {
            for x in 192..208 {
                let i = (y * f.w + x) * 3;
                acc += 0.299 * f.rgb[i] as f64 + 0.587 * f.rgb[i + 1] as f64 + 0.114 * f.rgb[i + 2] as f64;
            }
        }
        acc / 256.0
    })
}

/// Overexposed by two stops, the square's charge outlives it at clipped white; the ACT gun
/// removes that, leaving only the lag, which is faint two frames later.
#[test]
fn an_overexposed_highlight_leaves_a_trail_on_tape_and_the_act_gun_removes_it() {
    let off = moved_square("");
    let tube = moved_square("[camera]\ntube = \"plumbicon\"\niris_stops = 2.0\n");
    let act = moved_square("[camera]\ntube = \"plumbicon-act\"\niris_stops = 2.0\n");
    for k in 0..2 {
        assert!(tube[k] > off[k] + 40.0, "frame {k}: trail {} vs no tube {}", tube[k], off[k]);
    }
    assert!(act[0] < tube[0] - 40.0, "ACT {} vs plain {}", act[0], tube[0]);
    assert!(act[1] < off[1] + 15.0, "ACT {} vs no tube {}", act[1], off[1]);
}

/// A still flat grey comes out of the camera at the level the tube's gamma of 0.95 gives it.
/// The rows at the raster edge are left out: the beam's wings there find no neighbouring line.
/// Measured at the camera's output: the tape chain's noise decorrelates under any change of its
/// input, so a whole-chain comparison cannot isolate the camera.
#[test]
fn a_still_flat_grey_keeps_its_level_apart_from_the_tube_gamma() {
    let params = vhsphys_core::camera::CameraParams { tube: TubeKind::Plumbicon, ..Default::default() };
    for grey in STILL_GREYS {
        let mut cam = vhsphys_core::camera::CameraTube::new(&params).unwrap();
        let mut input = Frame::new(ACTIVE_W, ACTIVE_H);
        input.rgb.fill(grey);
        let mut out = cam.process_frame(&input);
        for _ in 0..SETTLE_FRAMES {
            out = cam.process_frame(&input);
        }
        let want = 255.0 * oetf(oetf_inverse(grey as f64 / 255.0).powf(GAMMA));
        let inner = &out.rgb[EDGE_ROWS * ACTIVE_W * 3..(ACTIVE_H - EDGE_ROWS) * ACTIVE_W * 3];
        let got = inner.iter().map(|&v| v as f64).sum::<f64>() / inner.len() as f64;
        assert!((got - want).abs() < FLAT_TOLERANCE, "grey {grey}: {got} vs {want}");
    }
}

/// A still scene at normal exposure keeps its tones: averaged over blocks the camera's output is
/// its input, apart from the tube's gamma and the spot, which softens the finest stripes in
/// linear light before the gamma corrector and so lifts them a little.
#[test]
fn a_still_scene_at_normal_exposure_is_not_restyled() {
    let params = vhsphys_core::camera::CameraParams { tube: TubeKind::Plumbicon, ..Default::default() };
    let mut cam = vhsphys_core::camera::CameraTube::new(&params).unwrap();
    let input = vhsphys_core::video::test_pattern();
    let mut out = cam.process_frame(&input);
    for _ in 0..SETTLE_FRAMES {
        out = cam.process_frame(&input);
    }
    let (mut err, mut n) = (0.0, 0.0);
    for by in (0..=input.h - TONE_BLOCK).step_by(TONE_BLOCK) {
        for bx in (0..=input.w - TONE_BLOCK).step_by(TONE_BLOCK) {
            for c in 0..3 {
                let (mut a, mut b) = (0.0, 0.0);
                for y in by..by + TONE_BLOCK {
                    for x in bx..bx + TONE_BLOCK {
                        let i = (y * input.w + x) * 3 + c;
                        a += input.rgb[i] as f64;
                        b += out.rgb[i] as f64;
                    }
                }
                err += (a - b).abs() / (TONE_BLOCK * TONE_BLOCK) as f64;
                n += 1.0;
            }
        }
    }
    let d = err / n;
    assert!(d < TONE_TOLERANCE, "mean abs difference of the block means {d}");
}
