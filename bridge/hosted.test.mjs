/**
 * The hosted session, end to end through the real bridge.
 *
 *   node --test bridge/hosted.test.mjs
 *
 * Run as a child process against a stub broker and a stub Speech SDK, because
 * the parts worth checking are the ones no unit test reaches: that a session is
 * opened before any audio moves, that a heartbeat reports the seconds actually
 * streamed and not a total that would bill them twice, that a renewed token
 * reaches the live recogniser, and that quitting closes the session.
 *
 * Every one of those is somebody's money. A bridge that reports totals rather
 * than differences would overcharge a church roughly by the square of the
 * length of the service, and would pass any test that only checked it reported
 * something.
 */
import { deepStrictEqual, match, ok, strictEqual } from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, writeFileSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { after, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/** 8 kHz keeps the arithmetic small: 16000 bytes is one second of audio. */
const RATE = 8000;
const BYTES_PER_SECOND = RATE * 2;

/**
 * A stub Speech SDK that records what the bridge asked of it.
 *
 * Enough surface to run, and nothing else. It reports to stderr, which the
 * bridge leaves alone -- stdout is the protocol.
 */
const STUB_SDK = `
"use strict";
const say = (line) => process.stderr.write(line + "\\n");

class Recognizer {
  constructor(config) {
    this._token = config.authorizationToken || "";
    say("BUILT:" + config.kind + ":" + (config.authorizationToken || config.key) + ":" + config.region);
  }
  set authorizationToken(value) { this._token = value; say("RENEWED:" + value); }
  get authorizationToken() { return this._token; }
  startContinuousRecognitionAsync(done) { setTimeout(() => done(), 5); }
  stopContinuousRecognitionAsync(done) { setTimeout(() => done(), 5); }
  close() {}
}

module.exports = {
  SpeechConfig: {
    fromSubscription: (key, region) => ({ kind: "key", key, region, setProperty() {} }),
    fromAuthorizationToken: (authorizationToken, region) =>
      ({ kind: "token", authorizationToken, region, setProperty() {} }),
  },
  AudioStreamFormat: { getWaveFormatPCM: () => ({}) },
  AudioInputStream: { createPushStream: () => ({ write() {} }) },
  AudioConfig: { fromStreamInput: () => ({}) },
  SpeechRecognizer: Recognizer,
  PropertyId: { Speech_SegmentationSilenceTimeoutMs: 1 },
  ResultReason: { RecognizedSpeech: 3 },
  CancellationReason: { Error: 1 },
  CancellationErrorCode: { AuthenticationFailure: 1, Forbidden: 2, BadRequestParameters: 3 },
};
`;

/**
 * A stub `ws` that plays Deepgram.
 *
 * Reports the URL and the Authorization header it was handed, so a test can
 * prove the granted token went where it should and our key did not. Once audio
 * arrives it answers the way Deepgram does — an interim, then a final — which
 * is what makes this a test of the transport rather than of the constructor.
 */
const STUB_WS = `
"use strict";
const { EventEmitter } = require("events");
const say = (line) => process.stderr.write(line + "\\n");

class Socket extends EventEmitter {
  constructor(url, options) {
    super();
    say("WS:" + url);
    say("AUTH:" + ((options && options.headers && options.headers.authorization) || ""));
    this.readyState = 0;
    setTimeout(() => {
      this.readyState = 1;
      this.emit("open");
    }, 5);
  }
  send(data) {
    if (typeof data === "string") {
      say("SENT:" + data);
      if (JSON.parse(data).type === "CloseStream") {
        this.readyState = 3;
        setTimeout(() => this.emit("close", 1000), 5);
      }
      return;
    }
    say("AUDIO:" + data.length);
    if (this._answered) return;
    this._answered = true;
    const results = (is_final, transcript) =>
      this.emit("message", Buffer.from(JSON.stringify({
        type: "Results",
        is_final,
        start: 1.5,
        duration: 2.25,
        channel: { alternatives: [{ transcript }] },
      })));
    setTimeout(() => results(false, "in the beginning"), 5);
    setTimeout(() => results(true, "In the beginning was the Word."), 10);
  }
  terminate() { this.readyState = 3; }
  close() { this.readyState = 3; }
}

Socket.CONNECTING = 0;
Socket.OPEN = 1;
module.exports = Socket;
`;

/**
 * The same stub, but one that never acknowledges a stop.
 *
 * What a dead connection does. Derived from the stub above rather than written
 * out again, so the two cannot drift into testing different bridges.
 */
const STUB_SDK_STOP_HANGS = STUB_SDK.replace(
  "stopContinuousRecognitionAsync(done) { setTimeout(() => done(), 5); }",
  "stopContinuousRecognitionAsync() { say('STOP:ignored'); }",
);

/** The bridge, beside a fake node_modules so it resolves the stubs. */
function stage(sdk = STUB_SDK, ws = STUB_WS) {
  const dir = mkdtempSync(resolve(tmpdir(), "bridge-test-"));
  const put = (name, source) => {
    const module = resolve(dir, "node_modules", name);
    mkdirSync(module, { recursive: true });
    writeFileSync(resolve(module, "index.js"), source);
    writeFileSync(resolve(module, "package.json"), `{"name":"${name}","main":"index.js"}`);
  };
  put("microsoft-cognitiveservices-speech-sdk", sdk);
  put("ws", ws);
  copyFileSync(resolve(here, "index.js"), resolve(dir, "index.js"));
  return dir;
}

/** A broker that records what it was asked, and answers however the test wants. */
function broker(handlers = {}) {
  const calls = [];
  const server = createServer((request, response) => {
    let raw = "";
    request.on("data", (chunk) => (raw += chunk));
    request.on("end", () => {
      const path = request.url.replace("/api/v1/", "");
      const payload = raw ? JSON.parse(raw) : {};
      calls.push({ path, payload, authorization: request.headers.authorization });

      const reply = (handlers[path] || (() => ({ status: 200, body: {} })))(payload, calls);
      response.writeHead(reply.status, { "content-type": "application/json" });
      response.end(JSON.stringify(reply.body));
    });
  });
  return { server, calls };
}

async function listen(server) {
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return `http://127.0.0.1:${server.address().port}`;
}

/** Starts the bridge, and collects its two streams. */
function start(dir, url, extra = {}) {
  const child = spawn(process.execPath, [resolve(dir, "index.js")], {
    cwd: dir,
    env: {
      ...process.env,
      CASTAVOX_BROKER_URL: url,
      CASTAVOX_DEVICE_TOKEN: "device-token-secret",
      CASTAVOX_SPEECH_SAMPLE_RATE: String(RATE),
      ...extra,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });

  const events = [];
  let stderr = "";
  let out = "";
  child.stdout.on("data", (chunk) => {
    out += chunk;
    const lines = out.split("\n");
    out = lines.pop();
    for (const line of lines) if (line.trim()) events.push(JSON.parse(line));
  });
  child.stderr.on("data", (chunk) => (stderr += chunk));

  const exited = new Promise((done) => child.on("exit", (code) => done(code)));
  return { child, events, exited, stderr: () => stderr };
}

const settle = (ms) => new Promise((done) => setTimeout(done, ms));

/** Waits for something to become true, rather than guessing how long it takes. */
async function until(condition, what, timeout = 4000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (condition()) return;
    await settle(20);
  }
  throw new Error(`timed out waiting for ${what}`);
}

const servers = [];
after(() => servers.forEach((server) => server.close()));

describe("a hosted session", () => {
  it("opens a session, meters what it streams, renews, and closes", async () => {
    const { server, calls } = broker({
      "session/start": () => ({
        status: 200,
        body: { sessionId: "sess-1", token: "azure-first", region: "uksouth", heartbeatSeconds: 1 },
      }),
      "session/heartbeat": () => ({
        status: 200,
        body: { token: "azure-renewed", region: "uksouth", heartbeatSeconds: 1 },
      }),
      "session/end": () => ({ status: 200, body: { ended: true } }),
    });
    servers.push(server);
    const url = await listen(server);
    const dir = stage();
    const run = start(dir, url);

    await until(() => run.events.some((e) => e.type === "listening") || run.stderr().includes("BUILT:"), "the recogniser");

    // The token, never our key.
    match(run.stderr(), /BUILT:token:azure-first:uksouth/);
    // And the session was opened before any audio could move.
    strictEqual(calls[0].path, "session/start");
    strictEqual(calls[0].authorization, "Bearer device-token-secret");

    // The parent is told which session this is, so that it can close it if we
    // are killed before we can. It is the first thing on stdout.
    strictEqual(run.events[0].type, "session");
    strictEqual(run.events[0].id, "sess-1");

    // Three seconds of audio.
    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND * 3));
    await until(() => calls.some((c) => c.path === "session/heartbeat"), "the first heartbeat");

    const first = calls.find((c) => c.path === "session/heartbeat");
    strictEqual(first.payload.sessionId, "sess-1");
    strictEqual(first.payload.seconds, 3);
    // The renewed token reached the running recogniser.
    await until(() => run.stderr().includes("RENEWED:azure-renewed"), "the renewal");

    // Two more seconds. The next heartbeat must report the difference: a total
    // would bill the first three all over again.
    const sent = calls.filter((c) => c.path === "session/heartbeat").length;
    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND * 2));
    await until(
      () => calls.filter((c) => c.path === "session/heartbeat").length > sent,
      "the second heartbeat",
    );
    const second = calls.filter((c) => c.path === "session/heartbeat")[sent];
    strictEqual(second.payload.seconds, 2);

    // Quitting closes the session and reports the tail.
    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND));
    await settle(50);
    run.child.kill("SIGTERM");
    strictEqual(await run.exited, 0);

    const end = calls.find((c) => c.path === "session/end");
    ok(end, "the session was closed");
    strictEqual(end.payload.sessionId, "sess-1");
    strictEqual(end.payload.seconds, 1);

    // Every second streamed, billed exactly once.
    const billed = calls
      .filter((c) => c.path === "session/heartbeat" || c.path === "session/end")
      .reduce((total, c) => total + c.payload.seconds, 0);
    strictEqual(billed, 6);
  });

  it("closes the session even when the recogniser never acknowledges the stop", async () => {
    /*
     * The bug this exists to keep fixed.
     *
     * Closing used to happen inside the stop callback, so a stop that never
     * came took the close with it, and the failsafe killed the process with
     * the session still open on the broker. It counted against the
     * concurrency limit until somebody cleared it by hand -- twice.
     *
     * A connection that has just failed is exactly when a stop goes
     * unacknowledged, and exactly when a session most wants closing.
     */
    const { server, calls } = broker({
      "session/start": () => ({
        status: 200,
        body: { sessionId: "sess-hangs", token: "azure-first", region: "uksouth", heartbeatSeconds: 60 },
      }),
      "session/end": () => ({ status: 200, body: { ended: true } }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(STUB_SDK_STOP_HANGS), url);

    // The stub never fires sessionStarted, so "listening" never arrives; what
    // says the recogniser is up is that it was built at all.
    await until(() => run.stderr().includes("BUILT:"), "the recogniser");

    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND * 2));
    await settle(50);
    run.child.kill("SIGTERM");

    // It still leaves, on the failsafe rather than on a clean stop.
    strictEqual(await run.exited, 0);
    ok(run.stderr().includes("STOP:ignored"), "the stop was asked for and ignored");

    const end = calls.find((c) => c.path === "session/end");
    ok(end, "the session was closed despite the stop never answering");
    strictEqual(end.payload.sessionId, "sess-hangs");
    // And the tail was still reported: nothing about a hung stop makes the
    // seconds unknown, because stdin had already ended.
    strictEqual(end.payload.seconds, 2);
  });

  it("refuses to listen when the subscription refuses, and says why", async () => {
    const { server, calls } = broker({
      "session/start": () => ({
        status: 402,
        body: {
          error: "speech_quota_spent",
          message: "This month's hours are used up. Add hours, or switch this machine to Whisper.",
        },
      }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(), url);

    strictEqual(await run.exited, 1);
    const reported = run.events.find((e) => e.type === "error");
    deepStrictEqual(reported, {
      type: "error",
      // Passed through unchanged: the broker wrote it for the person at the desk.
      message: "This month's hours are used up. Add hours, or switch this machine to Whisper.",
      fatal: true,
    });
    // Nothing was streamed and nothing was opened.
    strictEqual(calls.filter((c) => c.path !== "session/start").length, 0);
    ok(!run.stderr().includes("BUILT:"), "no recogniser was built");
  });

  it("keeps listening when the broker cannot be reached", async () => {
    const { server, calls } = broker({
      "session/start": () => ({
        status: 200,
        body: { sessionId: "sess-2", token: "azure-first", region: "uksouth", heartbeatSeconds: 1 },
      }),
      // A heartbeat that fails outright, as it would on a dropped connection.
      "session/heartbeat": () => ({ status: 503, body: {} }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(), url);

    await until(() => run.stderr().includes("BUILT:"), "the recogniser");
    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND * 2));

    await until(
      () => calls.filter((c) => c.path === "session/heartbeat").length >= 2,
      "two failed heartbeats",
    );
    // Still alive, still listening: a service is not stopped because a request
    // failed.
    strictEqual(run.child.exitCode, null);

    // And nothing was counted as reported, so the seconds are still owed and
    // are carried into the next attempt rather than lost.
    const beats = calls.filter((c) => c.path === "session/heartbeat");
    strictEqual(beats[0].payload.seconds, 2);
    strictEqual(beats[1].payload.seconds, 2);

    run.child.kill("SIGTERM");
    await run.exited;
  });

  it("stops when the subscription itself has stopped", async () => {
    const { server } = broker({
      "session/start": () => ({
        status: 200,
        body: { sessionId: "sess-3", token: "azure-first", region: "uksouth", heartbeatSeconds: 1 },
      }),
      "session/heartbeat": () => ({
        status: 402,
        body: { error: "subscription_lapsed", message: "This subscription has ended." },
      }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(), url);

    await until(() => run.stderr().includes("BUILT:"), "the recogniser");
    strictEqual(await run.exited, 1);
    const reported = run.events.find((e) => e.type === "error");
    strictEqual(reported.message, "This subscription has ended.");
    strictEqual(reported.fatal, true);
  });

  it("runs on Deepgram when the broker says so, and meters it the same way", async () => {
    const { server, calls } = broker({
      "session/start": () => ({
        status: 200,
        body: {
          sessionId: "sess-dg",
          provider: "deepgram",
          token: "granted-token",
          model: "nova-3",
          heartbeatSeconds: 1,
        },
      }),
      "session/heartbeat": () => ({ status: 200, body: { token: "granted-again", heartbeatSeconds: 1 } }),
      "session/end": () => ({ status: 200, body: { ended: true } }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(), url);

    await until(() => run.stderr().includes("WS:"), "the socket");

    // The granted token, as a bearer. Our own Deepgram key is never here.
    match(run.stderr(), /AUTH:Bearer granted-token/);
    // And the model came from the broker, so changing it needs no release.
    match(run.stderr(), /WS:wss:\/\/api\.deepgram\.com\/v1\/listen\?.*model=nova-3/);
    // No region: Deepgram has one host, and a session that demanded one would
    // refuse to start against a broker that rightly did not send it.
    ok(!run.stderr().includes("BUILT:"), "should not have built an Azure recogniser");

    // The session id still goes out first, so the parent can close a session
    // this process is killed before finishing.
    strictEqual(run.events[0].type, "session");
    strictEqual(run.events[0].id, "sess-dg");

    run.child.stdin.write(Buffer.alloc(BYTES_PER_SECOND * 2));

    // Deepgram's is_final becomes our recognized; anything else is an interim.
    await until(() => run.events.some((e) => e.type === "recognized"), "a final result");
    const interim = run.events.find((e) => e.type === "recognizing");
    strictEqual(interim.text, "in the beginning");
    const final = run.events.find((e) => e.type === "recognized");
    strictEqual(final.text, "In the beginning was the Word.");
    // Seconds on the wire, milliseconds out -- the same shape Azure produces
    // from ticks, because nothing downstream knows which service spoke.
    strictEqual(final.offsetMs, 1500);
    strictEqual(final.durationMs, 2250);

    // Metering is the transport's business only in that it must not change:
    // seconds streamed, reported as a difference and not a total.
    await until(() => calls.some((c) => c.path === "session/heartbeat"), "the heartbeat");
    strictEqual(calls.find((c) => c.path === "session/heartbeat").payload.seconds, 2);

    run.child.stdin.end();
    strictEqual(await run.exited, 0);

    // Asked for the last of the audio before the socket went. Without it the
    // closing seconds of a sermon are simply dropped.
    match(run.stderr(), /SENT:{"type":"CloseStream"}/);
    ok(calls.some((c) => c.path === "session/end"), "should have closed the session");
  });

  it("never sends Deepgram a locale it refuses", async () => {
    // en-NG is the one that caught us: Deepgram takes en-US and en-GB and
    // rejects en-NG with a 400 at the handshake -- the locale a Nigerian
    // church picks, in the country most of ours are in. Only "en" and "multi"
    // are ever sent now, and both are verified against the live endpoint.
    for (const locale of ["en-NG", "en-GB", "en-US", "en"]) {
      const { server } = broker({
        "session/start": () => ({
          status: 200,
          body: { sessionId: `sess-${locale}`, provider: "deepgram", token: "t", heartbeatSeconds: 60 },
        }),
        "session/end": () => ({ status: 200, body: {} }),
      });
      servers.push(server);
      const run = start(stage(), await listen(server), { CASTAVOX_SPEECH_LANGUAGE: locale });

      await until(() => run.stderr().includes("WS:"), `the socket for ${locale}`);
      match(run.stderr(), /language=en&/, `${locale} should ask for plain en`);

      run.child.kill("SIGTERM");
      await run.exited;
    }
  });

  it("asks Deepgram for its multilingual model when the service is not in English", async () => {
    const { server } = broker({
      "session/start": () => ({
        status: 200,
        body: { sessionId: "sess-yo", provider: "deepgram", token: "t", heartbeatSeconds: 60 },
      }),
      "session/end": () => ({ status: 200, body: {} }),
    });
    servers.push(server);
    const url = await listen(server);
    const run = start(stage(), url, { CASTAVOX_SPEECH_LANGUAGE: "yo-NG" });

    await until(() => run.stderr().includes("WS:"), "the socket");
    // "yo-NG" would be refused at the handshake, and a church that chose
    // Yoruba would meet a connection error rather than a transcript.
    match(run.stderr(), /language=multi/);

    run.child.kill("SIGTERM");
    await run.exited;
  });

  it("still works on a church's own key, with no broker at all", async () => {
    const run = start(stage(), "", {
      CASTAVOX_BROKER_URL: "",
      CASTAVOX_DEVICE_TOKEN: "",
      CASTAVOX_SPEECH_KEY: "church-own-key",
      CASTAVOX_SPEECH_REGION: "westeurope",
    });

    await until(() => run.stderr().includes("BUILT:"), "the recogniser");
    match(run.stderr(), /BUILT:key:church-own-key:westeurope/);

    run.child.kill("SIGTERM");
    strictEqual(await run.exited, 0);
  });
});
