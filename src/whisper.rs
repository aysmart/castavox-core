//! Transcription on this machine, for anyone who cannot run an Azure account.
//!
//! Azure's free tier is about five hours of audio a month, which is two
//! services. Past that it needs a subscription and a card, and for a small
//! church that is the difference between using this software and not. So there
//! is a second engine: whisper.cpp, running locally, needing no account, no key
//! and no network once its model is here.
//!
//! # What it costs
//!
//! Stated plainly because the operator should choose knowing it.
//!
//! Azure streams: words arrive as they are spoken and a verse can be found
//! mid-sentence. Whisper decodes a finished stretch of audio, so a verse
//! arrives a second or two after the sentence ends. Cutting on silence keeps
//! that gap short, but it cannot be removed -- the model needs the whole
//! utterance before it can transcribe it.
//!
//! Accuracy is lower too, most visibly on names and places, and detection is
//! only ever as good as the transcript it reads.
//!
//! # The model
//!
//! Not bundled and not chosen for the operator. whisper.cpp publishes about
//! thirty, from a 30 MB quantised tiny that will run on a decade-old laptop to
//! a 1.5 GB large that will not -- and only the person at that machine knows
//! which they can afford, in disk, in CPU and in patience.
//!
//! So the list is fetched from whisper.cpp's own repository, with real sizes,
//! and the operator picks. Nothing about which models exist is written into
//! this app, because that is a fact about somebody else's repository and it
//! changes without telling us.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::TARGET_SAMPLE_RATE;

/// Where the list of models comes from.
///
/// Asked for rather than written down. whisper.cpp's own repository is the
/// authority on which models exist and how big each one is, and it gains new
/// ones -- the quantised builds, large-v3-turbo -- without asking us. A list
/// compiled into the app is a list that is wrong by the next release, and the
/// sizes in it are wrong the first time somebody re-uploads a file.
const CATALOGUE_URL: &str =
    "https://huggingface.co/api/models/ggerganov/whisper.cpp/tree/main";
const DOWNLOAD_BASE: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Below this, a window is silence rather than speech. Relative to the quietest
/// audio seen so far rather than absolute, because a condenser mic in a hall
/// and a laptop mic in an office do not share a noise floor.
const SPEECH_OVER_FLOOR: f32 = 3.0;
/// Nothing shorter is worth decoding; it is a cough or a chair.
const MIN_UTTERANCE: Duration = Duration::from_millis(900);
/// Silence that ends an utterance. Long enough not to cut mid-sentence at a
/// breath, short enough that the verse is not late.
const END_SILENCE: Duration = Duration::from_millis(650);
/// A speaker who never pauses still has to be transcribed eventually.
const MAX_UTTERANCE: Duration = Duration::from_secs(18);
/// How often the growing buffer is decoded to show words before the sentence
/// ends. Every interim costs a full decode, so this is deliberately unhurried.
const INTERIM_EVERY: Duration = Duration::from_millis(2_200);
/// Settled utterances outstanding before the operator is told the machine is
/// losing. Three is a genuine backlog rather than one slow decode.
const BEHIND_AFTER: usize = 3;

/// One model the operator could choose.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// The file name, which is also its identity: "ggml-base.en.bin".
    pub file: String,
    /// What to call it on screen: "base.en".
    pub label: String,
    pub bytes: u64,
    /// True for the `.en` builds, which are better at English and cannot do
    /// anything else -- the one distinction an operator has to understand.
    pub english_only: bool,
    /// Smaller and faster at some cost in accuracy.
    pub quantised: bool,
    pub installed: bool,
}

/// Every model on disk and, if the list can be reached, every one available.
///
/// Works offline: with no network this still reports what is already here, so
/// an operator who has downloaded one can still choose it in a hall with no
/// connection.
pub fn catalogue(data_dir: &Path) -> Vec<ModelInfo> {
    // rustls has no provider unless one is installed, and this may be the
    // first thing in the process to make an HTTPS request -- the assistant
    // installs one too, but the operator may never have turned it on.
    crate::tls::install();
    let mut found: Vec<ModelInfo> = Vec::new();

    if let Ok(response) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .and_then(|client| client.get(CATALOGUE_URL).send())
    {
        if let Ok(entries) = response.json::<Vec<serde_json::Value>>() {
            for entry in entries {
                let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                if !path.starts_with("ggml-") || !path.ends_with(".bin") {
                    continue;
                }
                let bytes = entry.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                found.push(describe(data_dir, path, bytes));
            }
        }
    }

    // Anything already downloaded, in case the list could not be fetched or no
    // longer offers something the operator is using.
    for file in installed_files(data_dir) {
        if !found.iter().any(|m| m.file == file) {
            let bytes = std::fs::metadata(model_path(data_dir, &file)).map(|m| m.len()).unwrap_or(0);
            found.push(describe(data_dir, &file, bytes));
        }
    }

    // Smallest first: the operator scanning this list is usually looking for
    // the one their machine can manage, not the best one that exists.
    found.sort_by(|a, b| a.bytes.cmp(&b.bytes).then_with(|| a.label.cmp(&b.label)));
    found
}

fn describe(data_dir: &Path, file: &str, bytes: u64) -> ModelInfo {
    let label = file.trim_start_matches("ggml-").trim_end_matches(".bin").to_string();
    ModelInfo {
        english_only: label.contains(".en"),
        quantised: label.contains("-q"),
        installed: is_installed(data_dir, file),
        file: file.to_string(),
        label,
        bytes,
    }
}

fn installed_files(data_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("models")) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("ggml-") && name.ends_with(".bin"))
        .collect()
}

/// Where a model lives once fetched.
pub fn model_path(data_dir: &Path, file: &str) -> PathBuf {
    // Basename only. The file name arrives from a remote listing, and a path
    // that walked out of this directory would be a remote server choosing
    // where we write.
    let safe = Path::new(file).file_name().unwrap_or_default();
    data_dir.join("models").join(safe)
}

pub fn is_installed(data_dir: &Path, file: &str) -> bool {
    // Non-empty and not the partial file: a download interrupted halfway leaves
    // something that exists and cannot be loaded, which reads to the operator
    // as a corrupt install rather than an unfinished one.
    std::fs::metadata(model_path(data_dir, file)).map(|m| m.len() > 0).unwrap_or(false)
}

/// Fetches a model, reporting progress as (received, total).
pub fn download(data_dir: &Path, file: &str, progress: impl Fn(u64, u64)) -> Result<PathBuf> {
    // rustls has no provider unless one is installed, and this may be the
    // first thing in the process to make an HTTPS request -- the assistant
    // installs one too, but the operator may never have turned it on.
    crate::tls::install();
    let target = model_path(data_dir, file);
    if is_installed(data_dir, file) {
        return Ok(target);
    }
    std::fs::create_dir_all(target.parent().expect("model path has a parent"))
        .context("could not make a place for the speech model")?;

    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or(file);
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .context("could not prepare the download")?;
    let mut response = client
        .get(format!("{DOWNLOAD_BASE}/{name}"))
        .send()
        .context("could not reach the model host")?;
    if !response.status().is_success() {
        return Err(anyhow!("the model host returned {}", response.status()));
    }
    let total = response.content_length().unwrap_or(0);

    // Written beside the target and renamed at the end, so an interrupted
    // download never leaves something that looks installed.
    let partial = target.with_extension("part");
    let mut sink = std::fs::File::create(&partial).context("could not write the speech model")?;

    let mut buffer = vec![0u8; 1 << 20];
    let mut received = 0u64;
    loop {
        let n = response.read(&mut buffer).context("the download was interrupted")?;
        if n == 0 {
            break;
        }
        sink.write_all(&buffer[..n]).context("could not write the speech model")?;
        received += n as u64;
        progress(received, total);
    }
    sink.flush().ok();
    drop(sink);

    if total > 0 && received != total {
        let _ = std::fs::remove_file(&partial);
        return Err(anyhow!("the download ended early: {received} of {total} bytes"));
    }

    std::fs::rename(&partial, &target).context("could not finish installing the speech model")?;
    Ok(target)
}

/// The model currently in memory.
struct Loaded {
    file: String,
    context: WhisperContext,
}

/// The local engine, and the model it is using.
///
/// Lives for as long as the app rather than for one session, so the operator
/// can change model while listening. The change is applied between utterances,
/// never during a decode, and a model that fails to load leaves the previous
/// one in place -- a bad choice in a settings dialog must not take the
/// transcript down in the middle of a sermon.
/// A piece of audio waiting to be decoded.
///
/// The distinction matters when the machine is behind: a settled utterance is
/// the transcript and is always decoded, a partial one is a courtesy and is the
/// first thing dropped.
enum Job {
    /// A complete utterance. Never discarded.
    Settled(Vec<f32>),
    /// A sentence still being spoken. Discarded freely.
    Partial(Vec<f32>),
}

pub struct Local {
    data_dir: PathBuf,
    threads: i32,
    /// What the operator has asked for. Compared against `loaded` at each safe
    /// point; a plain string compare, so polling it costs nothing.
    wanted: Mutex<String>,
    loaded: Mutex<Option<Loaded>>,
}

impl Local {
    pub fn new(data_dir: PathBuf, model_file: String) -> Self {
        // Leave a core for everything else: this runs while the machine is also
        // encoding video, and a transcript is not worth dropped frames.
        let threads = std::thread::available_parallelism()
            .map(|n| (n.get().saturating_sub(1)).clamp(1, 8))
            .unwrap_or(4) as i32;

        Self {
            data_dir,
            threads,
            wanted: Mutex::new(model_file),
            loaded: Mutex::new(None),
        }
    }

    /// Asks for a different model. Takes effect at the next utterance boundary.
    pub fn request(&self, model_file: &str) {
        *self.wanted.lock() = model_file.to_string();
    }

    pub fn loaded_model(&self) -> Option<String> {
        self.loaded.lock().as_ref().map(|l| l.file.clone())
    }

    /// Brings the loaded model into line with what was asked for.
    ///
    /// Returns the name of the model now in use. An unavailable or unreadable
    /// choice is reported and the previous model kept, so the session survives
    /// a mistake in the settings dialog.
    fn ensure(&self, note: &dyn Fn(String)) -> Result<String> {
        let wanted = self.wanted.lock().clone();

        if let Some(loaded) = self.loaded.lock().as_ref() {
            if loaded.file == wanted {
                return Ok(wanted);
            }
        }

        if !is_installed(&self.data_dir, &wanted) {
            let held = self.loaded.lock().as_ref().map(|l| l.file.clone());
            return match held {
                Some(file) => {
                    note(format!("{wanted} is not downloaded; still using {file}."));
                    Ok(file)
                }
                None => Err(anyhow!("{wanted} has not been downloaded")),
            };
        }

        let path = model_path(&self.data_dir, &wanted);
        match WhisperContext::new_with_params(
            path.to_str().unwrap_or_default(),
            WhisperContextParameters::default(),
        ) {
            Ok(context) => {
                *self.loaded.lock() = Some(Loaded { file: wanted.clone(), context });
                note(format!("Using {wanted}."));
                Ok(wanted)
            }
            Err(error) => {
                let held = self.loaded.lock().as_ref().map(|l| l.file.clone());
                match held {
                    Some(file) => {
                        note(format!("{wanted} could not be loaded ({error}); still using {file}."));
                        Ok(file)
                    }
                    None => Err(anyhow!("could not load {wanted}: {error}")),
                }
            }
        }
    }

    /// Decodes one stretch of audio into plain text.
    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let guard = self.loaded.lock();
        let loaded = guard.as_ref().ok_or_else(|| anyhow!("no speech model is loaded"))?;

        let mut state = loaded
            .context
            .create_state()
            .map_err(|error| anyhow!("could not start a decode: {error}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_translate(false);
        params.set_language(Some("en"));
        // All of this prints to stdout, which is the NDJSON pipe the host reads.
        // Left on, whisper.cpp's progress chatter would corrupt every message.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, samples)
            .map_err(|error| anyhow!("the speech model failed: {error}"))?;

        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            // Lossy: a model can emit a partial UTF-8 sequence at a segment
            // boundary, and losing one character is better than dropping the
            // sentence it was in.
            if let Some(segment) = state.get_segment(i) {
                if let Ok(words) = segment.to_str_lossy() {
                    text.push_str(&words);
                }
            }
        }
        Ok(clean(&text))
    }

    /// Consumes captured audio until stopped, reporting words as they settle.
    ///
    /// `interim` is shown and never detected against; `final_text` is what the
    /// detector reads. That is the same split the Azure bridge reports, so
    /// everything downstream is unaware of which engine produced it.
    ///
    /// # Why the decoding happens on its own thread
    ///
    /// Whisper is not guaranteed to run faster than the person talking. On a
    /// machine where it does not, decoding on the thread that drains the
    /// microphone is fatal rather than merely slow: capture arrives over a
    /// bounded channel, so every second spent decoding is a second nobody is
    /// reading from it, and the audio is not delayed -- it is dropped on the
    /// floor. Speech genuinely disappears, and the transcript is wrong in a way
    /// the operator cannot see.
    ///
    /// So this thread does nothing but listen and cut the audio into
    /// utterances. Decoding happens behind it, and the two are allowed to drift
    /// apart, because falling behind is recoverable and losing what was said is
    /// not.
    pub fn run(
        self: &Arc<Self>,
        pcm: Receiver<Vec<i16>>,
        stop: Arc<AtomicBool>,
        note: impl Fn(String),
        interim: impl Fn(String) + Send + 'static,
        final_text: impl Fn(String) + Send + 'static,
    ) -> Result<()> {
        self.ensure(&note)?;

        let rate = TARGET_SAMPLE_RATE as f32;
        let mut buffer: Vec<f32> = Vec::with_capacity(TARGET_SAMPLE_RATE as usize * 20);
        let mut floor = f32::MAX;
        let mut silence = Duration::ZERO;
        let mut spoke = false;
        let mut last_interim = Instant::now();
        // How many settled utterances the decoder still owes us, and whether we
        // have already said something about it.
        let mut waiting = 0usize;
        let mut warned = false;

        let (jobs, decoded, done, worker) = Arc::clone(self).decoder(&stop, interim, final_text);

        while !stop.load(Ordering::Relaxed) {
            let Ok(chunk) = pcm.recv() else { break };
            if chunk.is_empty() {
                continue;
            }

            // Whatever the decoder finished while we were listening.
            waiting = waiting.saturating_sub(done.swap(0, Ordering::Relaxed));

            let span = Duration::from_secs_f32(chunk.len() as f32 / rate);
            let loudness = rms(&chunk);
            // The floor drifts down to the room's own noise and never up, so a
            // long stretch of speech cannot raise it and swallow the next pause.
            floor = floor.min(loudness.max(1e-5));

            buffer.extend(chunk.iter().map(|s| *s as f32 / 32_768.0));

            if loudness > floor * SPEECH_OVER_FLOOR {
                silence = Duration::ZERO;
                spoke = true;
            } else {
                silence += span;
            }

            let held = Duration::from_secs_f32(buffer.len() as f32 / rate);
            let ended = spoke && held >= MIN_UTTERANCE && silence >= END_SILENCE;
            let overran = held >= MAX_UTTERANCE;

            if ended || overran {
                // Never dropped, however far behind the decoder is: this is the
                // transcript, and a queued sentence arrives late where a
                // discarded one never arrives at all.
                let _ = jobs.send(Job::Settled(std::mem::take(&mut buffer)));
                buffer.reserve(TARGET_SAMPLE_RATE as usize * 20);
                waiting += 1;

                /*
                 * Say so when the machine cannot keep up.
                 *
                 * Without this the only symptom is a transcript arriving later
                 * and later, which reads as the app being broken rather than as
                 * the model being too large for the hardware -- and the person
                 * at the desk is the only one who can fix that, by choosing a
                 * smaller one. Reported once per session rather than per
                 * utterance: it is a standing condition, not an event.
                 */
                if waiting >= BEHIND_AFTER && !warned {
                    warned = true;
                    note(format!(
                        "Transcription is running behind: {waiting} utterances are still \
                         being decoded. This model is too large for this machine -- a \
                         smaller one under Settings will keep up."
                    ));
                }
                silence = Duration::ZERO;
                spoke = false;
                last_interim = Instant::now();

                // Between utterances is the only safe moment to change model:
                // nothing is part-decoded and no audio is waiting on it.
                self.ensure(&note)?;
                continue;
            }

            // Something on screen while the sentence is still being said, and
            // the first thing to go when the machine cannot keep up. Skipped
            // outright while the decoder is busy: an interim is a courtesy, and
            // queueing them is what turns a slow machine into a stuck one --
            // each decode covers a longer buffer than the last, so every one
            // takes longer than the interval that triggers it.
            if spoke
                && held >= MIN_UTTERANCE
                && last_interim.elapsed() >= INTERIM_EVERY
                && !decoded.load(Ordering::Relaxed)
            {
                let _ = jobs.send(Job::Partial(buffer.clone()));
                last_interim = Instant::now();
            }
        }

        // Whatever was still being said when the operator stopped. Sent as
        // settled: it is the end of a sentence somebody spoke, and the fact
        // that a pause never arrived to close it does not make it disposable.
        if spoke && buffer.len() as f32 / rate >= MIN_UTTERANCE.as_secs_f32() {
            let _ = jobs.send(Job::Settled(buffer));
        }

        // Closing the queue is what ends the decoder's loop.
        drop(jobs);

        // Waited for rather than abandoned: anything still queued is transcript
        // that was spoken and not yet reported, and dropping the thread here
        // would discard it. On a machine that has fallen behind this can take a
        // moment, which is the cost of not losing what was said.
        if let Some(worker) = worker {
            let _ = worker.join();
        }

        Ok(())
    }

    /// What the decoder has been handed.
    fn decoder(
        self: Arc<Self>,
        stop: &Arc<AtomicBool>,
        interim: impl Fn(String) + Send + 'static,
        final_text: impl Fn(String) + Send + 'static,
    ) -> (
        std::sync::mpsc::Sender<Job>,
        Arc<AtomicBool>,
        Arc<AtomicUsize>,
        Option<std::thread::JoinHandle<()>>,
    ) {
        // Unbounded on purpose. A bound here would push back on the thread
        // draining the microphone, which is the one thing that must never
        // block; settled utterances are at most MAX_UTTERANCE long and stop
        // arriving when the operator stops talking, so the queue drains.
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let busy = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicUsize::new(0));

        let working = Arc::clone(&busy);
        let counted = Arc::clone(&finished);
        let stop = Arc::clone(stop);
        let engine = self;

        let worker = std::thread::Builder::new()
            .name("pulpitry-whisper".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if stop.load(Ordering::Relaxed) {
                        // Finish what was settled; abandon anything partial.
                        if matches!(job, Job::Partial(_)) {
                            continue;
                        }
                    }
                    working.store(true, Ordering::Relaxed);
                    let audio = match &job {
                        Job::Settled(audio) | Job::Partial(audio) => audio,
                    };
                    let spoken = engine.transcribe(audio);
                    working.store(false, Ordering::Relaxed);

                    if matches!(job, Job::Settled(_)) {
                        counted.fetch_add(1, Ordering::Relaxed);
                    }
                    match (job, spoken) {
                        (Job::Settled(_), Ok(text)) if !text.is_empty() => final_text(text),
                        (Job::Partial(_), Ok(text)) if !text.is_empty() => interim(text),
                        (_, Err(error)) => crate::log_line!("[whisper] {error:#}"),
                        _ => {}
                    }
                }
            })
            .ok();

        (tx, busy, finished, worker)
    }
}

fn rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| {
        let v = *s as f64 / 32_768.0;
        v * v
    }).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Whisper marks non-speech with bracketed tags and pads with spaces.
///
/// `[BLANK_AUDIO]`, `(wind blowing)` and their like are descriptions of the
/// room, not things anybody said. Left in, they would reach the transcript on
/// screen and be searched for scripture.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changing model must never take the session down with it.
    ///
    /// The operator is in a settings dialog during a service. Whatever they
    /// pick -- something not downloaded, something corrupt, a name that means
    /// nothing -- the words on screen have to keep arriving.
    #[test]
    #[ignore = "needs a model on disk"]
    fn a_bad_model_choice_keeps_the_good_one() {
        let model = std::env::var("PULPITRY_TEST_MODEL").expect("PULPITRY_TEST_MODEL");
        let path = std::path::Path::new(&model);
        let dir = path.parent().and_then(|p| p.parent()).expect("models/ has a parent");
        let file = path.file_name().unwrap().to_str().unwrap().to_string();

        let quiet = |_: String| {};
        let engine = Local::new(dir.to_path_buf(), file.clone());
        assert_eq!(engine.ensure(&quiet).expect("the real model loads"), file);

        // Not downloaded.
        engine.request("ggml-nonesuch.bin");
        assert_eq!(engine.ensure(&quiet).expect("survives"), file, "should have kept the loaded model");

        // Present but not a model: a truncated download looks exactly like this.
        let junk = dir.join("models").join("ggml-broken.bin");
        std::fs::write(&junk, b"not a model").unwrap();
        engine.request("ggml-broken.bin");
        assert_eq!(engine.ensure(&quiet).expect("survives"), file, "should have kept the loaded model");
        std::fs::remove_file(&junk).ok();

        // And it can still transcribe afterwards, which is the actual claim.
        engine.request(&file);
        assert_eq!(engine.ensure(&quiet).unwrap(), file);
        let tone: Vec<f32> = (0..16_000).map(|i| ((i as f32) * 0.01).sin() * 0.05).collect();
        engine.transcribe(&tone).expect("still works after two bad choices");
    }

    /// The only proof that matters: real audio in, the right words out.
    ///
    /// Ignored by default because it needs a model on disk, which is 30 MB
    /// nobody should download to run a unit test. Point PULPITRY_TEST_MODEL and
    /// PULPITRY_TEST_WAV at one and a 16 kHz WAV, then:
    ///
    ///     cargo test --release -- --ignored transcribes
    #[test]
    #[ignore = "needs a model and a recording"]
    fn transcribes_real_speech() {
        let model = std::env::var("PULPITRY_TEST_MODEL").expect("PULPITRY_TEST_MODEL");
        let wav = std::env::var("PULPITRY_TEST_WAV").expect("PULPITRY_TEST_WAV");

        let bytes = std::fs::read(&wav).expect("the recording");
        // Walk the RIFF chunks rather than assuming a 44-byte header, which is
        // only true of the simplest writers and not of the one on this machine.
        let mut at = 12;
        let (start, len) = loop {
            let id = &bytes[at..at + 4];
            let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            if id == b"data" {
                break (at + 8, size);
            }
            at += 8 + size + (size & 1);
        };
        let samples: Vec<i16> = bytes[start..start + len]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();

        let path = std::path::Path::new(&model);
        let dir = path.parent().and_then(|p| p.parent()).expect("models/ has a parent");
        let engine = Local::new(dir.to_path_buf(), path.file_name().unwrap().to_str().unwrap().into());
        engine.ensure(&|_| {}).expect("model loads");
        let started = std::time::Instant::now();
        let text = engine.transcribe(
            &samples.iter().map(|s| *s as f32 / 32_768.0).collect::<Vec<_>>(),
        ).expect("decode");
        let took = started.elapsed();

        let audio = samples.len() as f32 / TARGET_SAMPLE_RATE as f32;
        println!("audio {audio:.1}s, decoded in {:.1}s ({:.2}x real time)", took.as_secs_f32(), took.as_secs_f32() / audio);
        println!("heard: {text}");

        let lower = text.to_lowercase();
        assert!(lower.contains("god so loved the world"), "wrong words back: {text}");
        assert!(!lower.contains("["), "apparatus survived: {text}");
    }

    /// The list has to arrive here too, not only in the sibling product.
    #[test]
    #[ignore = "reaches the network"]
    fn lists_the_models_that_exist() {
        let dir = std::env::temp_dir().join("pulpitry-model-test");
        let found = catalogue(&dir);
        assert!(found.len() > 10, "expected a real catalogue, got {}", found.len());
        assert!(found.iter().any(|m| m.label.contains("base")), "no base model listed");
        // Smallest first, so the operator scanning it meets the affordable ones.
        assert!(found[0].bytes <= found[found.len() - 1].bytes, "not sorted by size");
        println!("{} models; smallest {} at {} MB", found.len(), found[0].label, found[0].bytes / 1048576);
    }

    #[test]
    fn strips_the_sounds_nobody_said() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean("(wind blowing) and he said"), "and he said");
        assert_eq!(clean("For God [MUSIC] so loved the world"), "For God so loved the world");
    }

    #[test]
    fn collapses_the_padding_whisper_adds() {
        assert_eq!(clean("   For   God so loved   "), "For God so loved");
    }

    #[test]
    fn an_unclosed_bracket_does_not_eat_the_rest() {
        // Better to lose the tail of one utterance than to have a stray
        // bracket silence the transcript from then on.
        assert_eq!(clean("he said [unfinished"), "he said");
    }

    #[test]
    fn silence_and_speech_are_told_apart() {
        let quiet: Vec<i16> = vec![0; 1600];
        let loud: Vec<i16> = (0..1600).map(|i| ((i as f32 * 0.1).sin() * 8000.0) as i16).collect();
        assert!(rms(&quiet) < rms(&loud));
        assert!(rms(&loud) > 0.05, "a speaking voice should clear the floor");
    }
}
