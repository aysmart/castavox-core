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

/** The bridge, beside a fake node_modules so it resolves the stub. */
function stage() {
  const dir = mkdtempSync(resolve(tmpdir(), "bridge-test-"));
  const module = resolve(dir, "node_modules/microsoft-cognitiveservices-speech-sdk");
  mkdirSync(module, { recursive: true });
  writeFileSync(resolve(module, "index.js"), STUB_SDK);
  writeFileSync(resolve(module, "package.json"), '{"name":"microsoft-cognitiveservices-speech-sdk","main":"index.js"}');
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
