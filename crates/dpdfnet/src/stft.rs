//! Vorbis analysis/synthesis adapted from HushMic (MIT OR Apache-2.0).
use crate::HOP;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;
const FFT: usize = 960;

pub(super) struct Stft {
    window: [f32; FFT],
    ring: [f32; FFT],
    ola: [f32; FFT],
    buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
}
impl Stft {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(FFT);
        let inverse = planner.plan_fft_inverse(FFT);
        let mut window = [0.; FFT];
        for (n, w) in window.iter_mut().enumerate() {
            let s = (std::f32::consts::PI * (n as f32 + 0.5) / FFT as f32).sin();
            *w = (std::f32::consts::FRAC_PI_2 * s * s).sin();
        }
        let scratch = vec![
            Complex32::default();
            forward
                .get_inplace_scratch_len()
                .max(inverse.get_inplace_scratch_len())
        ];
        Self {
            window,
            ring: [0.; FFT],
            ola: [0.; FFT],
            buffer: vec![Complex32::default(); FFT],
            scratch,
            forward,
            inverse,
        }
    }
    pub fn reset(&mut self) {
        self.ring.fill(0.);
        self.ola.fill(0.);
    }
    pub fn analysis(&mut self, input: &[f32; HOP], out: &mut [f32; 962]) {
        self.ring.copy_within(HOP.., 0);
        self.ring[HOP..].copy_from_slice(input);
        for (i, v) in self.buffer.iter_mut().enumerate() {
            *v = Complex32::new(self.ring[i] * self.window[i], 0.);
        }
        self.forward
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        for (bin, value) in out.as_chunks_mut::<2>().0.iter_mut().zip(&self.buffer) {
            *bin = [value.re, value.im];
        }
    }
    pub fn synthesis(&mut self, input: &[f32; 962], out: &mut [f32; HOP]) {
        for (value, bin) in self.buffer.iter_mut().zip(input.as_chunks::<2>().0) {
            *value = Complex32::new(bin[0], bin[1]);
        }
        for j in 481..FFT {
            self.buffer[j] = self.buffer[FFT - j].conj();
        }
        self.inverse
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        self.ola.copy_within(HOP.., 0);
        self.ola[HOP..].fill(0.);
        for (i, value) in self.buffer.iter().enumerate() {
            self.ola[i] += value.re / FFT as f32 * self.window[i];
        }
        out.copy_from_slice(&self.ola[..HOP]);
    }
}
