use super::spectral::{SpecParams, SpectralShifter};
use super::ssb::{SsbParams, SsbShifter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineMode {
    Ssb,
    Spectral,
}

impl EngineMode {
    pub fn label(self) -> &'static str {
        match self {
            EngineMode::Ssb => "SSB (time-domain)",
            EngineMode::Spectral => "Spectral (FFT)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Params {
    pub mode: EngineMode,
    pub ssb: SsbParams,
    pub spec: SpecParams,
    pub output_gain: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            mode: EngineMode::Ssb,
            ssb: SsbParams::default(),
            spec: SpecParams::default(),
            output_gain: 0.9,
        }
    }
}

pub struct Engine {
    sample_rate: f32,
    ssb: SsbShifter,
    spec: SpectralShifter,
    last_mode: EngineMode,
}

impl Engine {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            ssb: SsbShifter::new(sample_rate),
            spec: SpectralShifter::new(sample_rate, 2),
            last_mode: EngineMode::Ssb,
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        if (sr - self.sample_rate).abs() > 0.5 {
            self.sample_rate = sr;
            self.ssb.set_sample_rate(sr);
            self.spec.set_sample_rate(sr);
        }
    }

    /// Process a stereo block in-place-ish: input → output, both length-N.
    pub fn process_stereo(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        params: &Params,
    ) {
        // Reset the inactive engine when the user toggles to avoid clicks
        // when they switch back later with a residual ring buffer.
        if params.mode != self.last_mode {
            match self.last_mode {
                EngineMode::Ssb => self.ssb.reset(),
                EngineMode::Spectral => self.spec.reset(),
            }
            self.last_mode = params.mode;
        }

        match params.mode {
            EngineMode::Ssb => {
                self.ssb
                    .process_stereo(in_l, in_r, out_l, out_r, &params.ssb);
            }
            EngineMode::Spectral => {
                self.spec
                    .process_stereo(in_l, in_r, out_l, out_r, &params.spec);
            }
        }

        let g = params.output_gain;
        for s in out_l.iter_mut().chain(out_r.iter_mut()) {
            *s *= g;
            // Final safety clip — keeps a runaway feedback bomb out of the
            // user's speakers without affecting normal level material.
            if *s > 1.0 {
                *s = 1.0;
            } else if *s < -1.0 {
                *s = -1.0;
            }
        }
    }
}
