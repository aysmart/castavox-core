#!/usr/bin/env node
/**
 * Copies the canonical speech bridge into the products that ship it.
 *
 *   node bridge/sync.mjs ../Pulpitry/src-sidecar/src ../Castavox/sidecar/speech-node/src
 *
 * With no arguments it looks for both products beside this repository, which is
 * how they are laid out on a development machine.
 *
 * # Why a copy rather than a dependency
 *
 * The bridge is JavaScript, and the two products consume this repository
 * through Cargo, which knows nothing about it. Publishing it to npm would work
 * and is the obvious answer, but it puts a release step between a one-line fix
 * and the machine that needs it, and it means publishing something to the
 * public registry before either product has shipped a hosted subscription.
 *
 * A copy costs nothing so long as nobody can edit it in place, which is what
 * the lock file beside it is for: each product's build hashes its copy and
 * refuses to bundle one that does not match. So the copies stay copies, and the
 * only way to change the bridge is to change it here and run this.
 */
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const source = resolve(here, "index.js");
const repo = resolve(here, "..");

/** Where the products sit when everything is checked out side by side. */
const DEFAULT_TARGETS = [
  "../Pulpitry/src-sidecar/src",
  "../Castavox/sidecar/speech-node/src",
];

/**
 * The hash of the bridge's *content*, not of its bytes on disk.
 *
 * Line endings are normalised first, and that is load-bearing rather than
 * tidy. Git for Windows converts LF to CRLF on checkout, so the same commit
 * produces a different file on a Windows runner than on a Linux one — and a
 * check that hashed the raw bytes failed every Windows build while passing
 * macOS and Linux, which reads as the bridge having been tampered with when
 * nothing had been touched at all.
 *
 * What this is for is catching a copy edited in place instead of in
 * castavox-core. A newline convention is not that.
 */
export const hashOf = (text) =>
  createHash("sha256").update(text.replace(/\r\n/g, "\n"), "utf8").digest("hex");

/** The revision being copied, so a stale product copy can be identified later. */
function revision() {
  try {
    return execFileSync("git", ["rev-parse", "--short", "HEAD"], {
      cwd: repo,
      encoding: "utf8",
    }).trim();
  } catch {
    // A tarball rather than a checkout. The hash still identifies the content,
    // which is the part that matters.
    return "unknown";
  }
}

function main() {
  const given = process.argv.slice(2);
  const targets = (given.length ? given : DEFAULT_TARGETS).map((path) =>
    resolve(given.length ? process.cwd() : repo, path),
  );

  const contents = readFileSync(source, "utf8");
  const sha256 = hashOf(contents);
  const lock = JSON.stringify(
    {
      // Everything here is advisory except the hash, which the builds enforce.
      source: "castavox-core/bridge/index.js",
      sha256,
      revision: revision(),
      syncedAt: new Date().toISOString().slice(0, 10),
      note: "Generated. Edit the canonical file in castavox-core and re-run bridge/sync.mjs.",
    },
    null,
    2,
  );

  let missing = 0;
  for (const directory of targets) {
    if (!existsSync(directory)) {
      console.error(`skipped, no such directory: ${directory}`);
      missing += 1;
      continue;
    }

    const destination = resolve(directory, "index.js");
    const before = existsSync(destination) ? readFileSync(destination, "utf8") : null;
    copyFileSync(source, destination);
    writeFileSync(resolve(directory, "bridge.lock.json"), `${lock}\n`);
    console.log(`${before === contents ? "unchanged" : "updated  "}  ${destination}`);
  }

  console.log(`\nsha256 ${sha256}`);
  if (missing) {
    // Loudly, because a half-done sync is the exact state this exists to
    // prevent: one product fixed, the other not, and nothing to say so.
    console.error(`\n${missing} target(s) missing — the products are now out of step.`);
    process.exit(1);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  main();
}
