use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecMode {
    /// Shift every frequency bin by the same Hz amount.
    Linear,
    /// Shift only frequencies above the cutoff. Below stays put.
    Highpass,
    /// Shift only frequencies below the cutoff. Above stays put.
    Lowpass,
}

impl SpecMode {
    pub fn label(self) -> &'static str {
        match self {
            SpecMode::Linear => "linear",
            SpecMode::Highpass => "highpass",
            SpecMode::Lowpass => "lowpass",
        }
    }
}

/// Hardcoded FFT size (powers of 2 are friendly for `rustfft`).
pub const FFT_SIZE: usize = 2048;
pub const HOP_OPTIONS: &[usize] = &[64, 128, 256, 512, 1024];

#[derive(Clone, Copy, Debug)]
pub struct SpecParams {
    pub shift_hz: f32,
    pub mode: SpecMode,
    pub cutoff_hz: f32,
    pub mix: f32,
    pub hop_size: usize,
}

impl Default for SpecParams {
    fn default() -> Self {
        Self {
            shift_hz: 80.0,
            mode: SpecMode::Linear,
            cutoff_hz: 1000.0,
            mix: 1.0,
            hop_size: 512,
        }
    }
}

/// FFT-based frequency shifter with frequency-dependent shift profiles.
///
/// Streaming STFT (analysis Hann × synthesis Hann, hop = N/4) with overlap-add
/// reconstruction. For each output bin k, the input is interpolated from
/// bin (k − Δf · N / fs); per-bin phase is compensated across hops so the
/// shift sounds smooth even when fractional or frequency-dependent.
///
/// Hermitian symmetry is enforced on the negative-frequency half so the IFFT
/// produces a real-valued time-domain signal.
pub struct SpectralShifter {
    sample_rate: f32,
    fft_size: usize,
    hop_size: usize,
    fft_fwd: Arc<dyn Fft<f32>>,
    fft_inv: Arc<dyn Fft<f32>>,
    win_a: Vec<f32>,
    win_s: Vec<f32>,
    cola_scale: f32,
    chans: Vec<SpecChannel>,
}

struct SpecChannel {
    in_ring: Vec<f32>,
    in_pos: usize,
    hop_counter: usize,
    out_ring: Vec<f32>,
    read_pos: usize,
    phase_acc: Vec<f32>, // length n_pos = fft_size/2 + 1
    fft_in: Vec<Complex<f32>>,
    fft_out: Vec<Complex<f32>>,
    dry_delay: Vec<f32>,
    dry_pos: usize,
}

impl SpectralShifter {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let fft_size = FFT_SIZE;
        let hop_size = 512usize;

        let mut planner = FftPlanner::<f32>::new();
        let fft_fwd = planner.plan_fft_forward(fft_size);
        let fft_inv = planner.plan_fft_inverse(fft_size);

        let win_a: Vec<f32> = (0..fft_size)
            .map(|n| 0.5 - 0.5 * (TAU * n as f32 / fft_size as f32).cos())
            .collect();
        let win_s = win_a.clone();

        let cola_scale = compute_cola(&win_a, &win_s, hop_size);

        let chans = (0..channels).map(|_| SpecChannel::new(fft_size)).collect();

        Self {
            sample_rate,
            fft_size,
            hop_size,
            fft_fwd,
            fft_inv,
            win_a,
            win_s,
            cola_scale,
            chans,
        }
    }

    pub fn set_hop_size(&mut self, hop: usize) {
        let hop = hop.clamp(16, self.fft_size);
        if hop == self.hop_size {
            return;
        }
        self.hop_size = hop;
        self.cola_scale = compute_cola(&self.win_a, &self.win_s, hop);
        // Reset all per-channel state — a hop change invalidates the OLA
        // alignment, the hop counter, and the per-bin phase accumulators.
        for c in &mut self.chans {
            c.reset();
        }
    }

    pub fn hop_size(&self) -> usize {
        self.hop_size
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        if (sr - self.sample_rate).abs() > 0.5 {
            self.sample_rate = sr;
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        for c in &mut self.chans {
            c.reset();
        }
    }

    pub fn process_stereo(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &SpecParams,
    ) {
        // Apply any pending hop change before we touch a sample.
        self.set_hop_size(p.hop_size);

        let n = in_l.len();
        for i in 0..n {
            let dry_l = self.push_input(0, in_l[i]);
            if self.chans[0].hop_counter == 0 {
                self.run_hop(0, p);
            }
            let wet_l = self.read_output(0);
            out_l[i] = (1.0 - p.mix) * dry_l + p.mix * wet_l;

            let dry_r = self.push_input(1, in_r[i]);
            if self.chans[1].hop_counter == 0 {
                self.run_hop(1, p);
            }
            let wet_r = self.read_output(1);
            out_r[i] = (1.0 - p.mix) * dry_r + p.mix * wet_r;
        }
    }

    fn push_input(&mut self, ch: usize, x: f32) -> f32 {
        let n = self.fft_size;
        let hop = self.hop_size;
        let chan = &mut self.chans[ch];
        chan.in_ring[chan.in_pos] = x;
        chan.in_pos = (chan.in_pos + 1) % n;

        // Dry path delayed to roughly align with the wet (the OLA latency is
        // ~ fft_size − hop_size samples).
        let delay_len = chan.dry_delay.len();
        chan.dry_delay[chan.dry_pos] = x;
        let dry_read = (chan.dry_pos + delay_len - (n - hop)) % delay_len;
        let dry = chan.dry_delay[dry_read];
        chan.dry_pos = (chan.dry_pos + 1) % delay_len;

        chan.hop_counter += 1;
        if chan.hop_counter >= hop {
            chan.hop_counter = 0; // signal: caller should run a hop now
        }
        dry
    }

    fn read_output(&mut self, ch: usize) -> f32 {
        let n = self.fft_size;
        let chan = &mut self.chans[ch];
        let s = chan.out_ring[chan.read_pos];
        chan.out_ring[chan.read_pos] = 0.0;
        chan.read_pos = (chan.read_pos + 1) % n;
        s
    }

    fn run_hop(&mut self, ch: usize, p: &SpecParams) {
        let Self {
            sample_rate,
            fft_size,
            hop_size,
            fft_fwd,
            fft_inv,
            win_a,
            win_s,
            cola_scale,
            chans,
            ..
        } = self;
        let n = *fft_size;
        let hop = *hop_size;
        let sr = *sample_rate;
        let cola = *cola_scale;
        let n_pos = n / 2 + 1;
        let bin_to_hz = sr / n as f32;
        let hop_phase_factor = TAU * hop as f32 / sr;

        let chan = &mut chans[ch];

        // Build windowed analysis frame, oldest sample first.
        for k in 0..n {
            let idx = (chan.in_pos + k) % n;
            chan.fft_in[k] = Complex::new(chan.in_ring[idx] * win_a[k], 0.0);
        }
        fft_fwd.process(&mut chan.fft_in);

        // Frequency-shift the positive half, with per-bin phase compensation.
        for k in 0..n_pos {
            let bin_freq = k as f32 * bin_to_hz;
            let shift_k = match p.mode {
                SpecMode::Linear => p.shift_hz,
                SpecMode::Highpass => {
                    if bin_freq > p.cutoff_hz {
                        p.shift_hz
                    } else {
                        0.0
                    }
                }
                SpecMode::Lowpass => {
                    if bin_freq < p.cutoff_hz {
                        p.shift_hz
                    } else {
                        0.0
                    }
                }
            };
            let bin_shift = shift_k / bin_to_hz; // shift in (real-valued) bins
            let src = k as f32 - bin_shift;

            let interp = if src < 0.0 || src > (n_pos - 1) as f32 {
                Complex::new(0.0, 0.0)
            } else {
                let i_floor = src.floor() as usize;
                let alpha = src - i_floor as f32;
                let i_ceil = (i_floor + 1).min(n_pos - 1);
                chan.fft_in[i_floor] * (1.0 - alpha) + chan.fft_in[i_ceil] * alpha
            };

            let dphase = shift_k * hop_phase_factor;
            chan.phase_acc[k] = wrap_pi(chan.phase_acc[k] + dphase);
            let (s_sin, s_cos) = chan.phase_acc[k].sin_cos();
            chan.fft_out[k] = interp * Complex::new(s_cos, s_sin);
        }

        // Force DC and Nyquist to be real, then mirror Hermitian to negative half.
        chan.fft_out[0].im = 0.0;
        chan.fft_out[n_pos - 1].im = 0.0;
        for k in 1..(n_pos - 1) {
            chan.fft_out[n - k] = chan.fft_out[k].conj();
        }

        // Inverse FFT (rustfft is unnormalized; we divide by n below).
        fft_inv.process(&mut chan.fft_out);

        // Synthesis window + overlap-add into the output ring at read_pos.
        let inv_n = 1.0 / n as f32;
        for k in 0..n {
            let s = chan.fft_out[k].re * inv_n * win_s[k] * cola;
            let idx = (chan.read_pos + k) % n;
            chan.out_ring[idx] += s;
        }
    }
}

impl SpecChannel {
    fn new(fft_size: usize) -> Self {
        Self {
            in_ring: vec![0.0; fft_size],
            in_pos: 0,
            hop_counter: 0,
            out_ring: vec![0.0; fft_size],
            read_pos: 0,
            phase_acc: vec![0.0; fft_size / 2 + 1],
            fft_in: vec![Complex::new(0.0, 0.0); fft_size],
            fft_out: vec![Complex::new(0.0, 0.0); fft_size],
            dry_delay: vec![0.0; fft_size],
            dry_pos: 0,
        }
    }

    fn reset(&mut self) {
        self.in_ring.iter_mut().for_each(|s| *s = 0.0);
        self.in_pos = 0;
        self.hop_counter = 0;
        self.out_ring.iter_mut().for_each(|s| *s = 0.0);
        self.read_pos = 0;
        self.phase_acc.iter_mut().for_each(|s| *s = 0.0);
        self.fft_in
            .iter_mut()
            .for_each(|s| *s = Complex::new(0.0, 0.0));
        self.fft_out
            .iter_mut()
            .for_each(|s| *s = Complex::new(0.0, 0.0));
        self.dry_delay.iter_mut().for_each(|s| *s = 0.0);
        self.dry_pos = 0;
    }
}

fn compute_cola(win_a: &[f32], win_s: &[f32], hop: usize) -> f32 {
    let n = win_a.len();
    if hop == 0 {
        return 1.0;
    }
    let n_overlap = (n / hop).max(1);
    let mut probe = vec![0.0f32; n];
    for kh in 0..n_overlap {
        let off = kh * hop;
        for i in 0..n {
            let src = (i + n - off) % n;
            probe[i] += win_a[src] * win_s[src];
        }
    }
    let mean: f32 = probe.iter().sum::<f32>() / n as f32;
    if mean > 1e-6 {
        1.0 / mean
    } else {
        1.0
    }
}

#[inline]
fn wrap_pi(mut x: f32) -> f32 {
    while x > PI {
        x -= TAU;
    }
    while x < -PI {
        x += TAU;
    }
    x
}
