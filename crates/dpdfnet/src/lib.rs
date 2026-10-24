//! Offline microphone suppression with DPDFNet-2 48 kHz, entirely in Rust.
//! Fixed pretrained graph, FP32 kernels and FFT; no ONNX/C/Python runtime.
#![forbid(unsafe_code)]
mod generated;
mod kernels;
mod stft;

use anyhow::{ensure, Result};
use kernels::{Block, Conv, Dense};
use sha2::{Digest, Sha256};

pub const SAMPLE_RATE: u32 = 48_000;
pub const HOP: usize = 480;
/// One STFT hop plus four model context hops. Removed from saved clip output.
pub const LATENCY_SAMPLES: usize = 5 * HOP;
pub const WEIGHTS_SHA256: &str = "cb7248b8fccbff7b32f254ec2ed0061e604514b2ccd98d997cd081a876424e7b";
const WEIGHTS: &[u8] = include_bytes!("../../../assets/models/dpdfnet2_48khz_hr.weights.f32");

pub struct Model {
    arena: Vec<f32>,
    constants: Vec<f32>,
    dense: Vec<Dense>,
    conv: Vec<Conv>,
    blocks: Vec<Block>,
}
impl Model {
    pub fn bundled() -> Result<Self> {
        Self::from_weights(WEIGHTS)
    }
    pub fn from_weights(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() == 2_582_444 * 4,
            "invalid DPDFNet weight length"
        );
        ensure!(
            format!("{:x}", Sha256::digest(bytes)) == WEIGHTS_SHA256,
            "DPDFNet weights do not match the supported pretrained model"
        );
        let w = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect::<Vec<_>>();
        Ok(Self {
            arena: vec![0.; 219_760],
            constants: generated::constants(&w),
            dense: generated::DENSE
                .iter()
                .map(|&(o, b, k, n, t)| Dense::new(&w, o, b, k, n, t))
                .collect(),
            conv: generated::CONV
                .iter()
                .map(|&(o, b, s)| Conv::new(&w, o, b, s))
                .collect(),
            blocks: generated::BLOCKS
                .iter()
                .map(|&(f, o, a, b)| Block::new(&w, f, o, a, b))
                .collect(),
        })
    }
    pub fn initial_state() -> Vec<f32> {
        let mut state = vec![0.; 56_436];
        state[..generated::INITIAL.len()].copy_from_slice(generated::INITIAL);
        state
    }
    pub fn process_spectrum(
        &mut self,
        input: &[f32; 962],
        state: &[f32],
        output: &mut [f32; 962],
        next: &mut [f32],
    ) -> Result<()> {
        ensure!(
            state.len() == 56_436 && next.len() == 56_436,
            "invalid DPDFNet recurrent state shape"
        );
        ensure!(
            input.iter().all(|v| v.is_finite()),
            "non-finite DPDFNet input"
        );
        self.graph(input, state, output, next);
        ensure!(
            output.iter().chain(next.iter()).all(|v| v.is_finite()),
            "non-finite DPDFNet inference output"
        );
        Ok(())
    }
}

/// Arbitrary input chunks, latency-compensated output, exact sample count on finish.
/// Construct on a clip/export worker. Capture must not instantiate this processor.
pub struct Denoiser {
    model: Model,
    state: Vec<f32>,
    next: Vec<f32>,
    stft: stft::Stft,
    input: [f32; HOP],
    used: usize,
    skip: usize,
    fed: u64,
    written: u64,
    finished: bool,
}
impl Denoiser {
    pub fn new() -> Result<Self> {
        Ok(Self {
            model: Model::bundled()?,
            state: Model::initial_state(),
            next: vec![0.; 56_436],
            stft: stft::Stft::new(),
            input: [0.; HOP],
            used: 0,
            skip: LATENCY_SAMPLES,
            fed: 0,
            written: 0,
            finished: false,
        })
    }
    pub fn reset(&mut self) {
        self.state.fill(0.);
        self.state[..generated::INITIAL.len()].copy_from_slice(generated::INITIAL);
        self.next.fill(0.);
        self.stft.reset();
        self.input.fill(0.);
        self.used = 0;
        self.skip = LATENCY_SAMPLES;
        self.fed = 0;
        self.written = 0;
        self.finished = false;
    }
    fn hop(&mut self, emit: &mut impl FnMut(&[f32]) -> Result<()>) -> Result<()> {
        let mut spec = [0.; 962];
        let mut enhanced = [0.; 962];
        let mut output = [0.; HOP];
        self.stft.analysis(&self.input, &mut spec);
        self.model
            .process_spectrum(&spec, &self.state, &mut enhanced, &mut self.next)?;
        std::mem::swap(&mut self.state, &mut self.next);
        self.stft.synthesis(&enhanced, &mut output);
        let skip = self.skip.min(HOP);
        self.skip -= skip;
        let count = (HOP - skip).min((self.fed - self.written) as usize);
        if count > 0 {
            emit(&output[skip..skip + count])?;
            self.written += count as u64;
        }
        self.used = 0;
        Ok(())
    }
    pub fn process(
        &mut self,
        mut input: &[f32],
        mut emit: impl FnMut(&[f32]) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            !self.finished,
            "denoiser has already finished; reset before reuse"
        );
        ensure!(
            input.iter().all(|v| v.is_finite()),
            "non-finite audio samples"
        );
        while !input.is_empty() {
            let take = input.len().min(HOP - self.used);
            self.input[self.used..self.used + take].copy_from_slice(&input[..take]);
            self.used += take;
            self.fed += take as u64;
            input = &input[take..];
            if self.used == HOP {
                self.hop(&mut emit)?;
            }
        }
        Ok(())
    }
    pub fn finish(&mut self, mut emit: impl FnMut(&[f32]) -> Result<()>) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.used > 0 {
            self.input[self.used..].fill(0.);
            self.hop(&mut emit)?;
        }
        self.input.fill(0.);
        while self.written < self.fed {
            self.hop(&mut emit)?;
        }
        self.finished = true;
        Ok(())
    }
}
