//! Raw little-endian f32 mono 48 kHz in/out, for tests and offline tools.
use anyhow::{Context, Result};
use dpdfnet::Denoiser;
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    time::Instant,
};
fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let mut input = BufReader::new(File::open(args.get(1).context("input f32 file required")?)?);
    let mut output = BufWriter::new(File::create_new(
        args.get(2).context("output f32 file required")?,
    )?);
    let mut denoiser = Denoiser::new()?;
    let start = Instant::now();
    let mut count = 0u64;
    let mut block = [0u8; 4 * 480];
    let mut emit = |samples: &[f32]| -> Result<()> {
        for value in samples {
            output.write_all(&value.to_le_bytes())?;
        }
        Ok(())
    };
    loop {
        let mut n = 0;
        while n < block.len() {
            let got = input.read(&mut block[n..])?;
            if got == 0 {
                break;
            }
            n += got;
        }
        if n == 0 {
            break;
        }
        anyhow::ensure!(n.is_multiple_of(4), "unaligned f32 input");
        let samples = block[..n]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect::<Vec<_>>();
        denoiser.process(&samples, &mut emit)?;
        count += samples.len() as u64;
    }
    denoiser.finish(&mut emit)?;
    output.flush()?;
    println!(
        "samples={count} seconds={:.3} processing={:.3}s RTF={:.4}",
        count as f64 / 48000.,
        start.elapsed().as_secs_f64(),
        start.elapsed().as_secs_f64() / (count as f64 / 48000.)
    );
    Ok(())
}
