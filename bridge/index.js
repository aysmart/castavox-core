/**
 * The streaming speech bridge, shared by Castavox and Pulpitry.
 *
 * Reads raw PCM on stdin (16 kHz, 16-bit little-endian, mono — written by the
 * Rust capture thread), streams it to a speech service, and writes
 * newline-delimited JSON transcript events to stdout.
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
 * # Two ways to authenticate, and two services
 *
 * With the church's own Azure key (CASTAVOX_SPEECH_KEY), which is how this has
 * always worked, or on a hosted subscription (CASTAVOX_BROKER_URL and
 * CASTAVOX_DEVICE_TOKEN), where the broker hands out a short-lived token
 * against a plan it meters.
 *
 * A hosted session runs on whichever service the broker names when it opens
 * the session — Azure, or Deepgram at about a third the price for the same
 * audio hour. **The choice is the broker\'s, per session, and this file simply
 * does as it is told.** That is what makes the migration reversible: if verse
 * detection turns out worse on a real Sunday it goes back by changing one
 * variable on a server, without releasing two desktop applications and without
 * leaving anybody on a worse transcript while a build runs. An older broker
 * that names nothing means Azure, which is what it was already doing.
 *
 * A church on its own key is always Azure. That account is theirs, none of
 * this reaches it, and we are not about to ask a church to open a second one.
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
// Already in the tree -- the Speech SDK's own transport -- but declared as a
// dependency of this bundle rather than borrowed from theirs, so a version of
// the SDK that stops using it cannot take our other transport with it.
const WebSocket = require("ws");

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
/**
 * How long Deepgram may hear nothing from us before we say we are still here.
 * It gives up at about ten seconds; this leaves room for a slow round trip.
 */
const KEEPALIVE_AFTER_MS = 3000;
/**
 * How long a stop waits for a handshake in progress, so that audio captured
 * while connecting is flushed rather than dropped.
 */
const CONNECT_WAIT_MS = 1500;
const AUDIO_FORMAT = sdk.AudioStreamFormat.getWaveFormatPCM(SAMPLE_RATE, 16, 1);

/** Cancellation codes that retrying will never fix. */
const FATAL_ERROR_CODES = new Set([
  sdk.CancellationErrorCode.AuthenticationFailure,
  sdk.CancellationErrorCode.Forbidden,
  sdk.CancellationErrorCode.BadRequestParameters,
]);

let pushStream = null;
let recognizer = null;
/** Whichever service this run is speaking to, once it has been built. */
let transport = null;
let restartTimer = null;
let shuttingDown = false;

/** Hosted state. All of it stays null on a church's own key. */
let session = null;
let authToken = "";
let hostedRegion = "";
/**
 * Which service this run speaks to. The broker decides it; a church on its own
 * key is Azure and never asks.
 */
let providerName = "azure";
/** Deepgram's model, named by the broker so it can change without a release. */
let deepgramModel = "nova-3";
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
  // What this build can actually speak, so the broker never hands back a
  // credential for a service this copy has never heard of.
  //
  // It is the thing that makes the provider switch safe to throw. Churches
  // update on their own schedule -- some of them a year late -- and a bridge
  // from before Deepgram existed, handed a Deepgram token and no region,
  // refuses to start at all. Saying so here means an old install keeps working
  // on Azure and a new one moves, without anybody choosing which churches find
  // out on a Sunday.
  const { reached, ok, detail } = await broker("session/start", {
    speaks: ["azure", "deepgram"],
  });
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
  // Absent means Azure. A broker older than the second service says nothing
  // here, and what it says nothing about is what it was already doing.
  providerName = detail.provider === "deepgram" ? "deepgram" : "azure";
  if (detail.model) deepgramModel = String(detail.model);

  // Azure is addressed at a regional endpoint and Deepgram at one host, so
  // what counts as a usable credential differs. Checked here, before any audio
  // moves, rather than as a connection failure thirty seconds into a sermon.
  const usable = authToken && (providerName === "deepgram" || hostedRegion);
  if (!usable) {
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
    // Repeated, for the same reason it is sent at the start: the renewed token
    // is what the next reconnection uses, and it has to be for a service this
    // copy can talk to.
    speaks: ["azure", "deepgram"],
  });

  if (ok) {
    // The broker recorded it, so it must not be sent twice.
    reportedSeconds += used;
    // A switch thrown between heartbeats reaches us here, and takes effect at
    // the next reconnection rather than interrupting the one we are on.
    if (detail.provider) providerName = detail.provider === "deepgram" ? "deepgram" : "azure";
    if (detail.model) deepgramModel = String(detail.model);
    if (detail.region) hostedRegion = detail.region;
    if (detail.token) {
      authToken = detail.token;
      // Azure's SDK carries a renewed token into its next connection without
      // interrupting this one. Deepgram authenticates the handshake and
      // nothing after it, so an established connection is unaffected by its
      // token expiring and the new one simply waits for the next reconnection.
      if (providerName === "azure" && recognizer) recognizer.authorizationToken = detail.token;
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

/**
 * Deepgram's language parameter, from the locale the operator chose.
 *
 * Exactly two values are ever sent: `en` for an English locale and `multi` --
 * nova-3's multilingual mode -- for anything else. Both are verified against
 * the live endpoint.
 *
 * # Why the region is dropped rather than passed on
 *
 * This used to send any `en-*` tag through unchanged, on the reasonable-looking
 * assumption that Deepgram takes BCP-47. It takes *some* of it. `en-US` and
 * `en-GB` are accepted; **`en-NG` is refused with a 400 at the handshake** --
 * which is the locale a Nigerian church picks, in the country most of our
 * churches are in, so the failure landed precisely on the people it could hurt
 * most and on nobody testing in English elsewhere.
 *
 * Nothing is lost by dropping it. nova-3 has one English model; the regional
 * tags are aliases for it rather than accent-specific models, so `en-GB` and
 * `en` transcribe a Nigerian speaker identically. What the narrower set buys is
 * that no locale an operator can choose produces a request Deepgram rejects --
 * and a rejected handshake is not a degraded transcript, it is no transcript.
 *
 * Azure still gets the full locale. It supports `en-NG` properly, and that path
 * is unchanged.
 */
function deepgramLanguage() {
  return /^en(-|$)/i.test(LANGUAGE) ? "en" : "multi";
}

/**
 * Transcription over Deepgram's streaming WebSocket.
 *
 * Presents the same three operations as the Azure transport below -- start,
 * write, stop -- and emits the same messages, so nothing downstream of this
 * file knows or cares which service produced a word.
 *
 * # Which results are final
 *
 * Deepgram marks a segment `is_final` when it will not revise it again, and
 * separately marks `speech_final` at an endpoint. `is_final` is what becomes a
 * "recognized" here: the detector's contract is a confirmed transcript segment,
 * those segments are confirmed and do not overlap, and waiting for the endpoint
 * would hold a finished clause back until the speaker drew breath.
 */
function buildDeepgram() {
  const query = new URLSearchParams({
    model: deepgramModel,
    language: deepgramLanguage(),
    encoding: "linear16",
    sample_rate: String(SAMPLE_RATE),
    channels: "1",
    interim_results: "true",
    punctuate: "true",
    smart_format: "true",
    // 800 ms of silence ends an utterance, the same figure the Azure side is
    // configured with and for the same reason: preaching pauses for effect,
    // and a shorter one cuts a sentence in half.
    endpointing: "800",
    // Return a result as soon as it is ready rather than waiting to batch it.
    no_delay: "true",
  });

  const socket = new WebSocket(`wss://api.deepgram.com/v1/listen?${query}`, {
    headers: {
      // A granted token, not our key. The key never leaves the broker.
      authorization: HOSTED ? `Bearer ${authToken}` : `Token ${KEY}`,
    },
    handshakeTimeout: REQUEST_TIMEOUT_MS,
  });

  /*
   * Audio that arrived before the socket finished opening.
   *
   * Capture starts the moment the operator presses Listen and the handshake
   * takes a round trip, so there is always some -- and on a reconnection there
   * is more. Azure's push stream swallows it for us; a WebSocket cannot send
   * before it is open, and a transport that quietly dropped it would lose the
   * first words of every service and the first words after every blip.
   *
   * Bounded, because a socket that never opens must not grow this forever.
   * Past the bound `write` says so, and what it refuses is not billed.
   */
  const pending = [];
  let pendingBytes = 0;
  const PENDING_LIMIT = BYTES_PER_SECOND * 5;

  socket.on("open", () => {
    emit({ type: "listening" });
    for (const chunk of pending.splice(0)) socket.send(chunk);
    pendingBytes = 0;
  });

  /*
   * A word to Deepgram when the microphone has gone quiet on us.
   *
   * Not silence -- silence is audio and is streamed like any other. This is
   * *no audio at all*, which is a stalled capture thread, a device unplugged,
   * or a machine that slept. Deepgram closes such a connection after about ten
   * seconds with a 1011, and it takes the utterance it had not yet finalised
   * with it. Seen exactly that way in testing.
   *
   * So a KeepAlive goes out when nothing has been sent for a few seconds. It
   * holds the connection without pretending there was sound: it is not audio,
   * it is not billed as audio, and it does not affect what is transcribed.
   */
  let lastAudioAt = Date.now();
  const keepAlive = setInterval(() => {
    if (socket.readyState !== WebSocket.OPEN) return;
    if (Date.now() - lastAudioAt < KEEPALIVE_AFTER_MS) return;
    try {
      socket.send(JSON.stringify({ type: "KeepAlive" }));
    } catch {
      /* the close handler deals with a socket that has gone */
    }
  }, KEEPALIVE_AFTER_MS);
  keepAlive.unref?.();
  socket.on("close", () => clearInterval(keepAlive));

  socket.on("message", (data) => {
    let payload;
    try {
      payload = JSON.parse(data.toString());
    } catch {
      // Not ours to interpret. Deepgram sends JSON; anything else is a version
      // of their protocol this does not know, and dropping it is better than
      // crashing a service over it.
      return;
    }
    if (payload.type === "Error") {
      scheduleRestart(payload.description || payload.message || "the speech service reported an error");
      return;
    }
    if (payload.type !== "Results") return;

    const alternative = payload.channel && payload.channel.alternatives && payload.channel.alternatives[0];
    const text = alternative && alternative.transcript;
    if (!text) return;

    if (!payload.is_final) {
      emit({ type: "recognizing", text });
      return;
    }
    emit({
      type: "recognized",
      text,
      // Seconds here, where Azure reports ticks. Both leave as milliseconds.
      offsetMs: Math.round(Number(payload.start || 0) * 1000),
      durationMs: Math.round(Number(payload.duration || 0) * 1000),
    });
  });

  /*
   * A refused upgrade: a bad token, or a query it will not accept.
   *
   * The body is read, and that is the whole point of this handler. It reported
   * the status alone once -- "the speech service answered 400" -- which is true
   * and tells nobody which of a dozen parameters it objected to. Deepgram says
   * exactly which; throwing that away and reconnecting to be refused again in
   * the same way is not a diagnosis, it is a loop.
   *
   * Bounded, and to stderr rather than into the operator's message: what a
   * volunteer needs is one sentence, and `endpointing must be an integer` is
   * not it.
   */
  socket.on("unexpected-response", (_request, response) => {
    const status = (response && response.statusCode) || 0;
    let body = "";
    response.on("data", (chunk) => {
      if (body.length < 500) body += chunk.toString();
    });
    response.on("end", () => {
      const detail = body.trim().slice(0, 500);
      if (detail) console.error(`[deepgram] ${status}: ${detail}`);

      // A credential problem cannot be retried into working.
      if (status === 401 || status === 403) {
        fail(`Deepgram rejected the connection (${status}).`, true);
        shutdown(1);
        return;
      }
      // Nor can a request it will never accept. Reconnecting on a 400 is a
      // loop that ends when the operator gives up, so it stops and says so.
      if (status === 400) {
        fail(
          "Deepgram would not accept the connection. This is ours to fix, not " +
            "yours — transcription on this machine still works.",
          true,
        );
        shutdown(1);
        return;
      }
      scheduleRestart(`the speech service answered ${status || "an error"}`);
    });
    response.resume();
  });

  socket.on("error", (err) => scheduleRestart(describe(err)));

  socket.on("close", (code) => {
    // 1000 after we asked to stop is the ordinary end of a run. Anything else
    // arriving while we are still listening is a connection to rebuild.
    if (shuttingDown || code === 1000) return;
    scheduleRestart(`the speech service closed the connection (${code})`);
  });

  return {
    kind: "deepgram",
    socket,
    write(chunk) {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(chunk);
        lastAudioAt = Date.now();
        return true;
      }
      // Held until the handshake finishes. Counted as sent, because it will be.
      if (socket.readyState === WebSocket.CONNECTING && pendingBytes + chunk.byteLength <= PENDING_LIMIT) {
        pending.push(Buffer.from(chunk));
        pendingBytes += chunk.byteLength;
        return true;
      }
      return false;
    },
    stop() {
      // Asks for the last of the audio to be transcribed before the socket
      // goes. Without it the closing seconds of a sermon are simply dropped.
      const flush = (done) => {
        socket.once("close", done);
        try {
          socket.send(JSON.stringify({ type: "CloseStream" }));
        } catch {
          done();
        }
      };

      return new Promise((done) => {
        if (socket.readyState === WebSocket.OPEN) return flush(done);

        /*
         * Still shaking hands. Wait for it, briefly.
         *
         * Everything captured so far is in `pending` and is sent the moment the
         * socket opens, so giving up here throws all of it away -- which is
         * what this did, and what a stop during a reconnection would have meant
         * on bad hall wifi: the operator stops, and the last thing anybody said
         * was never transcribed. Nothing announces that; it is simply missing.
         *
         * Short, because it runs inside the shutdown grace window and a
         * handshake that has not finished by now will not rescue much.
         */
        if (socket.readyState === WebSocket.CONNECTING) {
          const giveUp = setTimeout(() => {
            try {
              socket.terminate();
            } catch {
              /* already gone */
            }
            done();
          }, CONNECT_WAIT_MS);
          giveUp.unref?.();

          const settle = () => {
            clearTimeout(giveUp);
            done();
          };
          socket.once("open", () => {
            clearTimeout(giveUp);
            flush(done);
          });
          socket.once("error", settle);
          return;
        }

        try {
          socket.terminate();
        } catch {
          /* already gone */
        }
        done();
      });
    },
    close() {
      try {
        socket.terminate();
      } catch {
        /* already gone */
      }
    },
  };
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

/** The Azure recogniser, behind the same three operations as the other one. */
function buildAzure() {
  recognizer = buildRecognizer();
  const speech = recognizer;

  speech.startContinuousRecognitionAsync(
    () => {},
    (err) => scheduleRestart(describe(err)),
  );

  return {
    kind: "azure",
    write(chunk) {
      if (!pushStream) return false;
      // The SDK wants an ArrayBuffer; slice so a pooled Buffer's neighbours
      // don't get sent along with it.
      pushStream.write(chunk.buffer.slice(chunk.byteOffset, chunk.byteOffset + chunk.byteLength));
      return true;
    },
    stop() {
      return new Promise((done) => {
        try {
          speech.stopContinuousRecognitionAsync(() => {
            try {
              speech.close();
            } catch {
              /* ignore */
            }
            done();
          }, done);
        } catch {
          done();
        }
      });
    },
    close() {
      try {
        speech.close();
      } catch {
        /* already torn down */
      }
    },
  };
}

function startRecognition() {
  try {
    transport = providerName === "deepgram" ? buildDeepgram() : buildAzure();
  } catch (err) {
    fail(`Could not initialise the speech recogniser: ${describe(err)}`, true);
    shutdown(1);
    return;
  }
}

/**
 * Rebuilds the connection after a recoverable failure.
 *
 * Closing an Azure recogniser also closes its push stream, so a fresh stream is
 * created each time and stdin is re-pointed at it. A Deepgram socket has no
 * such attachment, but the shape is the same: whatever is there is dropped and
 * a new one is built.
 */
function scheduleRestart(reason) {
  if (shuttingDown || restartTimer) return;
  emit({ type: "reconnecting", message: reason });

  const dying = transport;
  transport = null;
  recognizer = null;
  pushStream = null;
  if (dying) dying.close();

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

  const stopped = transport ? transport.stop() : Promise.resolve();

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
    if (!transport || shuttingDown) return;
    try {
      // Counted only when the transport took it, so audio dropped on the floor
      // during a reconnection is not billed to a church that never heard it.
      if (transport.write(chunk)) streamedBytes += chunk.byteLength;
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
