// Postinstall: fetch the prebuilt universal Nova binary for this package
// version from its GitHub Release and vendor it next to the launcher.
//
// Nova is a macOS-only native binary (ScreenCaptureKit / Accessibility), so
// there is nothing to compile — we download the single universal-darwin
// artifact the Release workflow publishes and verify its checksum.
//
// Idempotent and best-effort: if it fails here (e.g. `npm ci --ignore-scripts`,
// offline install), bin/nova.js downloads on first launch instead, so the
// package still works.

"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const crypto = require("crypto");
const zlib = require("zlib");
const { execFileSync } = require("child_process");

const VERSION = require("./package.json").version;
const REPO = "bigduu/Nova";
const ASSET = `nova-v${VERSION}-universal-apple-darwin.tar.gz`;
const BASE = `https://github.com/${REPO}/releases/download/v${VERSION}`;

const VENDOR_DIR = path.join(__dirname, "bin");
const BIN_PATH = path.join(VENDOR_DIR, "nova");

// GET that follows GitHub's redirect to the asset CDN. Resolves to a Buffer.
function fetch(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "nova-npm-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          resolve(fetch(res.headers.location));
          return;
        }
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error(`GET ${url} → HTTP ${res.statusCode}`));
          return;
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
      })
      .on("error", reject);
  });
}

// Extract the single `nova` file from a gzipped tar (one regular file, no deps).
function extractNovaFromTarGz(gzBuf) {
  const tar = zlib.gunzipSync(gzBuf);
  for (let off = 0; off + 512 <= tar.length; ) {
    const header = tar.subarray(off, off + 512);
    if (header.every((b) => b === 0)) break; // end-of-archive
    const name = header.subarray(0, 100).toString("utf8").replace(/\0.*$/, "");
    const size = parseInt(header.subarray(124, 136).toString("utf8").replace(/\0.*$/, "").trim() || "0", 8);
    const dataStart = off + 512;
    const base = path.basename(name);
    if (base === "nova") return tar.subarray(dataStart, dataStart + size);
    off = dataStart + Math.ceil(size / 512) * 512;
  }
  throw new Error("`nova` not found inside the release tarball");
}

async function main() {
  if (os.platform() !== "darwin") {
    // Don't hard-fail installs on Linux/Windows CI that pull the dep
    // transitively; just make the launcher explain itself.
    console.warn("[nova] skipping binary download: Nova runs on macOS only.");
    return;
  }
  try {
    const [tarGz, shaLine] = await Promise.all([fetch(`${BASE}/${ASSET}`), fetch(`${BASE}/${ASSET}.sha256`)]);
    const want = shaLine.toString("utf8").trim().split(/\s+/)[0];
    const got = crypto.createHash("sha256").update(tarGz).digest("hex");
    if (want && want !== got) {
      throw new Error(`checksum mismatch for ${ASSET}\n  expected ${want}\n  got      ${got}`);
    }
    const bin = extractNovaFromTarGz(tarGz);
    fs.mkdirSync(VENDOR_DIR, { recursive: true });
    fs.writeFileSync(BIN_PATH, bin, { mode: 0o755 });
    // Belt-and-braces: strip any quarantine attribute (node's download won't
    // set one, but keep it bulletproof). Ignore failure.
    try {
      execFileSync("xattr", ["-dr", "com.apple.quarantine", BIN_PATH]);
    } catch {}
    console.log(`[nova] installed ${BIN_PATH} (v${VERSION})`);
  } catch (err) {
    console.warn(`[nova] could not pre-download the binary (${err.message}); it will be fetched on first run.`);
  }
}

main();
