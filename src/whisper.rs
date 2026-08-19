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
//! mid-sentence. Whisper decodes a stretch of audio, so a verse arrives a
//! second or two after the sentence ends. Cutting on silence keeps that gap
//! short, but it cannot be removed -- the model needs a finished stretch before
//! it can transcribe it.
//!
//! The growing buffer is also decoded while the sentence is still being spoken,
//! and those interims go to the detector as well as to the screen, so a
//! quotation can be caught before the speaker stops. How often that happens is
//! paced off how long the last one took on this machine: a fast one gets an
//! interim every 600 ms and something close to Azure's behaviour, and a slow one
//! is unchanged, which is to say it waits for the sentence to end.
//!
//! Accuracy is lower too, most visibly on names and places, and detection is
//! only ever as good as the transcript it reads.
//!
//! # The model
//!
//! Not bundled. whisper.cpp publishes about thirty, from a 30 MB quantised tiny
//! that will run on a decade-old laptop to a 1.5 GB large that will not, so the
//! list is fetched from whisper.cpp's own repository with real sizes. Nothing
//! about which models exist is written into this app, because that is a fact
//! about somebody else's repository and it changes without telling us.
//!
//! It *is* now chosen for the operator, which it was not. The argument for
//! asking was that only the person at that machine knows what they can afford
//! in disk, in CPU and in patience — which is true, and which they still cannot
//! answer, because nothing in the question tells them whether base or small is
//! the one their laptop keeps up with. So the question went unanswered, the
//! engine sat unusable behind it, and the operator concluded local
//! transcription did not work.
//!
//! `recommended` now picks one from the machine's core count, and the operator
//! overrides it whenever they like. The guess is deliberately conservative and
//! the reason is asymmetric: too small costs some accuracy on names, which is
//! quiet and correctable, while too large costs a transcript that arrives after
//! the sentence, which a congregation sees.

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
/// ends, before anything has been measured. Every interim costs a full decode
/// of everything said so far, so the first one is deliberately unhurried.
const INTERIM_EVERY: Duration = Duration::from_millis(2_200);
/// The slowest this ever goes, which is what it did at every speed until the
/// pacing below existed. A machine that cannot afford interims is already
/// skipping them while the decoder is busy, so there is nothing to gain by
/// backing off further.
const INTERIM_SLOWEST: Duration = INTERIM_EVERY;
/// The fastest. Below this the words on screen churn faster than anyone reads
/// them, the detector throttles the extra away at 250 ms anyway, and the CPU is
/// wanted for the settled decode that follows.
const INTERIM_FASTEST: Duration = Duration::from_millis(600);
/// What fraction of the time a machine may spend on interims: one part decoding
/// to two parts not. An interim is a courtesy and the settled utterance is the
/// transcript, so the courtesy never gets the majority of the processor.
const INTERIM_DUTY: u32 = 3;
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
    let mut found: Vec<ModelInfo> = Vec::new();

    if let Ok(response) = crate::tls::client()
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

/// How long to wait before decoding the growing buffer again, from how long the
/// last such decode took on this machine.
///
/// Nothing measured yet means the unhurried default: the first interim of a
/// session should not be the one that finds out the machine is slow.
fn interim_pace(last_decode_ms: usize) -> Duration {
    if last_decode_ms == 0 {
        return INTERIM_EVERY;
    }
    Duration::from_millis(last_decode_ms as u64)
        .checked_mul(INTERIM_DUTY)
        .unwrap_or(INTERIM_SLOWEST)
        .clamp(INTERIM_FASTEST, INTERIM_SLOWEST)
}

/// The sizes that are ever chosen for somebody, smallest first.
///
/// whisper.cpp publishes far more than this — quantised builds, `large-v3`,
/// `large-v3-turbo` — and all of them remain selectable by hand. These four are
/// the ones whose behaviour on ordinary church hardware is predictable enough to
/// pick unattended; `medium` is here only so a machine that is running it can be
/// stepped down from, never as an automatic choice.
const LADDER: [&str; 4] = ["tiny", "base", "small", "medium"];

/// What one decode costs this machine, in seconds of work per window.
///
/// # What this is, and what it is not
///
/// It is the cost of putting one utterance through the model, whatever length
/// that utterance is. It is **not** a real-time factor, and the version that
/// claimed to be one was wrong in two ways that both flattered the machine.
///
/// It decoded five seconds of digital silence and divided the time by five. But
/// whisper pads whatever it is handed up to a thirty-second window and encodes
/// the whole window, so the work does not vary with the length of the clip --
/// only the divisor does. Measured on an Apple laptop against `base.en`, one
/// window costs about 0.07 s and the same machine reported 16x, 68x, 224x or
/// 422x real time purely according to whether it was handed one second, five,
/// fifteen or twenty-nine. The published figure was a property of the number 5.
///
/// And silence is the cheapest thing there is: whisper short-circuits when it
/// hears no speech, so the decoder -- which runs once per token it produces --
/// never runs at all. Real speech through the same window cost 2.5x as much on
/// that machine, and the gap widens on a processor with no GPU behind it.
///
/// Together those are enough for a machine to measure thirty times faster than
/// speech and still fall behind a preacher, which is exactly what one did.
///
/// # So what should be trusted instead
///
/// [`Local::sustained`], once a session has run for a minute: real speech, at
/// the lengths people really speak in, on this machine. This function remains
/// for the one thing it can honestly answer -- roughly what a single utterance
/// costs, before anybody has spoken, which is enough to warn that a model is far
/// too large for the machine it has been put on.
pub fn measure(data_dir: &Path, file: &str) -> Result<f32> {
    if !is_installed(data_dir, file) {
        return Err(anyhow!("{file} is not downloaded"));
    }

    let engine = Local::new(data_dir.to_path_buf(), file.to_string());
    engine.ensure(&|_| {})?;

    // A full window, because that is what every decode costs regardless.
    // Anything shorter measures the same work and invites dividing by a
    // smaller number, which is the mistake this replaces.
    let samples = vec![0.0f32; (TARGET_SAMPLE_RATE as f32 * WINDOW_SECONDS) as usize];

    // The first decode loads and warms; the second is the one worth timing.
    let _ = engine.transcribe(&samples);
    let began = Instant::now();
    engine.transcribe(&samples)?;

    /*
     * Reported against a window an utterance-sized fraction of which is
     * typical, not against thirty seconds.
     *
     * A church's utterances are a few seconds each and each one costs a whole
     * window, so dividing by thirty would understate the load by the same
     * factor the old code overstated it. `TYPICAL_UTTERANCE` is what a settled
     * stretch of preaching actually runs to, and it is the honest divisor until
     * `sustained` has real speech to replace all of this with.
     */
    Ok(began.elapsed().as_secs_f32() / TYPICAL_UTTERANCE)
}

/// The window whisper pads every decode to. Not configurable; it is the shape
/// of the model.
const WINDOW_SECONDS: f32 = 30.0;

/// How long a settled stretch of preaching tends to run.
///
/// A guess, and marked as one -- it is only the divisor for the cold estimate
/// above, and [`Local::sustained`] replaces it with the truth as soon as
/// anybody speaks.
const TYPICAL_UTTERANCE: f32 = 6.0;

/// What a measured real-time factor means for a service, in a sentence.
pub fn describe_speed(factor: f32) -> String {
    let times = if factor > 0.0 { 1.0 / factor } else { 0.0 };
    if factor <= 0.35 {
        format!("This machine transcribes about {times:.0}x faster than speech — comfortable.")
    } else if factor <= 0.7 {
        format!(
            "This machine transcribes about {times:.1}x faster than speech. That is enough, but \
             not by much: a long service may start to fall behind."
        )
    } else {
        format!(
            "This machine transcribes about {times:.1}x faster than speech, which is not enough \
             for a live service. A smaller model may help; a subscription or an Azure key \
             transcribes on our machines instead."
        )
    }
}

/// Whether this machine is likely to keep up with a preacher.
///
/// # What actually decides it
///
/// The CPU, and only the CPU. `whisper-rs` is built here with
/// `default-features = false`, which compiles in no Metal, no CUDA and no
/// CoreML — so decoding runs on the processor on every platform, and a discrete
/// graphics card contributes nothing at all. Advice that sends a church out to
/// buy a gaming laptop for the GPU is advice to spend money on a part this
/// build cannot use.
///
/// So: Apple silicon, which is fast at this because of wide vector units and
/// memory bandwidth rather than anything exotic; or an x86 machine with enough
/// cores to be a workstation rather than an office laptop.
///
/// # It is a guess, and it is allowed to be
///
/// Nothing is refused on the strength of it. It decides whether an operator is
/// warned before Sunday rather than after one — the engine already reports when
/// it is running behind, and that report is the truth. This only exists so the
/// truth does not arrive for the first time during a sermon.
pub fn likely_keeps_up() -> bool {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

    // Apple silicon: every model from the M1 up decodes faster than real time
    // on the sizes this ships.
    if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        return true;
    }

    /*
     * Everything else is judged on core count, and the bar is high because the
     * number lies about thin-and-lights.
     *
     * A Dell XPS advertises twelve to sixteen threads and could not keep up at
     * all. Sixteen genuine cores is a desktop or a mobile workstation; twelve
     * threads is an ultrabook. The line goes above the machines that report a
     * flattering number, which means some capable laptops are told they are
     * marginal -- a warning they can ignore, against a promise that fails
     * during a sermon.
     */
    cores >= 16
}

/// The model to start with on this machine, when nobody has chosen one.
///
/// Sized from the logical core count, because with no GPU backend compiled in
/// this decodes on the CPU everywhere and throughput is what decides whether a
/// verse arrives during the sentence or after it.
///
/// The bands are deliberately cautious. Guessing too small costs accuracy on
/// names and places — real, but quiet, and the operator can raise it once they
/// have seen it work. Guessing too large costs a transcript running minutes
/// behind a preacher, which the whole congregation sees and which reads as the
/// software being broken. Those two are not worth trading evenly, so the bands
/// sit a size below what a benchmark alone would suggest — this machine is also
/// encoding video or driving a projector while it decodes.
///
/// `small` is the ceiling. `medium` is 1.5 GB to fetch and beyond most of the
/// laptops this exists for, and a download that large should be somebody's
/// decision rather than a consequence of pressing Start.
pub fn recommended(locale: &str) -> String {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);

    /*
     * Thread count is not comparable across architectures, and an earlier
     * version of this treated it as though it were.
     *
     * A Dell XPS reports twelve to sixteen hardware threads and is a
     * thin-and-light: a handful of performance cores, the rest efficiency
     * cores, in a chassis that throttles under sustained load. An Apple silicon
     * laptop reports eight and is several times quicker at this. Counting
     * threads alone handed that XPS `small` -- 466 MB of model on cores that
     * could not carry it -- and the result was reported, accurately, as
     * "totally useless".
     *
     * So the ceiling is per architecture. Apple silicon may have `small`
     * because every machine in that family can run it. Everything else stops
     * at `base` however many threads it advertises, because the number does not
     * distinguish a workstation from a laptop pretending to be one, and being
     * wrong downward costs some accuracy while being wrong upward costs the
     * whole feature.
     *
     * The engine still says when it is falling behind, and that report remains
     * the only thing here that is measured rather than guessed.
     */
    let apple_silicon = cfg!(all(target_arch = "aarch64", target_os = "macos"));

    let size = match (apple_silicon, cores) {
        (_, 0..=3) => "tiny",
        (true, 4..=9) => "base",
        (true, _) => "small",
        // Not Apple silicon: base is the ceiling, whatever it claims to have.
        (false, _) => "base",
    };
    named(size, english(locale))
}

/// The next size down from what is loaded, for a machine that cannot keep up.
///
/// Returns none at the bottom of the ladder, or for a model that is not on it —
/// somebody running `large-v3-turbo` chose it deliberately and does not need us
/// guessing at what they meant.
pub fn smaller_than(file: &str) -> Option<String> {
    let label = file.trim_start_matches("ggml-").trim_end_matches(".bin");
    let size = label.split('.').next().unwrap_or_default();
    let index = LADDER.iter().position(|rung| *rung == size)?;
    Some(named(LADDER[index.checked_sub(1)?], label.contains(".en")))
}

/// The next size up, for a machine that has proved it has room.
///
/// # Why this is measured and the first choice is not
///
/// [`recommended`] caps every machine that is not Apple silicon at `base`,
/// whatever it advertises, because a thread count does not distinguish a
/// workstation from a thin-and-light pretending to be one -- and being wrong
/// upward there cost a church the whole feature. That caution is right for a
/// first launch, when nothing is known.
///
/// It is the wrong answer for ever. The rule predates Vulkan shipping, and a
/// machine that has been running comfortably for a service has told us
/// something no core count could: [`Local::sustained`] is real speech, at real
/// lengths, on this hardware. So the ceiling is raised by evidence rather than
/// by guessing harder about the guess.
///
/// Returns none at the top of the ladder, or for a model that is not on it --
/// somebody running `large-v3-turbo` chose it deliberately.
pub fn larger_than(file: &str) -> Option<String> {
    let label = file.trim_start_matches("ggml-").trim_end_matches(".bin");
    let size = label.split('.').next().unwrap_or_default();
    let index = LADDER.iter().position(|rung| *rung == size)?;
    // `medium` is on the ladder to be stepped *down* from and has never been an
    // automatic choice; offering it is not the same as choosing it, and the
    // operator still presses the button.
    Some(named(*LADDER.get(index + 1)?, label.contains(".en")))
}

/// Sustained cost at which a machine plainly has room for the next model up.
///
/// Seconds of work per second of audio. 0.15 is nearly seven times faster than
/// speech, sustained, on real utterances -- and each rung of the ladder costs
/// roughly two to three times the one below it, so a machine at this figure has
/// the headroom for one step with margin left over. Deliberately far below the
/// 0.35 that merely reads as "comfortable": a step up that has to be undone
/// mid-service is worse than never offering it.
pub const ROOM_TO_SPARE: f32 = 0.15;

/// Whether the `.en` builds are the right family for this locale. They are
/// better at English and cannot do anything else.
fn english(locale: &str) -> bool {
    let base = locale.split(['-', '_']).next().unwrap_or_default();
    base.eq_ignore_ascii_case("en") || base.is_empty()
}

fn named(size: &str, english: bool) -> String {
    // large has no .en build, so this would name a file that does not exist --
    // but nothing above ever asks for one, and a caller that did should get
    // something fetchable rather than a 404 an hour into a service.
    if english && size != "large" {
        format!("ggml-{size}.en.bin")
    } else {
        format!("ggml-{size}.bin")
    }
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
    let target = model_path(data_dir, file);
    if is_installed(data_dir, file) {
        return Ok(target);
    }
    std::fs::create_dir_all(target.parent().expect("model path has a parent"))
        .context("could not make a place for the speech model")?;

    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or(file);
    let client = crate::tls::client()
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

/// The language whisper should be told to decode, from what the operator chose.
///
/// Settings hold a locale in Azure's spelling — `en-NG`, `fr-FR`, `yo-NG` —
/// because that is what the streaming engines want. whisper.cpp wants the bare
/// code, and rejects anything it does not know, so this takes the part before
/// the region and checks it against whisper's own list rather than trusting it.
///
/// An `.en` model is asked for English whatever the setting says. Those builds
/// cannot do anything else, and telling one to decode Yoruba does not produce
/// Yoruba — it produces English-shaped nonsense, which is worse than a setting
/// that was quietly ignored.
pub fn decode_language(locale: &str, english_only: bool) -> &'static str {
    if english_only {
        return "en";
    }

    let base = locale.split(['-', '_']).next().unwrap_or("").to_lowercase();
    match whisper_rs::get_lang_id(&base) {
        // Borrowed back from whisper's own table, so the string outlives the
        // lowercased copy made above.
        Some(id) => whisper_rs::get_lang_str(id).unwrap_or("en"),
        // Unset, or a locale this build of whisper has never heard of. English
        // is what this did before there was a setting at all, and it is a
        // better answer than refusing to transcribe.
        None => "en",
    }
}

pub struct Local {
    data_dir: PathBuf,
    threads: i32,
    /// What the operator has asked for. Compared against `loaded` at each safe
    /// point; a plain string compare, so polling it costs nothing.
    wanted: Mutex<String>,
    /// The spoken language, in Azure's spelling. Read at each utterance, so
    /// changing it in settings takes effect without restarting a session.
    language: Mutex<String>,
    loaded: Mutex<Option<Loaded>>,
    /// What this machine has actually done, from settled decodes of real
    /// speech. See [`Local::sustained`].
    sustained: Sustained,
}

/// A running record of real decodes, in milliseconds of work and of audio.
///
/// # Why a benchmark could not answer this
///
/// [`measure`] times one decode of one clip, and that turns out to say almost
/// nothing. whisper pads whatever it is given up to a thirty-second window and
/// encodes the whole thing, so the *work* is the same whether the clip is one
/// second or twenty-nine -- only the divisor changes. Measured on an Apple
/// laptop against `base.en`, one window costs about 0.07 s and the same machine
/// reports 16x, 68x, 224x or 422x real time depending only on how long a clip
/// somebody chose to hand it. Ours hands it five seconds and prints 68x.
///
/// It also decodes silence, which whisper short-circuits: real speech through
/// the same window cost 2.5x as much on that machine, and the gap is wider on a
/// processor with no GPU to fall back on, because the decoder runs once per
/// token produced and silence produces none.
///
/// Both errors point the same way, which is why a machine can measure 30x and
/// still fall behind a preacher. So the honest number is not a benchmark at
/// all: it is what this machine did to the last few minutes of somebody's
/// actual speech, at whatever lengths their utterances actually were.
#[derive(Debug, Default)]
struct Sustained {
    work_ms: AtomicUsize,
    audio_ms: AtomicUsize,
}

impl Local {
    pub fn new(data_dir: PathBuf, model_file: String) -> Self {
        // Leave a core for everything else: this runs while the machine is also
        // encoding video, and a transcript is not worth dropped frames.
        let threads = std::thread::available_parallelism()
            .map(|n| (n.get().saturating_sub(1)).clamp(1, 8))
            .unwrap_or(4) as i32;

        Self {
            sustained: Sustained::default(),
            data_dir,
            threads,
            wanted: Mutex::new(model_file),
            language: Mutex::new("en-US".to_string()),
            loaded: Mutex::new(None),
        }
    }

    /// Asks for a different model. Takes effect at the next utterance boundary.
    pub fn request(&self, model_file: &str) {
        *self.wanted.lock() = model_file.to_string();
    }

    /// Sets the spoken language, in Azure's spelling. Also at the next boundary.
    pub fn set_language(&self, locale: &str) {
        *self.language.lock() = locale.to_string();
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

    /// Decodes one stretch of audio, for the benchmark example.
    ///
    /// Public only so `examples/bench.rs` can measure this machine rather than
    /// anybody guessing at it. Loads the model on the first call, as a session
    /// does.
    /// What this machine has actually been doing, or `None` before it has done
    /// enough to say.
    ///
    /// Seconds of work per second of audio, over settled decodes of real
    /// speech at the lengths people really speak in -- which is the number
    /// [`measure`] cannot produce and the one that decides whether a service
    /// keeps up. Below 1.0 is faster than speech.
    ///
    /// `None` until a minute of audio has gone through, because the first few
    /// utterances of a session include a cold model and a cold cache and would
    /// libel the machine.
    pub fn sustained(&self) -> Option<f32> {
        let audio_ms = self.sustained.audio_ms.load(Ordering::Relaxed);
        if audio_ms < 60_000 {
            return None;
        }
        let work_ms = self.sustained.work_ms.load(Ordering::Relaxed);
        Some(work_ms as f32 / audio_ms as f32)
    }

    /// Records one settled decode against the running total.
    fn record(&self, work: Duration, samples: usize) {
        let audio_ms = samples * 1000 / TARGET_SAMPLE_RATE as usize;
        self.sustained.work_ms.fetch_add(work.as_millis() as usize, Ordering::Relaxed);
        self.sustained.audio_ms.fetch_add(audio_ms, Ordering::Relaxed);
    }

    pub fn transcribe_for_bench(&self, samples: &[f32]) -> Result<String> {
        self.ensure(&|_| {})?;
        self.transcribe(samples)
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
        // Transcribe, never translate: a church that selects Yoruba wants
        // Yoruba on the screen, not an English rendering of it.
        params.set_translate(false);
        params.set_language(Some(decode_language(
            &self.language.lock(),
            // Derived from the file name, the same way the catalogue does it.
            loaded.file.contains(".en"),
        )));
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
        // The last sustained figure said out loud, so a steady machine does not
        // narrate itself between every sentence.
        let mut reported_sustained: Option<f32> = None;
        // Once per session. An offer repeated between sentences is nagging.
        let mut offered_larger = false;
        let mut last_interim = Instant::now();
        // How many settled utterances the decoder still owes us, and whether we
        // have already said something about it.
        let mut waiting = 0usize;
        let mut warned = false;

        let (jobs, decoded, done, took_ms, worker) =
            Arc::clone(self).decoder(&stop, interim, final_text);

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

                    /*
                     * Step down, rather than advise a step down.
                     *
                     * This used to name the smaller model and leave it to the
                     * operator. That is the right *sentence* and the wrong
                     * moment: they are mid-service, watching a transcript fall
                     * further behind, and the fix is in a settings dialog
                     * behind a list of thirty files. What actually happened is
                     * that the feature was written off as useless -- correctly,
                     * from where they were standing.
                     *
                     * So it does it. Between utterances is already a safe point
                     * to change model, and a smaller one that keeps up is
                     * strictly better than a larger one that does not: the
                     * accuracy lost is nothing against a transcript arriving
                     * after the sermon.
                     *
                     * Only to a model already on disk. Downloading one takes
                     * minutes over the connection this machine has, and doing
                     * it mid-service would compete with the recogniser for the
                     * network. If there is nothing smaller here, it says so and
                     * carries on -- and provisioning fetches a smaller one for
                     * next time.
                     */
                    let current = self.loaded_model();
                    let smaller = current.as_deref().and_then(smaller_than);

                    match smaller {
                        Some(smaller) if is_installed(&self.data_dir, &smaller) => {
                            note(format!(
                                "Transcription was running behind, so it has switched to \
                                 {smaller}, which this machine can keep up with. Change it \
                                 under Settings if you would rather have the larger one."
                            ));
                            self.request(&smaller);
                        }
                        Some(smaller) => note(format!(
                            "Transcription is running behind: {waiting} utterances are still \
                             being decoded. {smaller} would keep up, and is not downloaded \
                             yet -- fetch it under Settings before the next service."
                        )),
                        None => note(format!(
                            "Transcription is running behind: {waiting} utterances are still \
                             being decoded, and this is already the smallest model. A \
                             subscription or an Azure key transcribes on our machines instead."
                        )),
                    }
                }
                /*
                 * What this machine is actually doing, said once when it is
                 * first known and then only when it changes materially.
                 *
                 * The plan's first instruction for this was to instrument a
                 * real session and find where it diverges, and this is that
                 * line: real speech, at the lengths the preacher actually
                 * speaks in, on their hardware. It is the number the settings
                 * dialog should eventually show instead of a benchmark, and
                 * until it does, it is the number to ask an operator for when
                 * they report falling behind.
                 */
                if let Some(factor) = self.sustained() {
                    let moved = reported_sustained.is_none_or(|last: f32| {
                        (factor - last).abs() > (last * 0.2).max(0.02)
                    });
                    if moved {
                        reported_sustained = Some(factor);
                        note(format!(
                            "This machine is sustaining {:.1}x real time on real speech.",
                            if factor > 0.0 { 1.0 / factor } else { 0.0 }
                        ));
                    }

                    /*
                     * A machine with room to spare, told once.
                     *
                     * The mirror of the step-down above, and the only honest
                     * way to raise a ceiling that is otherwise a guess from the
                     * core count. `recommended` caps everything that is not
                     * Apple silicon at `base` because a thread count cannot
                     * tell a workstation from a thin-and-light -- but a machine
                     * that has just run a service on real speech has answered
                     * that question itself.
                     *
                     * Said, not done. Stepping down is a rescue and happens
                     * without asking; stepping up is an improvement, costs a
                     * download, and belongs to the operator. It is also said
                     * exactly once per session: an offer repeated between
                     * sentences is nagging.
                     */
                    if !offered_larger && factor <= ROOM_TO_SPARE {
                        offered_larger = true;
                        if let Some(larger) = self.loaded_model().as_deref().and_then(larger_than) {
                            note(format!(
                                "This machine has room to spare. {larger} would be more accurate \
                                 and it can carry it -- fetch it under Settings before the next \
                                 service."
                            ));
                        }
                    }
                }

                silence = Duration::ZERO;
                spoke = false;
                last_interim = Instant::now();

                // Between utterances is the only safe moment to change model:
                // nothing is part-decoded and no audio is waiting on it.
                self.ensure(&note)?;
                continue;
            }

            /*
             * Something on screen while the sentence is still being said, and
             * the first thing to go when the machine cannot keep up. Skipped
             * outright while the decoder is busy: an interim is a courtesy, and
             * queueing them is what turns a slow machine into a stuck one --
             * each decode covers a longer buffer than the last, so every one
             * takes longer than the interval that triggers it.
             *
             * How often is measured rather than fixed. 2.2 seconds was one
             * number for every machine, and it was chosen for the slowest: on an
             * Apple silicon laptop running tiny.en a partial decode is over in a
             * fraction of a second, and waiting out the rest of the interval is
             * two seconds a verse arrives later than it needed to. So the pace
             * follows the last decode -- three times however long it took --
             * between the floor and what it always used to be.
             */
            let pace = interim_pace(took_ms.load(Ordering::Relaxed));
            if spoke
                && held >= MIN_UTTERANCE
                && last_interim.elapsed() >= pace
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
    /// Returns the queue, whether it is busy, how many settled utterances it
    /// has finished, how long the last partial decode took in milliseconds, and
    /// the thread itself.
    fn decoder(
        self: Arc<Self>,
        stop: &Arc<AtomicBool>,
        interim: impl Fn(String) + Send + 'static,
        final_text: impl Fn(String) + Send + 'static,
    ) -> (
        std::sync::mpsc::Sender<Job>,
        Arc<AtomicBool>,
        Arc<AtomicUsize>,
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
        // Zero until the first partial decode has been timed, which is what the
        // pacing above reads as "nothing measured yet".
        let took_ms = Arc::new(AtomicUsize::new(0));

        let working = Arc::clone(&busy);
        let counted = Arc::clone(&finished);
        let timed = Arc::clone(&took_ms);
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
                    let began = Instant::now();
                    let spoken = engine.transcribe(audio);
                    working.store(false, Ordering::Relaxed);

                    // Only the settled ones, and only when they produced text.
                    // An interim covers a stretch that will be decoded again,
                    // so counting both would charge this machine twice for the
                    // same seconds of a sermon.
                    if matches!(job, Job::Settled(_)) && spoken.as_deref().is_ok_and(|t| !t.trim().is_empty()) {
                        engine.record(began.elapsed(), audio.len());
                    }

                    // Only the partials. A settled decode covers the whole
                    // utterance and so takes longer than any interim of it
                    // will, and pacing interims off that number would make a
                    // fast machine wait for no reason.
                    if matches!(job, Job::Partial(_)) {
                        timed.store(
                            began.elapsed().as_millis().min(usize::MAX as u128) as usize,
                            Ordering::Relaxed,
                        );
                    }

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

        (tx, busy, finished, took_ms, worker)
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
    fn language_is_the_bare_code_whisper_knows() {
        // Settings hold Azure's spelling; whisper wants the part before the
        // region and nothing else.
        assert_eq!(decode_language("en-US", false), "en");
        assert_eq!(decode_language("en-NG", false), "en");
        assert_eq!(decode_language("fr-FR", false), "fr");
        assert_eq!(decode_language("yo-NG", false), "yo");
        assert_eq!(decode_language("zh-CN", false), "zh");
        // Underscores too, since a locale reaches us from more than one place.
        assert_eq!(decode_language("pt_BR", false), "pt");
    }

    #[test]
    fn an_english_only_model_is_never_asked_for_anything_else() {
        // Those builds cannot do it. Told to decode Yoruba, one produces
        // English-shaped nonsense rather than Yoruba, which is worse than a
        // setting quietly ignored.
        assert_eq!(decode_language("yo-NG", true), "en");
        assert_eq!(decode_language("fr-FR", true), "en");
    }

    #[test]
    fn an_unknown_locale_falls_back_rather_than_failing() {
        // whisper rejects a code it does not know, and refusing to transcribe
        // is a worse answer than transcribing in the language this always used.
        assert_eq!(decode_language("", false), "en");
        assert_eq!(decode_language("qq-ZZ", false), "en");
        assert_eq!(decode_language("klingon", false), "en");
    }

    #[test]
    fn interims_go_as_fast_as_the_machine_affords_and_no_faster() {
        // Nothing measured: the unhurried default, which is what this did at
        // every speed before it was measured at all.
        assert_eq!(interim_pace(0), INTERIM_EVERY);

        // A fast machine -- Apple silicon on tiny.en, a partial decode over in
        // 120 ms -- gets the floor rather than two seconds of waiting.
        assert_eq!(interim_pace(120), INTERIM_FASTEST);

        // In between, three times the decode: one part working, two parts not.
        assert_eq!(interim_pace(400), Duration::from_millis(1_200));

        // A slow one never goes slower than it always did. It is already
        // skipping interims while the decoder is busy, so backing off further
        // buys nothing.
        assert_eq!(interim_pace(5_000), INTERIM_SLOWEST);
        assert_eq!(interim_pace(usize::MAX), INTERIM_SLOWEST);
    }

    #[test]
    fn a_thin_laptop_is_never_handed_a_model_it_cannot_carry() {
        // The bug this replaced: a Dell XPS reports twelve to sixteen threads
        // from a chip with a few performance cores and the rest efficiency
        // cores, was handed `small`, and was accurately described as totally
        // useless. Thread count does not distinguish a workstation from an
        // ultrabook, so off Apple silicon nothing above `base` is ever chosen
        // for somebody.
        if !cfg!(all(target_arch = "aarch64", target_os = "macos")) {
            let chosen = recommended("en-GB");
            assert!(
                chosen.contains("tiny") || chosen.contains("base"),
                "{chosen} is too large to choose unattended on this architecture",
            );
        }
    }

    #[test]
    fn the_capability_guess_is_about_the_processor() {
        // Whatever it answers on the machine running the test, it must answer
        // *something* -- and the answer must not depend on a graphics card,
        // because this build compiles no GPU backend and cannot use one.
        let _ = likely_keeps_up();

        // On Apple silicon it is always yes; the smallest one is quick enough.
        if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
            assert!(likely_keeps_up());
        }
    }

    #[test]
    fn the_recommendation_is_a_model_that_exists() {
        // Whatever this machine has, the answer has to be a file whisper.cpp
        // actually publishes -- it is fetched without anybody checking it.
        let english = recommended("en-NG");
        assert!(english.starts_with("ggml-") && english.ends_with(".en.bin"), "{english}");
        assert!(
            LADDER.iter().any(|size| english == format!("ggml-{size}.en.bin")),
            "{english} is not on the ladder",
        );
        // Never medium: 1.5 GB is somebody's decision, not a consequence of
        // pressing Start.
        assert_ne!(english, "ggml-medium.en.bin");
    }

    #[test]
    fn a_church_not_working_in_english_is_not_given_an_english_only_model() {
        // The .en builds cannot decode anything else, so choosing one for a
        // Yoruba service would be choosing a transcript of nonsense.
        assert!(!recommended("yo-NG").contains(".en."));
        assert!(!recommended("fr-FR").contains(".en."));
        // Unset means English, which is what everything else here assumes.
        assert!(recommended("").contains(".en."));
    }

    #[test]
    fn falling_behind_names_the_next_size_down() {
        assert_eq!(smaller_than("ggml-small.en.bin").as_deref(), Some("ggml-base.en.bin"));
        assert_eq!(smaller_than("ggml-medium.bin").as_deref(), Some("ggml-small.bin"));
        assert_eq!(smaller_than("ggml-base.en.bin").as_deref(), Some("ggml-tiny.en.bin"));
        // Already the smallest there is: there is no advice left to give, and
        // inventing some would send the operator looking for a file.
        assert_eq!(smaller_than("ggml-tiny.en.bin"), None);

        // And back up the same rungs, for a machine that has earned it.
        assert_eq!(larger_than("ggml-tiny.en.bin").as_deref(), Some("ggml-base.en.bin"));
        assert_eq!(larger_than("ggml-base.en.bin").as_deref(), Some("ggml-small.en.bin"));
        assert_eq!(larger_than("ggml-small.bin").as_deref(), Some("ggml-medium.bin"));
        // The top of the ladder, and anything chosen by hand from outside it.
        assert_eq!(larger_than("ggml-medium.en.bin"), None);
        assert_eq!(larger_than("ggml-large-v3-turbo.bin"), None);
        // Chosen deliberately and off the ladder. Guessing what somebody
        // running large-v3-turbo meant is not our business.
        assert_eq!(smaller_than("ggml-large-v3-turbo.bin"), None);
    }

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
