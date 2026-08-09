/**
 * The Azure Speech bridge, shared by Castavox and Pulpitry.
 *
 * Reads raw PCM on stdin (16 kHz, 16-bit little-endian, mono — written by the
 * Rust capture thread), streams it to Azure Speech, and writes newline-delimited
 * JSON transcript events to stdout.
 *
 * # Canonical source
 *
 * This file lives in castavox-core. The copies at
 * Pulpitry/src-sidecar/src/index.js and Castavox/sidecar/speech-node/src/index.js
 * are generated from it by `npm run sync-bridge`, and each app's build refuses
 * to bundle a copy that has been edited by hand. Change this file, sync both.
 *
 * It is shared because the two copies had already become the same file to the
 * byte, which meant every fix had to be remembered twice and a forgotten one
 * would be found by a church rather than by us.
 *
 * # Why stdin rather than the SDK's own microphone input
 *
 * The JS SDK implements mic capture against `navigator.mediaDevices`, which does
 * not exist in Node, so `AudioConfig.fromDefaultMicrophoneInput()` cannot work
 * here. Capture lives in Rust instead, which also gives the operator UI real
 * device enumeration.
 *
 * stdout carries protocol messages only. Everything else goes to stderr.
 *
 * Messages emitted:
 *   {"type":"listening"}
 *   {"type":"recognizing","text":"..."}
 *   {"type":"recognized","text":"...","offsetMs":n,"durationMs":n}
 *   {"type":"reconnecting","message":"..."}
 *   {"type":"error","message":"...","fatal":bool}
 */

"use strict";

// Guard stdout: anything that logs by accident must not corrupt the stream.
console.log = (...args) => console.error(...args);

const sdk = require("microsoft-cognitiveservices-speech-sdk");

const KEY = process.env.CASTAVOX_SPEECH_KEY || "";
const REGION = process.env.CASTAVOX_SPEECH_REGION || "";
const LANGUAGE = process.env.CASTAVOX_SPEECH_LANGUAGE || "en-US";
const SAMPLE_RATE = Number(process.env.CASTAVOX_SPEECH_SAMPLE_RATE) || 16000;

const RESTART_DELAY_MS = 1200;
const AUDIO_FORMAT = sdk.AudioStreamFormat.getWaveFormatPCM(SAMPLE_RATE, 16, 1);

/** Cancellation codes that retrying will never fix. */
const FATAL_ERROR_CODES = new Set([
  sdk.CancellationErrorCode.AuthenticationFailure,
  sdk.CancellationErrorCode.Forbidden,
  sdk.CancellationErrorCode.BadRequestParameters,
]);

let pushStream = null;
let recognizer = null;
let restartTimer = null;
let shuttingDown = false;

function emit(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function fail(message, fatal) {
  emit({ type: "error", message, fatal: Boolean(fatal) });
}

function buildRecognizer() {
  const speechConfig = sdk.SpeechConfig.fromSubscription(KEY, REGION);
  speechConfig.speechRecognitionLanguage = LANGUAGE;
  // Preaching pauses for effect; don't cut a sentence short on a short silence.
  speechConfig.setProperty(sdk.PropertyId.Speech_SegmentationSilenceTimeoutMs, "800");

  pushStream = sdk.AudioInputStream.createPushStream(AUDIO_FORMAT);
  const audioConfig = sdk.AudioConfig.fromStreamInput(pushStream);
  const speech = new sdk.SpeechRecognizer(speechConfig, audioConfig);

  speech.recognizing = (_sender, event) => {
    const text = event.result && event.result.text;
    if (text) emit({ type: "recognizing", text });
  };

  speech.recognized = (_sender, event) => {
    const result = event.result;
    if (!result || result.reason !== sdk.ResultReason.RecognizedSpeech) return;
    if (!result.text) return;
    emit({
      type: "recognized",
      text: result.text,
      // The SDK reports ticks (100 ns); the UI wants milliseconds.
      offsetMs: Math.round(Number(result.offset) / 10000),
      durationMs: Math.round(Number(result.duration) / 10000),
    });
  };

  speech.canceled = (_sender, event) => {
    if (event.reason !== sdk.CancellationReason.Error) return;
    const detail = event.errorDetails || "the speech service closed the connection";
    if (FATAL_ERROR_CODES.has(event.errorCode)) {
      fail(`Azure rejected the connection: ${detail}`, true);
      shutdown(1);
      return;
    }
    scheduleRestart(detail);
  };

  speech.sessionStarted = () => emit({ type: "listening" });

  return speech;
}

function startRecognition() {
  try {
    recognizer = buildRecognizer();
  } catch (err) {
    fail(`Could not initialise the speech recogniser: ${describe(err)}`, true);
    shutdown(1);
    return;
  }

  recognizer.startContinuousRecognitionAsync(
    () => {},
    (err) => scheduleRestart(describe(err)),
  );
}

/**
 * Rebuilds the recogniser after a recoverable failure. Closing a recogniser
 * also closes its push stream, so a fresh stream is created each time and
 * stdin is re-pointed at it.
 */
function scheduleRestart(reason) {
  if (shuttingDown || restartTimer) return;
  emit({ type: "reconnecting", message: reason });

  const dying = recognizer;
  recognizer = null;
  pushStream = null;
  if (dying) {
    try {
      dying.close();
    } catch {
      /* already torn down */
    }
  }

  restartTimer = setTimeout(() => {
    restartTimer = null;
    if (!shuttingDown) startRecognition();
  }, RESTART_DELAY_MS);
}

function describe(err) {
  if (!err) return "unknown error";
  return err.message || String(err);
}

function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  if (restartTimer) clearTimeout(restartTimer);

  const finish = () => process.exit(code || 0);
  if (!recognizer) return finish();

  try {
    recognizer.stopContinuousRecognitionAsync(
      () => {
        try {
          recognizer.close();
        } catch {
          /* ignore */
        }
        finish();
      },
      finish,
    );
  } catch {
    finish();
  }
  // Never hang the parent waiting on a clean close.
  setTimeout(finish, 1500).unref();
}

function main() {
  if (!KEY || !REGION) {
    fail("The Azure Speech key and region must both be provided.", true);
    process.exit(1);
  }

  process.stdin.on("data", (chunk) => {
    if (!pushStream || shuttingDown) return;
    try {
      // The SDK wants an ArrayBuffer; slice so a pooled Buffer's neighbours
      // don't get sent along with it.
      pushStream.write(
        chunk.buffer.slice(chunk.byteOffset, chunk.byteOffset + chunk.byteLength),
      );
    } catch (err) {
      scheduleRestart(`audio stream rejected input: ${describe(err)}`);
    }
  });

  process.stdin.on("end", () => shutdown(0));
  process.stdin.on("error", () => shutdown(0));
  process.on("SIGTERM", () => shutdown(0));
  process.on("SIGINT", () => shutdown(0));
  process.on("uncaughtException", (err) => {
    fail(`sidecar crashed: ${describe(err)}`, false);
    shutdown(1);
  });

  startRecognition();
}

main();
