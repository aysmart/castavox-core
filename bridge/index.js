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
 *   {"type":"session","id":"..."}            hosted only, before anything else
 *   {"type":"listening"}
 *   {"type":"recognizing","text":"..."}
 *   {"type":"recognized","text":"...","offsetMs":n,"durationMs":n}
 *   {"type":"reconnecting","message":"..."}
 *   {"type":"error","message":"...","fatal":bool}
 *
 * # Two ways to authenticate
 *
 * With the church's own Azure key (CASTAVOX_SPEECH_KEY), which is how this has
 * always worked, or on a hosted subscription (CASTAVOX_BROKER_URL and
 * CASTAVOX_DEVICE_TOKEN), where the broker hands out a ten-minute Azure token
 * against a plan it meters.
 *
 * The hosted session lives here rather than in Rust because this process's
 * lifetime *is* the session: it opens one before listening, renews it on a
 * heartbeat, and closes it on the way out. Splitting that across two processes
 * would mean inventing a control channel down a pipe already carrying PCM, and
 * would leave sessions open whenever the two disagreed about what was running.
 *
 * The session id is announced on stdout all the same, because a process that
 * is killed cannot close anything and this one can be: the parent uses it to
 * close the session as a backstop once we are gone. Closing twice is safe --
 * the broker treats the second as already done.
 *
 * Our own Azure key is never here. What arrives is short-lived, scoped and
 * revocable, and the device token that fetches it is never written to stdout.
 */

"use strict";

// Guard stdout: anything that logs by accident must not corrupt the stream.
console.log = (...args) => console.error(...args);

const sdk = require("microsoft-cognitiveservices-speech-sdk");

const KEY = process.env.CASTAVOX_SPEECH_KEY || "";
const REGION = process.env.CASTAVOX_SPEECH_REGION || "";
const LANGUAGE = process.env.CASTAVOX_SPEECH_LANGUAGE || "en-US";
const SAMPLE_RATE = Number(process.env.CASTAVOX_SPEECH_SAMPLE_RATE) || 16000;

/** Set together when the church is on a hosted subscription rather than its own key. */
const BROKER = (process.env.CASTAVOX_BROKER_URL || "").replace(/\/+$/, "");
const DEVICE_TOKEN = process.env.CASTAVOX_DEVICE_TOKEN || "";
const HOSTED = Boolean(BROKER && DEVICE_TOKEN);

const RESTART_DELAY_MS = 1200;
const REQUEST_TIMEOUT_MS = 15000;
/** Long enough for a heartbeat to be attempted, short enough not to hang a quit. */
const SHUTDOWN_GRACE_MS = HOSTED ? 4000 : 1500;
/** Two bytes a frame, mono: what a second of the audio we are sending weighs. */
const BYTES_PER_SECOND = SAMPLE_RATE * 2;
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

/** Hosted state. All of it stays null on a church's own key. */
let session = null;
let authToken = "";
let hostedRegion = "";
let heartbeatTimer = null;
/** Streamed and already billed, so a heartbeat reports the difference. */
let streamedBytes = 0;
let reportedSeconds = 0;

/** True while stdout has not drained, so interims can be dropped rather than queued. */
let backedUp = false;

/**
 * Writes one protocol message.
 *
 * Interims are dropped while stdout is backed up; nothing else ever is.
 *
 * This is not tidiness, it is the metering. A host that reads stdout slowly --
 * and one of ours documents that it does -- lets the buffer fill, and then
 * `write` blocks, and a blocked write stalls Node's event loop. Every timer
 * stops with it, including the heartbeat that reports what a church has used.
 * The audio keeps streaming and Azure keeps charging us, and nothing is
 * counted: the failure is silent and costs money in the one direction we
 * cannot notice.
 *
 * Interims are the only messages produced fast enough to cause it and the only
 * ones worth nothing once superseded, so they are what gives way.
 */
function emit(message) {
  if (backedUp && message.type === "recognizing") return;

  const room = process.stdout.write(`${JSON.stringify(message)}\n`);
  if (room || backedUp) return;

  backedUp = true;
  process.stdout.once("drain", () => {
    backedUp = false;
  });
}

function fail(message, fatal) {
  emit({ type: "error", message, fatal: Boolean(fatal) });
}

/**
 * Calls the broker.
 *
 * Never throws and never puts the device token anywhere but the header: a
 * failure here is reported as a failure to reach the subscription, not as
 * whatever the network happened to say.
 */
async function broker(path, payload) {
  try {
    const response = await fetch(`${BROKER}/api/v1/${path}`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${DEVICE_TOKEN}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    // A redirect that changed host has already cost us the Authorization
    // header -- fetch drops it by spec, and rightly so. What comes back is a
    // 401 blaming the licence, which is the one thing that is not wrong.
    if (response.redirected && new URL(response.url).host !== new URL(BROKER).host) {
      return {
        reached: true,
        ok: false,
        status: 0,
        detail: {
          message:
            "Castavox is configured with the wrong address: it redirects, and a " +
            "credential is not carried across a redirect. Update the app.",
        },
      };
    }

    const detail = await response.json().catch(() => ({}));
    return { reached: true, ok: response.ok, status: response.status, detail };
  } catch {
    return { reached: false, ok: false, status: 0, detail: {} };
  }
}

/** Whole seconds of audio sent but not yet reported. */
function unreported() {
  return Math.max(0, Math.floor(streamedBytes / BYTES_PER_SECOND) - reportedSeconds);
}

/**
 * Opens a metered session, and returns whether listening may begin.
 *
 * A refusal here is the one place a church is turned away, and it is deliberate:
 * before any audio is sent, while the message still has somewhere useful to
 * point. Once a session is open it is never cut off for quota.
 */
async function openSession() {
  const { reached, ok, detail } = await broker("session/start", {});
  if (!reached) {
    fail("Could not reach your Castavox subscription. Check the internet connection.", true);
    return false;
  }
  if (!ok) {
    // The broker writes these for the person at the desk; passing them through
    // unchanged is the point of having written them there.
    fail(detail.message || "Your Castavox subscription would not start a session.", true);
    return false;
  }

  session = {
    id: detail.sessionId,
    heartbeatSeconds: Number(detail.heartbeatSeconds) || 300,
  };
  authToken = detail.token || "";
  hostedRegion = detail.region || "";
  if (!authToken || !hostedRegion) {
    fail("Your Castavox subscription did not return a usable credential.", true);
    return false;
  }

  // Told to the parent before any audio moves, so that however this process
  // ends -- including killed, where nothing here gets to run -- there is
  // something left that knows which session to close.
  emit({ type: "session", id: session.id });
  return true;
}

/**
 * Reports what has been used and renews the credential.
 *
 * Azure's tokens last ten minutes and a sermon does not, so this is what keeps
 * a long service running. A renewal that fails is not fatal: the current token
 * has not expired yet, and tearing down mid-sentence over one bad request would
 * be a worse answer than trying again.
 */
async function heartbeat() {
  if (!session || shuttingDown) return;

  const used = unreported();
  const { reached, ok, status, detail } = await broker("session/heartbeat", {
    sessionId: session.id,
    seconds: used,
  });

  if (ok) {
    // The broker recorded it, so it must not be sent twice.
    reportedSeconds += used;
    if (detail.token) {
      authToken = detail.token;
      // Set on the live recogniser, which is what carries it into the next
      // connection without interrupting this one.
      if (recognizer) recognizer.authorizationToken = detail.token;
    }
    if (Number(detail.heartbeatSeconds) > 0) session.heartbeatSeconds = Number(detail.heartbeatSeconds);
  } else if (status === 402) {
    // Standing, not quota: the subscription itself has stopped.
    fail(detail.message || "This Castavox subscription is no longer active.", true);
    shutdown(1);
    return;
  } else if (!reached) {
    // Offline mid-service. Keep listening on the token in hand and say so, so
    // the operator knows why the counter has stopped moving.
    emit({ type: "reconnecting", message: "cannot reach the subscription; still listening" });
  }

  scheduleHeartbeat();
}

function scheduleHeartbeat() {
  if (shuttingDown || !session) return;
  clearTimeout(heartbeatTimer);
  heartbeatTimer = setTimeout(heartbeat, session.heartbeatSeconds * 1000);
  // Never hold the process open on its own account.
  heartbeatTimer.unref?.();
}

/**
 * Closes the session, reporting the last of the audio.
 *
 * Best effort. By this point Azure has already been paid for what was streamed,
 * so a failure here loses the record rather than the money — worth one attempt
 * and not worth delaying a quit for.
 */
async function closeSession() {
  if (!session) return;
  const ending = session;
  session = null;
  clearTimeout(heartbeatTimer);
  await broker("session/end", { sessionId: ending.id, seconds: unreported() });
}

function buildRecognizer() {
  const speechConfig = HOSTED
    ? sdk.SpeechConfig.fromAuthorizationToken(authToken, hostedRegion)
    : sdk.SpeechConfig.fromSubscription(KEY, REGION);
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

/**
 * Ends the run: stops listening, closes the session, and quits.
 *
 * # The two are started together, not one after the other
 *
 * There is nothing the close needs from the recogniser. stdin has already
 * ended by the time this runs, so no more audio can arrive and `unreported()`
 * will not change again -- what there is to report is final at the first line
 * of this function.
 *
 * It used to wait: stop Azure, and report on the way out of that callback.
 * Both then had to fit inside the one grace window below, and stopping a
 * recogniser is a round trip to Azure over a connection that is often the very
 * thing that has just failed. When it was slow the failsafe killed the process
 * with `session/end` still in flight; when the callback never came at all --
 * which is what a dead connection does -- the close was never even started.
 * Either way the session stayed open on the broker, counted against the
 * concurrency limit, and had to be cleared by hand.
 */
function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  clearTimeout(restartTimer);
  clearTimeout(heartbeatTimer);

  const closed = closeSession();

  const stopped = new Promise((done) => {
    if (!recognizer) return done();
    try {
      recognizer.stopContinuousRecognitionAsync(() => {
        try {
          recognizer.close();
        } catch {
          /* ignore */
        }
        done();
      }, done);
    } catch {
      done();
    }
  });

  // Settled, not successful: a close the broker refused has been reported as
  // well as it can be from here, and the parent closes it again regardless.
  Promise.allSettled([closed, stopped]).then(() => process.exit(code || 0));

  // Never hang the parent waiting on a clean close.
  setTimeout(() => process.exit(code || 0), SHUTDOWN_GRACE_MS).unref();
}

async function main() {
  if (HOSTED) {
    // Nothing is captured until the subscription has agreed to it.
    if (!(await openSession())) {
      process.exit(1);
      return;
    }
  } else if (!KEY || !REGION) {
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
      // Counted after the write, so audio the stream refused is not billed.
      streamedBytes += chunk.byteLength;
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
  if (HOSTED) scheduleHeartbeat();
}

main().catch((err) => {
  fail(`sidecar could not start: ${describe(err)}`, true);
  process.exit(1);
});
