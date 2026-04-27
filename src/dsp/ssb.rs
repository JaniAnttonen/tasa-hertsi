use std::f32::consts::TAU;

use super::hilbert::HilbertFir;

/// Time-domain single-sideband frequency shifter (Bode-style).
///
/// For each channel, builds the analytic signal `(real, imag)` via a Hilbert
/// FIR, then multiplies by a complex carrier `e^{jωt}`:
///
///   upper = real·cos(ωt) − imag·sin(ωt)   // shift by +Δf
///   lower = real·cos(ωt) + imag·sin(ωt)   // shift by −Δf
///
/// Both sidebands are mixed back together by `upper_level` and `lower_level`,
/// allowing pure SSB, pure ring-mod-style DSB, or any blend in between.
///
/// Stereo: the left channel is shifted by `shift + spread/2`, the right by
/// `shift − spread/2`. With non-zero spread you get a slow stereo chorus.
pub struct SsbShifter {
    sample_rate: f32,
    h_l: HilbertFir,
    h_r: HilbertFir,
    phase_l: f32,
    phase_r: f32,
    fb_l: f32,
    fb_r: f32,
}

impl SsbShifter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            h_l: HilbertFir::new(63),
            h_r: HilbertFir::new(63),
            phase_l: 0.0,
            phase_r: 0.0,
            fb_l: 0.0,
            fb_r: 0.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        if (sr - self.sample_rate).abs() > 0.5 {
            self.sample_rate = sr;
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.h_l.reset();
        self.h_r.reset();
        self.phase_l = 0.0;
        self.phase_r = 0.0;
        self.fb_l = 0.0;
        self.fb_r = 0.0;
    }

    pub fn process_stereo(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &SsbParams,
    ) {
        let shift_l = p.shift_hz + p.stereo_spread_hz * 0.5;
        let shift_r = p.shift_hz - p.stereo_spread_hz * 0.5;
        let dphi_l = TAU * shift_l / self.sample_rate;
        let dphi_r = TAU * shift_r / self.sample_rate;
        let fb = p.feedback.clamp(0.0, 0.92);
        let n = in_l.len();

        for i in 0..n {
            let l_in = in_l[i] + fb * self.fb_l;
            let r_in = in_r[i] + fb * self.fb_r;

            let (rl, il) = self.h_l.process(l_in);
            let (rr, ir) = self.h_r.process(r_in);

            let (sl_sin, sl_cos) = self.phase_l.sin_cos();
            let (sr_sin, sr_cos) = self.phase_r.sin_cos();

            let up_l = rl * sl_cos - il * sl_sin;
            let lo_l = rl * sl_cos + il * sl_sin;
            let up_r = rr * sr_cos - ir * sr_sin;
            let lo_r = rr * sr_cos + ir * sr_sin;

            let wet_l = p.upper_level * up_l + p.lower_level * lo_l;
            let wet_r = p.upper_level * up_r + p.lower_level * lo_r;

            // Use the group-delayed input as "dry" so wet/dry stay aligned.
            let dry_l = rl;
            let dry_r = rr;

            out_l[i] = (1.0 - p.mix) * dry_l + p.mix * wet_l;
            out_r[i] = (1.0 - p.mix) * dry_r + p.mix * wet_r;

            // Soft-clip the feedback path so it stays bounded at high feedback.
            self.fb_l = soft_clip(wet_l);
            self.fb_r = soft_clip(wet_r);

            self.phase_l = wrap(self.phase_l + dphi_l);
            self.phase_r = wrap(self.phase_r + dphi_r);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SsbParams {
    pub shift_hz: f32,
    pub stereo_spread_hz: f32,
    pub upper_level: f32,
    pub lower_level: f32,
    pub feedback: f32,
    pub mix: f32,
}

impl Default for SsbParams {
    fn default() -> Self {
        Self {
            shift_hz: 80.0,
            stereo_spread_hz: 0.0,
            upper_level: 1.0,
            lower_level: 0.0,
            feedback: 0.0,
            mix: 1.0,
        }
    }
}

#[inline]
fn wrap(mut x: f32) -> f32 {
    while x > TAU {
        x -= TAU;
    }
    while x < -TAU {
        x += TAU;
    }
    x
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    // tanh-like saturator, cheap and bounded.
    let y = x.clamp(-3.0, 3.0);
    y - y * y * y * (1.0 / 27.0)
}
