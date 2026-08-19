//! Does silence really cost the same as speech to decode?
//!
//!   cargo run --release --example bench-silence-vs-speech -- <data-dir> <model-file>
//!
//! `measure()` times five seconds of digital silence and reports the result as
//! what this machine does with a sermon. Its doc comment defends that: whisper
//! pads every window to thirty seconds, so the cost is said to be set by the
//! model and the window rather than by what was said.
//!
//! That is true of the *encoder*, which runs once over a fixed window. It is not
//! true of the decoder, which runs once per token it produces -- and silence
//! produces none. If the claim holds, the two timings below are the same. If it
//! does not, the benchmark has been reporting encoder cost and calling it the
//! whole decode, which would explain a machine that measures 30x and then falls
//! behind a live preacher.
use std::path::PathBuf;
use std::time::Instant;

use castavox_core::whisper::Local;

const RATE: f32 = 16_000.0;
const SECONDS: f32 = 5.0;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let data_dir = PathBuf::from(args.next().expect("data dir"));
    let model = args.next().expect("model file");
    let speech_path = args.next().expect("a 16 kHz mono WAV of real speech");

    let engine = Local::new(data_dir, model.clone());
    // `transcribe_for_bench` loads the model itself on first use.

    let n = (RATE * SECONDS) as usize;
    let silence = vec![0.0f32; n];

    /*
     * Not speech, but not silence either.
     *
     * Broadband noise is what a room sounds like with nobody in it, and whisper
     * will attempt to decode it rather than short-circuiting on "no speech" --
     * so it exercises the token loop the silence case skips entirely. It is a
     * floor on what real speech costs, not an estimate of it.
     */
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let noise: Vec<f32> = (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed >> 40) as f32 / 8192.0 - 1.0) * 0.25
        })
        .collect();

    // Real speech, which is the only case that makes the decoder run: whisper
    // short-circuits on "no speech", so silence and even broadband noise return
    // without generating a single token.
    let speech = read_wav(&speech_path)?;
    let speech_seconds = speech.len() as f32 / RATE;

    // The first decode loads and warms; it is never the one timed.
    let _ = engine.transcribe_for_bench(&silence);

    let mut timed = |label: &str, samples: &[f32], seconds: f32| -> anyhow::Result<()> {
        let began = Instant::now();
        let text = engine.transcribe_for_bench(samples)?;
        let took = began.elapsed().as_secs_f32();
        let factor = took / seconds;
        println!(
            "{label:<8} {seconds:>5.1} s audio  {took:>6.3} s work  factor {factor:.4}  \
             ({:>5.1}x real time)  {} words",
            if factor > 0.0 { 1.0 / factor } else { 0.0 },
            text.split_whitespace().count()
        );
        Ok(())
    };

    println!("model {model}\n");
    timed("silence", &silence, SECONDS)?;
    timed("noise", &noise, SECONDS)?;
    timed("speech", &speech, speech_seconds)?;
    timed("silence", &silence, SECONDS)?;
    timed("speech", &speech, speech_seconds)?;

    /*
     * The window, which is the other half of the question.
     *
     * whisper pads whatever it is given up to thirty seconds and encodes the
     * whole window. If that is so, one second of audio costs the same wall time
     * as twenty-nine -- and `measure()` divides by the length of its clip, so a
     * five-second clip reports a machine six times faster than it will be on a
     * service made of five-second utterances.
     */
    println!();
    for len in [1.0f32, 5.0, 15.0, 29.0] {
        let padded = vec![0.0f32; (RATE * len) as usize];
        timed("silence", &padded, len)?;
    }
    Ok(())
}

/// Reads a 16 kHz mono 16-bit WAV. Enough of the format for a fixture, and no
/// dependency for something that never runs in the product.
fn read_wav(path: &str) -> anyhow::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    let at = bytes
        .windows(4)
        .position(|w| w == b"data")
        .ok_or_else(|| anyhow::anyhow!("no data chunk in {path}"))?
        + 8;
    Ok(bytes[at..]
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
        .collect())
}
