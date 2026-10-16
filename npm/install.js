// Postinstall: fetch the prebuilt Nova binary for this package version from
// its GitHub Release and vendor it next to the launcher.
//
// The package version matches the release tag (`nova-v${VERSION}-…`). macOS
// gets the universal archive; Windows gets the zip for the current CPU.
// There is nothing to compile.
//
// Idempotent and best-effort: if it fails here (for example `npm ci
// --ignore-scripts`, or an offline install), bin/nova.js downloads on first
// launch instead, so the package still works.

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
const BASE = `https://github.com/${REPO}/releases/download/v${VERSION}`;

const VENDOR_DIR = path.join(__dirname, "bin");

// Which published v${VERSION} archive this machine should run.
// Returns null for Linux and for architectures with no release asset.
function releaseFor(platform, arch) {
  if (platform === "darwin" && (arch === "arm64" || arch === "x64")) {
    return {
      asset: `nova-v${VERSION}-universal-apple-darwin.tar.gz`,
      binaryName: "nova",
      format: "tar.gz",
    };
  }
  if (platform === "win32" && arch === "x64") {
    return {
      asset: `nova-v${VERSION}-x86_64-pc-windows-msvc.zip`,
      binaryName: "nova.exe",
      format: "zip",
    };
  }
  if (platform === "win32" && arch === "arm64") {
    return {
      asset: `nova-v${VERSION}-aarch64-pc-windows-msvc.zip`,
      binaryName: "nova.exe",
      format: "zip",
    };
  }
  return null;
}

function unsupportedMessage(platform, arch) {
  if (platform === "linux") {
    return (
      "this npm package installs the macOS or Windows desktop binary only. " +
      "Linux desktop control is not supported."
    );
  }
  if (platform === "win32") {
    return `Windows ${arch} is not a published Nova build. Use x64 or ARM64.`;
  }
  if (platform === "darwin") {
    return `macOS ${arch} is not a published Nova build. Use Apple Silicon or Intel.`;
  }
  return "Nova's npm package supports macOS and Windows only.";
}

function binaryPath(binaryName) {
  return path.join(VENDOR_DIR, binaryName);
}

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

// Extract one regular file from a gzipped tar (the release archive is
// `nova` plus LICENSE, no nested deps).
function extractFromTarGz(gzBuf, binaryName) {
  const tar = zlib.gunzipSync(gzBuf);
  for (let off = 0; off + 512 <= tar.length; ) {
    const header = tar.subarray(off, off + 512);
    if (header.every((b) => b === 0)) break; // end-of-archive
    const name = header.subarray(0, 100).toString("utf8").replace(/\0.*$/, "");
    const size = parseInt(header.subarray(124, 136).toString("utf8").replace(/\0.*$/, "").trim() || "0", 8);
    const dataStart = off + 512;
    const base = path.basename(name);
    if (base === binaryName) return tar.subarray(dataStart, dataStart + size);
    off = dataStart + Math.ceil(size / 512) * 512;
  }
  throw new Error(`\`${binaryName}\` not found inside the release tarball`);
}

function entryBase(name) {
  const normalized = name.replace(/\\/g, "/").replace(/\/+$/, "");
  const slash = normalized.lastIndexOf("/");
  return slash === -1 ? normalized : normalized.slice(slash + 1);
}

function need(buf, off, len) {
  if (off < 0 || len < 0 || off + len > buf.length) {
    throw new Error("truncated ZIP");
  }
}

// Extract one file from a ZIP (PowerShell Compress-Archive: deflate or
// store). Sizes come from the central directory so a data descriptor in
// the local header does not matter.
function extractFromZip(zipBuf, binaryName) {
  const eocdSig = 0x06054b50;
  let eocd = -1;
  const min = Math.max(0, zipBuf.length - 22 - 65535);
  for (let i = zipBuf.length - 22; i >= min; i--) {
    if (zipBuf.readUInt32LE(i) === eocdSig) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error("ZIP end-of-central-directory not found");
  need(zipBuf, eocd, 22);
  const total = zipBuf.readUInt16LE(eocd + 10);
  const cdOffset = zipBuf.readUInt32LE(eocd + 16);
  if (cdOffset === 0xffffffff) throw new Error("ZIP64 archives are not supported");
  let off = cdOffset;
  for (let n = 0; n < total; n++) {
    need(zipBuf, off, 46);
    if (zipBuf.readUInt32LE(off) !== 0x02014b50) throw new Error("bad ZIP central directory");
    const method = zipBuf.readUInt16LE(off + 10);
    const compSize = zipBuf.readUInt32LE(off + 20);
    const uncompSize = zipBuf.readUInt32LE(off + 24);
    const nameLen = zipBuf.readUInt16LE(off + 28);
    const extraLen = zipBuf.readUInt16LE(off + 30);
    const commentLen = zipBuf.readUInt16LE(off + 32);
    const localOff = zipBuf.readUInt32LE(off + 42);
    need(zipBuf, off + 46, nameLen);
    const name = zipBuf.subarray(off + 46, off + 46 + nameLen).toString("utf8");
    off += 46 + nameLen + extraLen + commentLen;
    if (entryBase(name) !== binaryName) continue;
    need(zipBuf, localOff, 30);
    if (zipBuf.readUInt32LE(localOff) !== 0x04034b50) throw new Error("bad ZIP local header");
    const localNameLen = zipBuf.readUInt16LE(localOff + 26);
    const localExtraLen = zipBuf.readUInt16LE(localOff + 28);
    const dataStart = localOff + 30 + localNameLen + localExtraLen;
    need(zipBuf, dataStart, compSize);
    const data = zipBuf.subarray(dataStart, dataStart + compSize);
    let out;
    if (method === 0) out = Buffer.from(data);
    else if (method === 8) out = zlib.inflateRawSync(data);
    else throw new Error(`unsupported ZIP compression method ${method}`);
    if (out.length !== uncompSize) {
      throw new Error(`extracted ${binaryName} size ${out.length} != ${uncompSize}`);
    }
    return out;
  }
  throw new Error(`\`${binaryName}\` not found inside the release archive`);
}

function extractBinary(archive, format, binaryName) {
  const bin = format === "tar.gz" ? extractFromTarGz(archive, binaryName) : extractFromZip(archive, binaryName);
  if (!bin || bin.length < 4) throw new Error(`extracted ${binaryName} is empty`);
  if (binaryName.endsWith(".exe")) {
    if (bin[0] !== 0x4d || bin[1] !== 0x5a) throw new Error("extracted nova.exe is not a PE executable");
  } else {
    const magic = bin.readUInt32BE(0);
    const machos = new Set([0xcafebabe, 0xbebafeca, 0xfeedface, 0xcefaedfe, 0xfeedfacf, 0xcffaedfe]);
    if (!machos.has(magic)) throw new Error("extracted nova is not a Mach-O executable");
  }
  return bin;
}

async function main() {
  const release = releaseFor(os.platform(), os.arch());
  if (!release) {
    // Don't hard-fail installs on hosts that pull the package anyway
    // (`--force`, or a CI image). The launcher explains itself.
    console.warn(`[nova] skipping binary download: ${unsupportedMessage(os.platform(), os.arch())}`);
    return;
  }
  try {
    const [archive, shaLine] = await Promise.all([
      fetch(`${BASE}/${release.asset}`),
      fetch(`${BASE}/${release.asset}.sha256`),
    ]);
    const want = shaLine.toString("utf8").trim().split(/\s+/)[0].toLowerCase();
    const got = crypto.createHash("sha256").update(archive).digest("hex");
    if (want && want !== got) {
      throw new Error(`checksum mismatch for ${release.asset}\n  expected ${want}\n  got      ${got}`);
    }
    const bin = extractBinary(archive, release.format, release.binaryName);
    const dest = binaryPath(release.binaryName);
    fs.mkdirSync(VENDOR_DIR, { recursive: true });
    fs.writeFileSync(dest, bin, { mode: 0o755 });
    // Belt-and-braces: strip any quarantine attribute (node's download won't
    // set one, but keep it bulletproof). Ignore failure. Windows has no xattr.
    if (os.platform() === "darwin") {
      try {
        execFileSync("xattr", ["-dr", "com.apple.quarantine", dest]);
      } catch {}
    }
    console.log(`[nova] installed ${dest} (v${VERSION})`);
  } catch (err) {
    console.warn(`[nova] could not pre-download the binary (${err.message}); it will be fetched on first run.`);
  }
}

if (require.main === module) {
  main();
}

module.exports = {
  releaseFor,
  unsupportedMessage,
  binaryPath,
  extractBinary,
  fetch,
};
