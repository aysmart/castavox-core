//! How fast this machine decodes, so the answer is measured and not assumed.
//!
//!     cargo run --release --example bench -- <model.bin> <audio.raw>
//!
//! The audio is raw 16 kHz mono s16le -- what the capture thread produces.
//! Prints the real-time factor: below 1.0 means it decodes faster than the
//! speech arrives, which is the whole question.
use std::time::Instant;

fn main() {
    let model = std::env::args().nth(1).expect("a model path");
    let audio = std::env::args().nth(2).expect("a raw audio path");

    let raw = std::fs::read(&audio).expect("read the audio");
    let samples: Vec<f32> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0)
        .collect();
    let seconds = samples.len() as f32 / 16_000.0;

    let dir = std::path::Path::new(&model).parent().unwrap().parent().unwrap();
    let file = std::path::Path::new(&model).file_name().unwrap().to_str().unwrap();

    let engine = std::sync::Arc::new(castavox_core::whisper::Local::new(
        dir.to_path_buf(),
        file.to_string(),
    ));

    // One pass to load and warm, then three timed.
    let mut best = f32::MAX;
    for pass in 0..4 {
        let began = Instant::now();
        let text = engine.transcribe_for_bench(&samples).expect("decode");
        let took = began.elapsed().as_secs_f32();
        if pass == 0 {
            println!("  (first pass loads the model) {text:.60}");
            continue;
        }
        best = best.min(took);
    }
    println!("  {seconds:.1}s of audio, best decode {best:.2}s, real-time factor {:.2}", best / seconds);
}
