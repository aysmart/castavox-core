//! Microphone capture.
//!
//! The Azure Speech JS SDK cannot open a microphone under Node — its mic source
//! is implemented against `navigator.mediaDevices`, which does not exist outside
//! a browser. So capture lives here instead: cpal reads the selected input
//! device (CoreAudio on macOS, WASAPI on Windows), converts to the mono 16 kHz
//! 16-bit PCM that Azure expects, and the samples are streamed to the sidecar's
//! stdin. Doing it this way also gives us native device enumeration, which is
//! what backs the mic selector in the operator UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig, SupportedStreamConfig};
use serde::Serialize;

/// Azure Speech is fed 16 kHz, 16-bit, single-channel PCM.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    /// cpal's stable device id, serialised. Safe to persist across restarts.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().and_then(|device| device.id().ok());

    let mut devices = Vec::new();
    for device in host.input_devices().context("could not enumerate input devices")? {
        let Ok(id) = device.id() else { continue };
        // Skip anything that cannot actually produce input; it would only fail
        // later, once the operator had already selected it.
        if device.default_input_config().is_err() {
            continue;
        }
        devices.push(AudioDevice {
            is_default: default_id.as_ref() == Some(&id),
            id: id.to_string(),
            // `Device` renders as its human-readable name.
            name: device.to_string(),
        });
    }
    Ok(devices)
}

fn open_device(device_id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();

    if let Some(raw) = device_id.filter(|id| !id.is_empty()) {
        if let Ok(id) = raw.parse::<cpal::DeviceId>() {
            if let Some(device) = host.device_by_id(&id) {
                return Ok(device);
            }
        }
        // The saved device is gone — unplugged between services, say. Falling
        // back to the default beats refusing to start.
        crate::log_line!("[audio] saved input device is unavailable; using the system default");
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("no microphone is available"))
}

/// Prefer a configuration the device can deliver at 16 kHz so no resampling is
/// needed at all; otherwise take the device default and resample.
fn pick_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    if let Ok(ranges) = device.supported_input_configs() {
        let mut best: Option<cpal::SupportedStreamConfigRange> = None;
        for range in ranges {
            let supports_target = range.min_sample_rate() <= TARGET_SAMPLE_RATE
                && TARGET_SAMPLE_RATE <= range.max_sample_rate();
            if !supports_target {
                continue;
            }
            // Fewest channels wins — we only need mono.
            let better = best.as_ref().is_none_or(|current| range.channels() < current.channels());
            if better {
                best = Some(range);
            }
        }
        if let Some(range) = best {
            return Ok(range.with_sample_rate(TARGET_SAMPLE_RATE));
        }
    }
    device
        .default_input_config()
        .context("device reported no usable input configuration")
}

/// Downmixes to mono and resamples to [`TARGET_SAMPLE_RATE`].
///
/// Downsampling averages each window of input samples rather than picking one,
/// which gives a cheap anti-aliasing filter — plain decimation folds noise into
/// the speech band and measurably hurts recognition.
struct Converter {
    channels: usize,
    /// Input samples consumed per output sample.
    step: f64,
    phase: f64,
    accumulator: f32,
    accumulated: u32,
}

impl Converter {
    fn new(src_rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            step: src_rate as f64 / TARGET_SAMPLE_RATE as f64,
            phase: 0.0,
            accumulator: 0.0,
            accumulated: 0,
        }
    }

    fn convert(&mut self, samples: &[f32]) -> Vec<i16> {
        let frames = samples.len() / self.channels;
        let mut out = Vec::with_capacity((frames as f64 / self.step.max(1.0)).ceil() as usize + 2);

        for frame in samples.chunks_exact(self.channels) {
            let mono = frame.iter().copied().sum::<f32>() / self.channels as f32;

            // At or above the target rate there is nothing to average.
            if self.step <= 1.0 {
                out.push(to_i16(mono));
                continue;
            }

            self.accumulator += mono;
            self.accumulated += 1;
            self.phase += 1.0;
            if self.phase >= self.step {
                out.push(to_i16(self.accumulator / self.accumulated as f32));
                self.accumulator = 0.0;
                self.accumulated = 0;
                self.phase -= self.step;
            }
        }
        out
    }
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Opens `device_id` and streams converted PCM into `sink` until `stop` is set.
///
/// Returns the opened device's name once the stream is confirmed running, so a
/// bad device surfaces as a real error to the operator instead of silence.
pub fn start_capture(
    device_id: Option<String>,
    sink: SyncSender<Vec<i16>>,
    stop: Arc<AtomicBool>,
) -> Result<String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();

    std::thread::Builder::new()
        .name("pulpitry-audio".into())
        .spawn(move || {
            // cpal's Stream is not Send on every platform, so it is built, kept,
            // and dropped entirely within this thread.
            match build_stream(device_id.as_deref(), sink, &stop) {
                Ok((stream, name)) => {
                    if stream.play().is_err() {
                        let _ = ready_tx.send(Err("could not start the audio stream".into()));
                        return;
                    }
                    let _ = ready_tx.send(Ok(name));
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err.to_string()));
                }
            }
        })
        .context("could not start the audio capture thread")?;

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(name)) => Ok(name),
        Ok(Err(message)) => bail!(message),
        Err(_) => bail!("timed out opening the microphone"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift(actual: i16, expected: i16) -> i32 {
        (actual as i32 - expected as i32).abs()
    }

    #[test]
    fn downsamples_48k_to_16k() {
        let mut converter = Converter::new(48_000, 1);
        let one_second = vec![0.5_f32; 48_000];

        let out = converter.convert(&one_second);

        assert!(
            (out.len() as i32 - 16_000).abs() <= 1,
            "expected ~16000 samples, got {}",
            out.len()
        );
        // Averaging a constant signal must return that same constant.
        assert!(out.iter().all(|&s| drift(s, to_i16(0.5)) <= 1));
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        let mut converter = Converter::new(TARGET_SAMPLE_RATE, 2);
        // Hard-panned: silent left, full right. Mono should land halfway.
        let out = converter.convert(&[0.0, 1.0, 0.0, 1.0]);

        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|&s| drift(s, to_i16(0.5)) <= 1));
    }

    #[test]
    fn passes_through_at_the_target_rate() {
        let mut converter = Converter::new(TARGET_SAMPLE_RATE, 1);
        assert_eq!(converter.convert(&[0.25_f32; 100]).len(), 100);
    }

    #[test]
    fn keeps_rate_across_callback_boundaries() {
        // The phase accumulator has to carry between callbacks, otherwise a
        // non-integer ratio drifts and the audio slowly desynchronises.
        let mut converter = Converter::new(44_100, 1);
        let produced: usize = (0..100).map(|_| converter.convert(&[0.1_f32; 441]).len()).sum();

        // 44_100 input samples at 44.1k -> 1 second -> 16_000 output samples.
        assert!((produced as i32 - 16_000).abs() <= 2, "produced {produced}");
    }

    #[test]
    fn clamps_out_of_range_samples() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), -i16::MAX);
    }
}

fn build_stream(
    device_id: Option<&str>,
    sink: SyncSender<Vec<i16>>,
    stop: &Arc<AtomicBool>,
) -> Result<(cpal::Stream, String)> {
    let device = open_device(device_id)?;
    let device_name = device.to_string();
    let supported = pick_config(&device)?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;

    let mut converter = Converter::new(config.sample_rate, channels);
    let stop_on_error = Arc::clone(stop);
    let error_handler = move |err| {
        crate::log_line!("[audio] input stream error: {err}");
        // A device that has gone away will never recover on its own; dropping
        // out lets the supervisor tear the session down and start a fresh one.
        stop_on_error.store(true, Ordering::SeqCst);
    };

    // `sink` is bounded: if the sidecar stalls we drop audio rather than grow a
    // queue forever, which would otherwise turn into unbounded transcript lag.
    macro_rules! build {
        ($sample:ty, $to_f32:expr) => {{
            let to_f32: fn($sample) -> f32 = $to_f32;
            device.build_input_stream(
                config.clone(),
                move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<f32> = data.iter().copied().map(to_f32).collect();
                    let pcm = converter.convert(&mono);
                    if !pcm.is_empty() {
                        let _ = sink.try_send(pcm);
                    }
                },
                error_handler,
                None,
            )?
        }};
    }

    let stream = match sample_format {
        SampleFormat::F32 => build!(f32, |s| s),
        SampleFormat::I16 => build!(i16, |s| s as f32 / 32_768.0),
        SampleFormat::U16 => build!(u16, |s| (s as f32 - 32_768.0) / 32_768.0),
        other => bail!("unsupported microphone sample format: {other:?}"),
    };

    Ok((stream, device_name))
}
