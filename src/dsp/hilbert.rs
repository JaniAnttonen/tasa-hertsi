use std::f32::consts::PI;

/// Linear-phase Hilbert transformer (FIR, odd length, Hann-windowed).
///
/// For each input sample, returns `(real_delayed, imag)` — the analytic-signal
/// pair where `real_delayed` is the input delayed by `(N-1)/2` samples (to
/// time-align with the FIR output). Multiplying this pair by `e^{j 2π f t}`
/// yields a single-sideband frequency-shifted output.
pub struct HilbertFir {
    taps: Vec<f32>,
    delay: Vec<f32>,
    idx: usize,
    pub group_delay: usize,
}

impl HilbertFir {
    pub fn new(n_taps: usize) -> Self {
        assert!(n_taps % 2 == 1, "Hilbert FIR length must be odd");
        let m = (n_taps - 1) / 2;
        let mut taps = vec![0.0f32; n_taps];
        for k in 0..n_taps {
            let i = k as i32 - m as i32;
            let h = if i == 0 || i % 2 == 0 {
                0.0
            } else {
                2.0 / (PI * i as f32)
            };
            // Hann window to keep the impulse response well-behaved on truncation.
            let w = 0.5 - 0.5 * (2.0 * PI * k as f32 / (n_taps - 1) as f32).cos();
            taps[k] = h * w;
        }
        Self {
            taps,
            delay: vec![0.0; n_taps],
            idx: 0,
            group_delay: m,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        self.delay[self.idx] = x;
        let n = self.taps.len();
        let mut imag = 0.0f32;
        // Convolve: imag[t] = sum_k taps[k] * delay[t-k]
        for k in 0..n {
            let i = (self.idx + n - k) % n;
            imag += self.taps[k] * self.delay[i];
        }
        let real_idx = (self.idx + n - self.group_delay) % n;
        let real = self.delay[real_idx];
        self.idx = (self.idx + 1) % n;
        (real, imag)
    }

    pub fn reset(&mut self) {
        self.delay.iter_mut().for_each(|s| *s = 0.0);
        self.idx = 0;
    }
}
